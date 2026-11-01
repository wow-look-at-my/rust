// dgstat measures rustc incremental dep-graph.bin and query-cache.bin files:
// the byte composition of each, and the size of denser dep-graph layouts.
package main

import (
	"bufio"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"math/bits"
	"os"
	"path/filepath"
	"runtime/debug"
	"sort"
	"strings"
	"sync/atomic"
	"time"
)

func leb(b []byte, pos int) (uint64, int, error) {
	var v uint64
	var shift uint
	for {
		if pos >= len(b) {
			return 0, pos, errors.New("leb past end")
		}
		c := b[pos]
		pos++
		if shift < 64 {
			v |= uint64(c&0x7f) << shift
		}
		if c&0x80 == 0 {
			return v, pos, nil
		}
		shift += 7
		if shift > 126 {
			return 0, pos, errors.New("leb too long")
		}
	}
}

func putLeb(w io.ByteWriter, v uint64) {
	for v >= 0x80 {
		w.WriteByte(byte(v) | 0x80)
		v >>= 7
	}
	w.WriteByte(byte(v))
}

func lebLen(v uint64) int {
	n := 1
	for v >= 0x80 {
		v >>= 7
		n++
	}
	return n
}

func zigzag(d int64) uint64 { return uint64((d << 1) ^ (d >> 63)) }

func log2Bucket(v uint64) int { return bits.Len64(v) }

func pct(a, b int) float64 {
	if b == 0 {
		return 0
	}
	return 100 * float64(a) / float64(b)
}

func label(f string) string {
	return filepath.Base(filepath.Dir(filepath.Dir(f))) + "/" + filepath.Base(f)
}

type kindNamer struct {
	names []string
	n     int
}

func (k kindNamer) name(kind uint16) string {
	if int(kind) < len(k.names) && len(k.names) == k.n {
		return k.names[kind]
	}
	return fmt.Sprintf("kind#%d", kind)
}

type kindStat struct {
	kind                         uint16
	count, bytes, edges, zeroVal int
}

func sortedKinds(m map[uint16]*kindStat, by func(*kindStat) int) []*kindStat {
	list := make([]*kindStat, 0, len(m))
	for _, k := range m {
		list = append(list, k)
	}
	// A tie goes to the lower kind, so the order does not depend on the map.
	sort.Slice(list, func(i, j int) bool {
		a, b := by(list[i]), by(list[j])
		return a > b || a == b && list[i].kind < list[j].kind
	})
	return list
}

type qcEntry struct {
	index uint32
	pos   uint64
	side  bool
}

