// histCap bounds the dense part of the edge-count histogram.

package main

import (
	"bufio"
	"crypto/sha256"
	"encoding/binary"
	"fmt"
	"io"
	"slices"
	"sync"

	"github.com/klauspost/compress/zstd"
)

// histCap bounds the dense part of the edge-count histogram. Larger counts go to a list.
const histCap = 1 << 12

// graphStats is what the full analysis measures over one dep graph.
type graphStats struct {
	nodes, edges, noEdge          int
	ecHist                        []int
	ecBig                         []int
	edgeBytes, spillBytes         int
	zeroKey, zeroVal              int
	seq, idxCost, zz              int
	srcB, prevB                   [34]int
	kinds                         []kindStat
	distinctKeys, distinctVals    int
	maxVal, dupLists, dupListByte int
}

func (s *graphStats) add(o *graphStats) {
	s.nodes += o.nodes
	s.edges += o.edges
	s.noEdge += o.noEdge
	for i, c := range o.ecHist {
		s.ecHist[i] += c
	}
	s.ecBig = append(s.ecBig, o.ecBig...)
	s.edgeBytes += o.edgeBytes
	s.spillBytes += o.spillBytes
	s.zeroKey += o.zeroKey
	s.zeroVal += o.zeroVal
	s.seq += o.seq
	s.idxCost += o.idxCost
	s.zz += o.zz
	for i := range s.srcB {
		s.srcB[i] += o.srcB[i]
		s.prevB[i] += o.prevB[i]
	}
	for i, k := range o.kinds {
		d := &s.kinds[i]
		d.count += k.count
		d.bytes += k.bytes
		d.edges += k.edges
		d.zeroVal += k.zeroVal
	}
}

// edgeCountAt returns the edge count at rank r of all nodes sorted by edge count.
func (s *graphStats) edgeCountAt(r int) int {
	for v, c := range s.ecHist {
		if r < c {
			return v
		}
		r -= c
	}
	return s.ecBig[r]
}

func newStats(numKindSlots int) *graphStats {
	s := &graphStats{ecHist: make([]int, histCap), kinds: make([]kindStat, numKindSlots)}
	for i := range s.kinds {
		s.kinds[i].kind = uint16(i)
	}
	return s
}

func isZero(b []byte) bool {
	for _, c := range b[:16] {
		if c != 0 {
			return false
		}
	}
	return true
}

// scanNodes adds up the per-node measures of positions lo to hi.
func scanNodes(g *cgraph, lo, hi int, s *graphStats) {
	prev := uint32(1<<32 - 1)
	if lo > 0 {
		prev = g.index(lo - 1)
	}
	for p := lo; p < hi; p++ {
		off := int(g.off[p])
		e, o := g.edgesAt(p)
		ix := g.index(p)
		s.nodes++
		s.edges += e.n
		if e.n == 0 {
			s.noEdge++
		}
		if e.n < histCap {
			s.ecHist[e.n]++
		} else {
			s.ecBig = append(s.ecBig, e.n)
		}
		s.edgeBytes += len(e.raw)
		s.spillBytes += o - off - 38
		zv := isZero(g.b[off+22:])
		if zv {
			s.zeroVal++
		}
		if isZero(g.b[off+6:]) {
			s.zeroKey++
		}
		k := &s.kinds[g.kind(p)]
		k.count++
		k.bytes += o - off + len(e.raw)
		k.edges += e.n
		if zv {
			k.zeroVal++
		}
		if ix == prev+1 {
			s.seq++
		} else {
			s.idxCost += lebLen(uint64(ix))
		}
		prev = ix
		pe := int64(ix)
		for j := range e.n {
			t := int64(e.at(j))
			d := t - int64(ix)
			s.srcB[log2Bucket(uint64(max(d, -d)))]++
			dp := t - pe
			s.prevB[log2Bucket(uint64(max(dp, -dp)))]++
			s.zz += lebLen(zigzag(dp))
			pe = t
		}
	}
}

// parallel splits the positions of g into one range per worker and calls f on each range.
func parallel(n, workers int, f func(w, lo, hi int)) {
	workers = max(1, min(workers, n))
	var wg sync.WaitGroup
	for w := range workers {
		wg.Go(func() { f(w, n*w/workers, n*(w+1)/workers) })
	}
	wg.Wait()
}

