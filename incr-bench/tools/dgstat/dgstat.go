// dgstat measures rustc incremental dep-graph.bin and query-cache.bin files:
// the byte composition of each, and the size of denser dep-graph layouts.
package main

import (
	"bufio"
	"bytes"
	"crypto/sha256"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"math/bits"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"github.com/klauspost/compress/zstd"
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

func putLeb(w *bytes.Buffer, v uint64) {
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

type node struct {
	kind  uint16
	head  uint16
	index uint32
	key   [16]byte
	val   [16]byte
	spill int
	edges []uint32
}

type graph struct {
	start, kindBits int
	nodeMax         uint64
	nodes           []node
	edgeBytes       int
	spillBytes      int
	tail            []byte
	numKinds        int
}

// parse decodes the node stream with the given number of kind bits. It
// returns an error when the bytes do not fit that layout.
func parse(b []byte, kindBits int) (*graph, error) {
	if len(b) < 8 || len(b) < 7+int(b[6])+8+24 {
		return nil, errors.New("too short")
	}
	// The file header is followed by the 8-byte dep_tracking_hash of the options.
	start := 7 + int(b[6]) + 8
	tail := b[len(b)-24:]
	nodeMax := binary.LittleEndian.Uint64(tail[0:])
	nodeCount := binary.LittleEndian.Uint64(tail[8:])
	edgeCount := binary.LittleEndian.Uint64(tail[16:])
	end := len(b) - 24
	if nodeCount > uint64(end) || nodeMax > uint64(end) {
		return nil, errors.New("bad counts")
	}
	g := &graph{start: start, kindBits: kindBits, nodeMax: nodeMax}
	g.nodes = make([]node, 0, nodeCount)
	seen := make([]bool, nodeMax)
	kmask := uint16(1<<kindBits - 1)
	pos := start
	var edges uint64
	for i := uint64(0); i < nodeCount; i++ {
		if pos+38 > end {
			return nil, errors.New("node past end")
		}
		h := b[pos : pos+38]
		pos += 38
		var n node
		n.head = binary.LittleEndian.Uint16(h)
		n.kind = n.head & kmask
		if n.kind == 0 {
			return nil, errors.New("null kind")
		}
		width := int((n.head>>kindBits)&3) + 1
		l := uint64(n.head) >> (kindBits + 2)
		if l == 0 {
			v, np, err := leb(b, pos)
			if err != nil {
				return nil, err
			}
			n.spill = np - pos
			g.spillBytes += n.spill
			pos = np
			l = v
		} else {
			l--
		}
		n.index = binary.LittleEndian.Uint32(h[2:])
		if uint64(n.index) >= nodeMax || seen[n.index] {
			return nil, errors.New("bad index")
		}
		seen[n.index] = true
		copy(n.key[:], h[6:22])
		copy(n.val[:], h[22:38])
		eb := int(l) * width
		if pos+eb > end {
			return nil, errors.New("edges past end")
		}
		n.edges = make([]uint32, l)
		for j := range n.edges {
			var t [4]byte
			copy(t[:], b[pos+j*width:pos+(j+1)*width])
			n.edges[j] = binary.LittleEndian.Uint32(t[:])
		}
		g.edgeBytes += eb
		pos += eb
		edges += l
		g.nodes = append(g.nodes, n)
	}
	if edges != edgeCount {
		return nil, errors.New("edge count mismatch")
	}
	// The kind counts and the session count follow the nodes as leb128.
	var nums []uint64
	for p := pos; p < end; {
		v, np, err := leb(b, p)
		if err != nil {
			return nil, err
		}
		nums = append(nums, v)
		p = np
	}
	if len(nums) < 2 {
		return nil, errors.New("no kind table")
	}
	var sum uint64
	for _, v := range nums[:len(nums)-1] {
		sum += v
	}
	if sum != nodeCount {
		return nil, errors.New("kind count mismatch")
	}
	g.numKinds = len(nums) - 1
	g.tail = b[pos:]
	return g, nil
}

func parseGraph(b []byte) (*graph, error) {
	var errs []error
	for kb := 7; kb <= 12; kb++ {
		g, err := parse(b, kb)
		if err == nil {
			return g, nil
		}
		errs = append(errs, fmt.Errorf("kind bits %d: %w", kb, err))
	}
	return nil, errors.Join(errs...)
}

// Each fingerprint inline on first use and as a back-reference id on
// repeat, and edges as zigzag deltas from the target. The first edge is
// relative to the source index.
func (g *graph) dense(file []byte) []byte {
	var w bytes.Buffer
	w.Write(file[:g.start])
	keys := map[[16]byte]uint64{}
	vals := map[[16]byte]uint64{}
	prev := uint32(1<<32 - 1)
	for _, n := range g.nodes {
		var flags byte
		if n.index == prev+1 {
			flags |= 1
		}
		kid, kok := keys[n.key]
		if kok {
			flags |= 2
		} else {
			keys[n.key] = uint64(len(keys))
		}
		vid, vok := vals[n.val]
		if vok {
			flags |= 4
		} else {
			vals[n.val] = uint64(len(vals))
		}
		w.WriteByte(flags)
		var h [2]byte
		binary.LittleEndian.PutUint16(h[:], n.head)
		w.Write(h[:])
		if flags&1 == 0 {
			putLeb(&w, uint64(n.index))
		}
		if kok {
			putLeb(&w, kid)
		} else {
			w.Write(n.key[:])
		}
		if vok {
			putLeb(&w, vid)
		} else {
			w.Write(n.val[:])
		}
		if n.spill > 0 {
			putLeb(&w, uint64(len(n.edges)))
		}
		p := int64(n.index)
		for _, e := range n.edges {
			putLeb(&w, zigzag(int64(e)-p))
			p = int64(e)
		}
		prev = n.index
	}
	w.Write(g.tail)
	return w.Bytes()
}

func zstdSize(data []byte, level zstd.EncoderLevel) int {
	enc, _ := zstd.NewWriter(nil, zstd.WithEncoderLevel(level), zstd.WithEncoderConcurrency(1))
	return len(enc.EncodeAll(data, nil))
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
	sort.Slice(list, func(i, j int) bool { return by(list[i]) > by(list[j]) })
	return list
}

func analyzeGraph(out io.Writer, names []string, f string, data []byte, g *graph) {
	kn := kindNamer{names, g.numKinds}
	nn := len(g.nodes)
	mapped := "mapped"
	if len(names) != g.numKinds {
		mapped = "UNMAPPED (kind count differs)"
	}
	fmt.Fprintf(out, "\n=== %s  %d bytes, kind bits %d, %d kinds, names %s\n", label(f), len(data), g.kindBits, g.numKinds, mapped)

	ec := make([]int, nn)
	totalEdges, noEdge := 0, 0
	for i, n := range g.nodes {
		ec[i] = len(n.edges)
		totalEdges += len(n.edges)
		if len(n.edges) == 0 {
			noEdge++
		}
	}
	sort.Ints(ec)
	fmt.Fprintf(out, "nodes %d (index space %d), edges %d; edges/node avg %.2f median %d p90 %d p99 %d max %d; nodes with 0 edges %.1f%%\n",
		nn, g.nodeMax, totalEdges, float64(totalEdges)/float64(nn), ec[nn/2], ec[nn*90/100], ec[nn*99/100], ec[nn-1], pct(noEdge, nn))

	hdr := 38 * nn
	fmt.Fprintf(out, "bytes: file header %d | node headers %d (%.1f%%: head 2B %.1f%%, index 4B %.1f%%, key fp %.1f%%, value fp %.1f%%) | edge lists %d (%.1f%%) | spilled lengths %d (%.2f%%) | trailer %d\n",
		g.start, hdr, pct(hdr, len(data)), pct(2*nn, len(data)), pct(4*nn, len(data)), pct(16*nn, len(data)), pct(16*nn, len(data)),
		g.edgeBytes, pct(g.edgeBytes, len(data)), g.spillBytes, pct(g.spillBytes, len(data)), len(g.tail))

	keys := map[[16]byte]int{}
	vals := map[[16]byte]int{}
	lists := map[[32]byte]int{}
	var zero [16]byte
	zeroVal, zeroKey, dupListBytes, dupLists := 0, 0, 0, 0
	for _, n := range g.nodes {
		keys[n.key]++
		vals[n.val]++
		if n.val == zero {
			zeroVal++
		}
		if n.key == zero {
			zeroKey++
		}
		if len(n.edges) > 1 {
			var buf bytes.Buffer
			for _, e := range n.edges {
				binary.Write(&buf, binary.LittleEndian, e)
			}
			h := sha256.Sum256(buf.Bytes())
			if lists[h] > 0 {
				dupLists++
				dupListBytes += (int((n.head>>g.kindBits)&3) + 1) * len(n.edges)
			}
			lists[h]++
		}
	}
	maxVal := 0
	for _, c := range vals {
		maxVal = max(maxVal, c)
	}
	keyRepeat := nn - len(keys)
	valRepeat := nn - len(vals)
	fmt.Fprintf(out, "key fp: distinct %d = %.1f%% of nodes (repeats %d, %.1f%% of file); zero key %.1f%%\n",
		len(keys), pct(len(keys), nn), keyRepeat, pct(16*keyRepeat, len(data)), pct(zeroKey, nn))
	fmt.Fprintf(out, "value fp: distinct %d = %.1f%% of nodes (repeats %.1f%% of file); ZERO %.1f%%; most common value %.1f%%\n",
		len(vals), pct(len(vals), nn), pct(16*valRepeat, len(data)), pct(zeroVal, nn), pct(maxVal, nn))
	fmt.Fprintf(out, "edge lists with >1 edge that exactly repeat an earlier node's list: %d nodes, %d B (%.1f%% of file)\n",
		dupLists, dupListBytes, pct(dupListBytes, len(data)))

	seq := 0
	prev := uint32(1<<32 - 1)
	for _, n := range g.nodes {
		if n.index == prev+1 {
			seq++
		}
		prev = n.index
	}
	fmt.Fprintf(out, "index: %.2f%% of nodes have index = previous+1 in stream order; %.2f%% of the index space holds a node\n",
		pct(seq, nn), pct(nn, int(g.nodeMax)))

	kinds := map[uint16]*kindStat{}
	for _, n := range g.nodes {
		k := kinds[n.kind]
		if k == nil {
			k = &kindStat{kind: n.kind}
			kinds[n.kind] = k
		}
		k.count++
		k.bytes += 38 + n.spill + (int((n.head>>g.kindBits)&3)+1)*len(n.edges)
		k.edges += len(n.edges)
		if n.val == zero {
			k.zeroVal++
		}
	}
	row := func(k *kindStat) {
		fmt.Fprintf(out, "  %-44s %9d nodes %5.1f%% | %11d B %5.1f%% | %7.1f edges/node | value ZERO %5.1f%%\n",
			kn.name(k.kind), k.count, pct(k.count, nn), k.bytes, pct(k.bytes, len(data)), float64(k.edges)/float64(k.count), pct(k.zeroVal, k.count))
	}
	fmt.Fprintf(out, "top 20 kinds by node count:\n")
	for _, k := range sortedKinds(kinds, func(k *kindStat) int { return k.count })[:min(20, len(kinds))] {
		row(k)
	}
	fmt.Fprintf(out, "top 20 kinds by bytes:\n")
	for _, k := range sortedKinds(kinds, func(k *kindStat) int { return k.bytes })[:min(20, len(kinds))] {
		row(k)
	}

	var srcB, prevB [34]int
	zz := 0
	for _, n := range g.nodes {
		p := int64(n.index)
		for _, e := range n.edges {
			d := int64(e) - int64(n.index)
			srcB[log2Bucket(uint64(max(d, -d)))]++
			dp := int64(e) - p
			prevB[log2Bucket(uint64(max(dp, -dp)))]++
			zz += lebLen(zigzag(dp))
			p = int64(e)
		}
	}
	fmt.Fprintf(out, "edge |delta| log2 buckets, %% of edges (vs source | vs previous edge, first edge vs source):\n")
	for i := range srcB {
		if srcB[i] == 0 && prevB[i] == 0 {
			continue
		}
		lo := uint64(0)
		if i > 0 {
			lo = 1 << (i - 1)
		}
		fmt.Fprintf(out, "  [%d,%d] %5.1f | %5.1f\n", lo, uint64(1)<<i-1, pct(srcB[i], totalEdges), pct(prevB[i], totalEdges))
	}
	fmt.Fprintf(out, "edge bytes: now %d (%.2f B/edge); zigzag-varint delta-vs-previous %d (%.2f B/edge, %.2fx)\n",
		g.edgeBytes, float64(g.edgeBytes)/float64(totalEdges), zz, float64(zz)/float64(totalEdges), float64(g.edgeBytes)/float64(zz))

	idxCost := 0
	prev = uint32(1<<32 - 1)
	for _, n := range g.nodes {
		if n.index != prev+1 {
			idxCost += lebLen(uint64(n.index))
		}
		prev = n.index
	}
	a := g.start + nn*(2+4+4) + (nn+7)/8 + idxCost + 16*(len(keys)+len(vals)) + g.spillBytes + zz + len(g.tail)
	fmt.Fprintf(out, "layout A (key/value tables + u32 refs, index implied by order, delta edges): est %d B = %.2fx smaller\n",
		a, float64(len(data))/float64(a))
	d := g.dense(data)
	n := float64(len(data))
	d1 := zstdSize(d, zstd.SpeedFastest)
	d3 := zstdSize(d, zstd.SpeedDefault)
	p1 := zstdSize(data, zstd.SpeedFastest)
	p3 := zstdSize(data, zstd.SpeedDefault)
	fmt.Fprintf(out, "layout B (built: flag byte, fp inline on first use else leb back-ref, index implied, delta edges): %d B = %.2fx; +zstd1 %.2fx; +zstd3 %.2fx\n",
		len(d), n/float64(len(d)), n/float64(d1), n/float64(d3))
	fmt.Fprintf(out, "plain file: zstd1 %.2fx; zstd3 %.2fx\n", n/float64(p1), n/float64(p3))
}

type qcEntry struct {
	index uint32
	pos   uint64
	side  bool
}

// analyzeQueryCache splits query-cache.bin into its regions using the footer,
// and charges each query value to the dep kind of its node in g.
func analyzeQueryCache(out io.Writer, names []string, f string, b []byte, g *graph) error {
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
	byIndex := map[uint32]uint16{}
	for _, n := range g.nodes {
		byIndex[n.index] = n.kind
	}
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
			kind, ok := byIndex[e.index]
			if !ok {
				unknown += sz
				continue
			}
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

func analyzeSession(out io.Writer, names []string, dg string) error {
	m, data, err := mapFile(dg)
	if err != nil {
		return err
	}
	defer m.Unmap()
	g, err := parseGraph(data)
	if err != nil {
		return fmt.Errorf("%s: %w", dg, err)
	}
	analyzeGraph(out, names, dg, data, g)
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

func run(out io.Writer, args []string) error {
	if len(args) < 2 {
		return errors.New("usage: dgstat KINDS.txt DEP-GRAPH.bin...")
	}
	names, err := readNames(args[0])
	if err != nil {
		return err
	}
	for _, f := range args[1:] {
		if err := analyzeSession(out, names, f); err != nil {
			return err
		}
	}
	return nil
}