// analyzeQueryCache splits query-cache.bin into its regions using the footer,
// and charges each query value to the dep kind of its node in g.
func analyzeQueryCache(out io.Writer, names []string, f string, b []byte, g *cgraph) error {
	if len(b) < 8+8 {
		return errors.New("query cache too short")
	}
	start := 7 + int(b[6])
	footerPos := binary.LittleEndian.Uint64(b[len(b)-8:])
	if footerPos >= uint64(len(b)) {
		return errors.New("bad footer pos")
	}
	p := int(footerPos)
	var err error
	rd := func() uint64 {
		var v uint64
		if err == nil {
			v, p, err = leb(b, p)
		}
		return v
	}
	skip := func(n int) {
		if err == nil {
			p += n
			if p > len(b) {
				err = errors.New("past end")
			}
		}
	}
	rd() // TAG_FILE_FOOTER
	nf := rd()
	for i := uint64(0); i < nf && err == nil; i++ {
		rd()
		skip(24)
	}
	var entries []qcEntry
	for _, side := range []bool{false, true} {
		cnt := rd()
		for i := uint64(0); i < cnt && err == nil; i++ {
			idx := rd()
			pos := rd()
			entries = append(entries, qcEntry{uint32(idx), pos, side})
		}
	}
	nAlloc := rd()
	var allocs []uint64
	for i := uint64(0); i < nAlloc && err == nil; i++ {
		allocs = append(allocs, rd())
	}
	nSc := rd()
	var hyg []uint64
	for i := uint64(0); i < nSc && err == nil; i++ {
		rd()
		hyg = append(hyg, rd())
	}
	nEx := rd()
	for i := uint64(0); i < nEx && err == nil; i++ {
		skip(16)
		hyg = append(hyg, rd())
	}
	nFx := rd()
	for i := uint64(0); i < nFx && err == nil; i++ {
		skip(16)
		rd()
	}
	rd() // encode_tagged length
	if err != nil {
		return err
	}
	if p != len(b)-8 {
		return fmt.Errorf("footer parse ended at %d, want %d", p, len(b)-8)
	}
	// Every boundary in the data region, sorted.
	type mark struct {
		pos uint64
		cat int
		e   int
	}
	const (
		catValue = iota
		catSide
		catAlloc
		catHygiene
	)
	var marks []mark
	for i, e := range entries {
		c := catValue
		if e.side {
			c = catSide
		}
		marks = append(marks, mark{e.pos, c, i})
	}
	for _, a := range allocs {
		marks = append(marks, mark{a, catAlloc, -1})
	}
	for _, h := range hyg {
		marks = append(marks, mark{h, catHygiene, -1})
	}
	sort.Slice(marks, func(i, j int) bool { return marks[i].pos < marks[j].pos })
	cat := [4]int{}
	kinds := map[uint16]*kindStat{}
	unknown := 0
	for i, m := range marks {
		endPos := footerPos
		if i+1 < len(marks) {
			endPos = marks[i+1].pos
		}
		sz := int(endPos - m.pos)
		cat[m.cat] += sz
		if m.cat == catValue {
			e := entries[m.e]
			if v, _, lerr := leb(b, int(e.pos)); lerr != nil || v != uint64(e.index) {
				return fmt.Errorf("entry at %d does not start with its index %d", e.pos, e.index)
			}
			np := g.position(e.index)
			if np < 0 {
				unknown += sz
				continue
			}
			kind := g.kind(np)
			k := kinds[kind]
			if k == nil {
				k = &kindStat{kind: kind}
				kinds[kind] = k
			}
			k.count++
			k.bytes += sz
		}
	}
	first := int(footerPos)
	if len(marks) > 0 {
		first = int(marks[0].pos)
	}
	fmt.Fprintf(out, "\n=== %s  %d bytes\n", label(f), len(b))
	fmt.Fprintf(out, "regions: header %d | gap before first entry %d | query values %d (%.1f%%, %d entries) | side effects %d (%.1f%%) | const allocs %d (%.1f%%, %d) | hygiene %d (%.1f%%, %d) | footer %d (%.1f%%)\n",
		start, first-start, cat[catValue], pct(cat[catValue], len(b)), len(entries), cat[catSide], pct(cat[catSide], len(b)),
		cat[catAlloc], pct(cat[catAlloc], len(b)), len(allocs), cat[catHygiene], pct(cat[catHygiene], len(b)), len(hyg),
		len(b)-int(footerPos), pct(len(b)-int(footerPos), len(b)))
	if unknown > 0 {
		fmt.Fprintf(out, "values whose index is not in the same session's dep graph: %d B\n", unknown)
	}
	kn := kindNamer{names, g.numKinds}
	fmt.Fprintf(out, "top 20 query kinds by cached bytes:\n")
	for _, k := range sortedKinds(kinds, func(k *kindStat) int { return k.bytes })[:min(20, len(kinds))] {
		fmt.Fprintf(out, "  %-44s %9d values | %11d B %5.1f%% | %7.1f B/value\n",
			kn.name(k.kind), k.count, k.bytes, pct(k.bytes, len(b)), float64(k.bytes)/float64(k.count))
	}
	return nil
}

func readNames(path string) ([]string, error) {
	fh, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer fh.Close()
	var names []string
	sc := bufio.NewScanner(fh)
	for sc.Scan() {
		if s := strings.TrimSpace(sc.Text()); s != "" {
			names = append(names, s)
		}
	}
	return names, sc.Err()
}

func analyzeSession(out io.Writer, names []string, dg string, workers int) error {
	m, data, err := mapFile(dg)
	if err != nil {
		return err
	}
	defer m.Unmap()
	t0, c0 := time.Now(), cpuTime()
	var peak atomic.Int64
	stop := make(chan struct{})
	done := make(chan struct{})
	go func() { anonPeak(stop, &peak); close(done) }()
	defer func() {
		close(stop)
		<-done
		wall, cpu := time.Since(t0), cpuTime()-c0
		fmt.Fprintf(out, "resources %s: wall %.1fs, cpu %.1fs, peak RssAnon %d MB\n", label(dg), wall.Seconds(), cpu.Seconds(), peak.Load()/1024)
	}()
	g, err := parseCompact(data)
	if err != nil {
		return fmt.Errorf("%s: %w", dg, err)
	}
	if err := analyzeGraph(out, names, dg, g, workers); err != nil {
		return err
	}
	qc := filepath.Join(filepath.Dir(dg), "query-cache.bin")
	if _, err := os.Stat(qc); errors.Is(err, os.ErrNotExist) {
		fmt.Fprintf(out, "(no query-cache.bin)\n")
		return nil
	}
	qm, qdata, err := mapFile(qc)
	if err != nil {
		return err
	}
	defer qm.Unmap()
	return analyzeQueryCache(out, names, qc, qdata, g)
}

var endMagic = []byte("rust-end-file")

func run(out io.Writer, args []string, workers int) error {
	if len(args) < 2 {
		return errors.New("usage: dgstat KINDS.txt DEP-GRAPH.bin...")
	}
	if workers < 1 {
		return fmt.Errorf("--workers must be at least 1, got %d", workers)
	}
	names, err := readNames(args[0])
	if err != nil {
		return err
	}
	// The analysis holds a few arrays of one entry per node. The limit keeps garbage from doubling that.
	debug.SetMemoryLimit(800 << 20)
	for _, f := range args[1:] {
		if err := analyzeSession(out, names, f, workers); err != nil {
			return err
		}
	}
	return nil
}