// fpStats sorts the fingerprint at byte offset at of each node into r, and numbers them into ids.
func fpStats(g *cgraph, at int, r []fpRec, ids []uint32, workers int) (distinct, largest int) {
	parallel(g.nodes(), workers, func(_, lo, hi int) {
		for p := lo; p < hi; p++ {
			a, b := fpAt(g.b[int(g.off[p])+at:])
			r[p] = fpRec{a, b, uint32(p)}
		}
	})
	sortRecs(r, workers)
	return fpIDs(r, ids)
}

// listStats finds the nodes with more than one edge whose list equals the list of an earlier node.
// It hashes the decoded targets, so lists of a different width but equal targets are equal.
func listStats(g *cgraph, r []fpRec, workers int) (dups, dupBytes int) {
	n := g.nodes()
	workers = max(1, min(workers, n))
	counts := make([]int, workers+1)
	parallel(n, workers, func(w, lo, hi int) {
		for p := lo; p < hi; p++ {
			if g.edges(p).n > 1 {
				counts[w+1]++
			}
		}
	})
	for w := range workers {
		counts[w+1] += counts[w]
	}
	parallel(n, workers, func(w, lo, hi int) {
		var buf []byte
		i := counts[w]
		for p := lo; p < hi; p++ {
			e := g.edges(p)
			if e.n <= 1 {
				continue
			}
			buf = buf[:0]
			for j := range e.n {
				buf = binary.LittleEndian.AppendUint32(buf, e.at(j))
			}
			h := sha256.Sum256(buf)
			a, b := fpAt(h[:])
			r[i] = fpRec{a, b, uint32(p)}
			i++
		}
	})
	r = r[:counts[workers]]
	sortRecs(r, workers)
	groups(r, func(run []fpRec) {
		for _, x := range run[1:] {
			e := g.edges(int(x.pos))
			dups++
			dupBytes += e.width * e.n
		}
	})
	return dups, dupBytes
}

// writeDense writes layout B. Each fingerprint is inline on first use and a back-reference id
// on repeat. Edges are zigzag deltas, the first one from the source index.
func writeDense(w *bufio.Writer, g *cgraph, keyID, valID []uint32) error {
	w.Write(g.b[:g.start])
	prev := uint32(1<<32 - 1)
	for p := range g.nodes() {
		off := int(g.off[p])
		ix := g.index(p)
		kid, vid := keyID[p], valID[p]
		var flags byte
		if ix == prev+1 {
			flags |= 1
		}
		if kid&firstUse == 0 {
			flags |= 2
		}
		if vid&firstUse == 0 {
			flags |= 4
		}
		w.WriteByte(flags)
		w.Write(g.b[off : off+2])
		if flags&1 == 0 {
			putLeb(w, uint64(ix))
		}
		if flags&2 != 0 {
			putLeb(w, uint64(kid))
		} else {
			w.Write(g.b[off+6 : off+22])
		}
		if flags&4 != 0 {
			putLeb(w, uint64(vid))
		} else {
			w.Write(g.b[off+22 : off+38])
		}
		e, o := g.edgesAt(p)
		if o > off+38 {
			putLeb(w, uint64(e.n))
		}
		pe := int64(ix)
		for j := range e.n {
			t := int64(e.at(j))
			putLeb(w, zigzag(t-pe))
			pe = t
		}
		prev = ix
	}
	w.Write(g.tail)
	return w.Flush()
}

type countWriter struct{ n int }

func (c *countWriter) Write(b []byte) (int, error) {
	c.n += len(b)
	return len(b), nil
}

// zstdStream returns the compressed size of what write produces, at the given level. Nothing
// holds the whole input or output.
func zstdStream(level zstd.EncoderLevel, write func(w *bufio.Writer) error) (int, error) {
	cw := &countWriter{}
	enc, err := zstd.NewWriter(cw, zstd.WithEncoderLevel(level), zstd.WithEncoderConcurrency(1))
	if err != nil {
		return 0, err
	}
	if err := write(bufio.NewWriterSize(enc, 1<<20)); err != nil {
		return 0, err
	}
	if err := enc.Close(); err != nil {
		return 0, err
	}
	return cw.n, nil
}

func writeAll(b []byte) func(w *bufio.Writer) error {
	return func(w *bufio.Writer) error {
		w.Write(b)
		return w.Flush()
	}
}

// sizes holds the layout B size and those compressed sizes.
type sizes struct {
	dense, dense1, dense3, plain1, plain3 int
}

// compressedSizes runs those zstd measures at the same time.
func compressedSizes(g *cgraph, keyID, valID []uint32) (sizes, error) {
	var s sizes
	dense := func(w *bufio.Writer) error { return writeDense(w, g, keyID, valID) }
	jobs := []struct {
		dst   *int
		level zstd.EncoderLevel
		write func(*bufio.Writer) error
	}{
		{&s.dense1, zstd.SpeedFastest, dense},
		{&s.dense3, zstd.SpeedDefault, dense},
		{&s.plain1, zstd.SpeedFastest, writeAll(g.b)},
		{&s.plain3, zstd.SpeedDefault, writeAll(g.b)},
	}
	errs := make([]error, len(jobs)+1)
	var wg sync.WaitGroup
	for i, j := range jobs {
		wg.Go(func() { *j.dst, errs[i] = zstdStream(j.level, j.write) })
	}
	wg.Go(func() {
		cw := &countWriter{}
		errs[len(jobs)] = writeDense(bufio.NewWriterSize(cw, 1<<20), g, keyID, valID)
		s.dense = cw.n
	})
	wg.Wait()
	for _, err := range errs {
		if err != nil {
			return s, err
		}
	}
	return s, nil
}

// measureGraph runs every measure of the full analysis. Its memory is a few arrays of one entry
// per node. It never holds a decoded copy of the graph.
func measureGraph(g *cgraph, workers int) (*graphStats, sizes, error) {
	n := g.nodes()
	slots := 1 << g.kindBits
	parts := make([]*graphStats, max(1, min(workers, n)))
	parallel(n, workers, func(w, lo, hi int) {
		parts[w] = newStats(slots)
		scanNodes(g, lo, hi, parts[w])
	})
	s := newStats(slots)
	for _, p := range parts {
		if p != nil {
			s.add(p)
		}
	}
	slices.Sort(s.ecBig)

	r := make([]fpRec, n)
	keyID := make([]uint32, n)
	valID := make([]uint32, n)
	s.distinctKeys, _ = fpStats(g, 6, r, keyID, workers)
	s.distinctVals, s.maxVal = fpStats(g, 22, r, valID, workers)
	s.dupLists, s.dupListByte = listStats(g, r, workers)
	sz, err := compressedSizes(g, keyID, valID)
	return s, sz, err
}

func analyzeGraph(out io.Writer, names []string, f string, g *cgraph, workers int) error {
	if g.nodes() >= firstUse {
		return fmt.Errorf("%s: %d nodes do not fit 31-bit ids", f, g.nodes())
	}
	s, sz, err := measureGraph(g, workers)
	if err != nil {
		return err
	}
	printGraph(out, names, f, g, s, sz)
	return nil
}

func printGraph(out io.Writer, names []string, f string, g *cgraph, s *graphStats, sz sizes) {
	kn := kindNamer{names, g.numKinds}
	size := len(g.b)
	nn := s.nodes
	mapped := "mapped"
	if len(names) != g.numKinds {
		mapped = "UNMAPPED (kind count differs)"
	}
	fmt.Fprintf(out, "\n=== %s  %d bytes, kind bits %d, %d kinds, names %s\n", label(f), size, g.kindBits, g.numKinds, mapped)
	fmt.Fprintf(out, "nodes %d (index space %d), edges %d; edges/node avg %.2f median %d p90 %d p99 %d max %d; nodes with 0 edges %.1f%%\n",
		nn, g.nodeMax, s.edges, float64(s.edges)/float64(nn), s.edgeCountAt(nn/2), s.edgeCountAt(nn*90/100), s.edgeCountAt(nn*99/100),
		s.edgeCountAt(nn-1), pct(s.noEdge, nn))

	hdr := 38 * nn
	fmt.Fprintf(out, "bytes: file header %d | node headers %d (%.1f%%: head 2B %.1f%%, index 4B %.1f%%, key fp %.1f%%, value fp %.1f%%) | edge lists %d (%.1f%%) | spilled lengths %d (%.2f%%) | trailer %d\n",
		g.start, hdr, pct(hdr, size), pct(2*nn, size), pct(4*nn, size), pct(16*nn, size), pct(16*nn, size),
		s.edgeBytes, pct(s.edgeBytes, size), s.spillBytes, pct(s.spillBytes, size), len(g.tail))

	keyRepeat := nn - s.distinctKeys
	valRepeat := nn - s.distinctVals
	fmt.Fprintf(out, "key fp: distinct %d = %.1f%% of nodes (repeats %d, %.1f%% of file); zero key %.1f%%\n",
		s.distinctKeys, pct(s.distinctKeys, nn), keyRepeat, pct(16*keyRepeat, size), pct(s.zeroKey, nn))
	fmt.Fprintf(out, "value fp: distinct %d = %.1f%% of nodes (repeats %.1f%% of file); ZERO %.1f%%; most common value %.1f%%\n",
		s.distinctVals, pct(s.distinctVals, nn), pct(16*valRepeat, size), pct(s.zeroVal, nn), pct(s.maxVal, nn))
	fmt.Fprintf(out, "edge lists with >1 edge that exactly repeat an earlier node's list: %d nodes, %d B (%.1f%% of file)\n",
		s.dupLists, s.dupListByte, pct(s.dupListByte, size))
	fmt.Fprintf(out, "index: %.2f%% of nodes have index = previous+1 in stream order; %.2f%% of the index space holds a node\n",
		pct(s.seq, nn), pct(nn, int(g.nodeMax)))

	kinds := map[uint16]*kindStat{}
	for i := range s.kinds {
		if s.kinds[i].count > 0 {
			kinds[uint16(i)] = &s.kinds[i]
		}
	}
	row := func(k *kindStat) {
		fmt.Fprintf(out, "  %-44s %9d nodes %5.1f%% | %11d B %5.1f%% | %7.1f edges/node | value ZERO %5.1f%%\n",
			kn.name(k.kind), k.count, pct(k.count, nn), k.bytes, pct(k.bytes, size), float64(k.edges)/float64(k.count), pct(k.zeroVal, k.count))
	}
	fmt.Fprintf(out, "top 20 kinds by node count:\n")
	for _, k := range sortedKinds(kinds, func(k *kindStat) int { return k.count })[:min(20, len(kinds))] {
		row(k)
	}
	fmt.Fprintf(out, "top 20 kinds by bytes:\n")
	for _, k := range sortedKinds(kinds, func(k *kindStat) int { return k.bytes })[:min(20, len(kinds))] {
		row(k)
	}

	fmt.Fprintf(out, "edge |delta| log2 buckets, %% of edges (vs source | vs previous edge, first edge vs source):\n")
	for i := range s.srcB {
		if s.srcB[i] == 0 && s.prevB[i] == 0 {
			continue
		}
		lo := uint64(0)
		if i > 0 {
			lo = 1 << (i - 1)
		}
		fmt.Fprintf(out, "  [%d,%d] %5.1f | %5.1f\n", lo, uint64(1)<<i-1, pct(s.srcB[i], s.edges), pct(s.prevB[i], s.edges))
	}
	fmt.Fprintf(out, "edge bytes: now %d (%.2f B/edge); zigzag-varint delta-vs-previous %d (%.2f B/edge, %.2fx)\n",
		s.edgeBytes, float64(s.edgeBytes)/float64(s.edges), s.zz, float64(s.zz)/float64(s.edges), float64(s.edgeBytes)/float64(s.zz))

	a := g.start + nn*(2+4+4) + (nn+7)/8 + s.idxCost + 16*(s.distinctKeys+s.distinctVals) + s.spillBytes + s.zz + len(g.tail)
	fmt.Fprintf(out, "layout A (key/value tables + u32 refs, index implied by order, delta edges): est %d B = %.2fx smaller\n",
		a, float64(size)/float64(a))
	n := float64(size)
	fmt.Fprintf(out, "layout B (built: flag byte, fp inline on first use else leb back-ref, index implied, delta edges): %d B = %.2fx; +zstd1 %.2fx; +zstd3 %.2fx\n",
		sz.dense, n/float64(sz.dense), n/float64(sz.dense1), n/float64(sz.dense3))
	fmt.Fprintf(out, "plain file: zstd1 %.2fx; zstd3 %.2fx\n", n/float64(sz.plain1), n/float64(sz.plain3))
}
