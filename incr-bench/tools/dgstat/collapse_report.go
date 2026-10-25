// edgeCost is what one worker adds up over the kept nodes it emits.

package main

import (
	"bufio"
	"encoding/binary"
	"fmt"
	"io"
	"os"
	"runtime"
	"runtime/debug"
	"slices"
	"sort"
	"strconv"
	"strings"
	"sync/atomic"
	"syscall"
	"time"

	"github.com/spf13/cobra"
	"github.com/wow-look-at-my/go-containers/set"
)

type layoutSizes struct {
	current, newOrig, newRenum int
}

// edgeCost is what one worker adds up over the kept nodes it emits.
type edgeCost struct {
	kept, edges, maxE int
	// cur is the edge bytes and spilled counts of the 38-byte layout.
	cur, newOrig, newRenum int
}

func headerSizes(g *cgraph, keep []bool, renum []uint32) layoutSizes {
	var s layoutSizes
	var zero [16]byte
	for _, re := range []bool{false, true} {
		size := 0
		keys, vals := newFPRefs(), newFPRefs()
		var prev uint32
		havePrev := false
		for p := range g.nodes() {
			if !keep[p] {
				continue
			}
			ix := g.index(p)
			if re {
				ix = renum[ix]
			}
			size += 3
			if !havePrev || prev+1 != ix {
				size += 4
			}
			prev, havePrev = ix, true
			if t, ok := keys.replace(g.key(p), ix); ok {
				size += lebLen(uint64(ix - t))
			} else {
				size += 16
			}
			if v := g.val(p); v != zero {
				if t, ok := vals.replace(v, ix); ok {
					size += lebLen(uint64(ix - t))
				} else {
					size += 16
				}
			}
			if !re {
				s.current += 38
			}
		}
		if re {
			s.newRenum = size
		} else {
			s.newOrig = size
		}
	}
	return s
}

type collapsePolicy struct {
	name, desc string
	// kinds keeps every node of a listed kind. nil keeps every node.
	kinds        []bool
	dropUpstream bool
}

func (pol collapsePolicy) keep(g *cgraph, class []uint8, keep []bool) {
	for p := range g.nodes() {
		k := pol.kinds == nil || pol.kinds[g.kind(p)] || g.edges(p).n == 0
		if pol.dropUpstream && class[p] == crateUpstream {
			k = false
		}
		keep[p] = k
	}
}

// collapsePolicies builds the keep rules. P1 keeps the non_query kinds but Null, every cache_on_disk or
// eval_always query, and every node with no edges. P3 is P1 without TraitSelect and AnonZeroDeps by
// kind. Their nodes with no edges stay.
func collapsePolicies(defs []kindDef, count, edgeSum []int) []collapsePolicy {
	nk := len(defs)
	base := make([]bool, nk)
	for k, d := range defs {
		if d.query {
			base[k] = d.cacheOnDisk || d.evalAlways
		} else {
			base[k] = d.name != "Null"
		}
	}
	heavy := make([]bool, nk)
	for k := range heavy {
		heavy[k] = base[k] || (count[k] > 0 && edgeSum[k] >= 16*count[k])
	}
	thin := slices.Clone(base)
	for k, d := range defs {
		if d.name == "TraitSelect" || d.name == "AnonZeroDeps" {
			thin[k] = false
		}
	}
	return []collapsePolicy{
		{"P0", "keep everything", nil, false},
		{"P1", "non_query but Null + cache_on_disk/eval_always queries + nodes with 0 edges", base, false},
		{"P2", "P1 + kinds with avg edges >= 16", heavy, false},
		{"P3", "P1 without TraitSelect and AnonZeroDeps by kind", thin, false},
		{"P1u", "P1, but every node with an upstream crate half in its key is dropped", base, true},
	}
}

const (
	crateLocal = iota
	crateUpstream
	crateTail
)

// upstreamMin is the smallest node count that makes a non-local key half an upstream crate.
const upstreamMin = 1000

type crateHalves struct {
	class    []uint8
	distinct int
	top      uint64
	topCount int
	// upstream holds the halves with at least upstreamMin nodes, most common first, without top.
	upstream []halfCount
}

type halfCount struct {
	h uint64
	c int
}

// crateClasses sorts each node by the first u64 of its key. The most common
// half is the local crate.
func crateClasses(g *cgraph) crateHalves {
	n := g.nodes()
	sorted := make([]uint64, n)
	for p := range n {
		sorted[p] = binary.LittleEndian.Uint64(g.b[g.off[p]+6:])
	}
	slices.Sort(sorted)
	var r crateHalves
	var runs []halfCount
	for i := 0; i < n; {
		j := i
		for j < n && sorted[j] == sorted[i] {
			j++
		}
		r.distinct++
		if j-i >= upstreamMin {
			runs = append(runs, halfCount{sorted[i], j - i})
		}
		if j-i > r.topCount {
			r.top, r.topCount = sorted[i], j-i
		}
		i = j
	}
	sorted = nil
	sort.Slice(runs, func(i, j int) bool { return runs[i].c > runs[j].c })
	up := set.New[uint64]()
	for _, e := range runs {
		if e.h != r.top {
			r.upstream = append(r.upstream, e)
			up.Add(e.h)
		}
	}
	r.class = make([]uint8, n)
	for p := range n {
		h := binary.LittleEndian.Uint64(g.b[g.off[p]+6:])
		switch {
		case h == r.top:
			r.class[p] = crateLocal
		case up.Contains(h):
			r.class[p] = crateUpstream
		default:
			r.class[p] = crateTail
		}
	}
	return r
}

// reportCrates prints the share of nodes and of 38-byte-layout bytes per crate class.
func reportCrates(out io.Writer, kn kindNamer, g *cgraph, ch crateHalves, size int) {
	var nodes, byt [3]int
	var kinds [3]map[uint16]*kindStat
	for i := range kinds {
		kinds[i] = map[uint16]*kindStat{}
	}
	for p := range g.nodes() {
		c := ch.class[p]
		b := g.nodeBytes(p)
		nodes[c]++
		byt[c] += b
		k := kinds[c][g.kind(p)]
		if k == nil {
			k = &kindStat{kind: g.kind(p)}
			kinds[c][g.kind(p)] = k
		}
		k.count++
		k.bytes += b
	}
	nb := size - g.start - len(g.tail)
	fmt.Fprintf(out, "key crate half: %d distinct values; top value %016x has %d nodes; %d other values have >= %d nodes\n",
		ch.distinct, ch.top, ch.topCount, len(ch.upstream), upstreamMin)
	for i, name := range []string{"local (top value)", fmt.Sprintf("upstream (%d values)", len(ch.upstream)), "tail (rare values)"} {
		top := sortedKinds(kinds[i], func(k *kindStat) int { return k.count })
		var s []string
		for _, k := range top[:min(5, len(top))] {
			s = append(s, fmt.Sprintf("%s %.0f%%", kn.name(k.kind), pct(k.count, nodes[i])))
		}
		fmt.Fprintf(out, "  %-22s %9d nodes %5.1f%% | %11d B %5.1f%% of node bytes | top kinds: %v\n",
			name, nodes[i], pct(nodes[i], g.nodes()), byt[i], pct(byt[i], nb), s)
	}
	fmt.Fprintf(out, "  most common upstream values:")
	for _, e := range ch.upstream[:min(8, len(ch.upstream))] {
		fmt.Fprintf(out, " %016x=%d", e.h, e.c)
	}
	fmt.Fprintln(out)
}

type collapseRow struct {
	pol         collapsePolicy
	cost        edgeCost
	sizes       layoutSizes
	st          collapseStats
	closureKind string
	elapsed     time.Duration
}

func analyzeCollapse(out io.Writer, defs []kindDef, f string, size int, g *cgraph, workers int) error {
	names := kindNames(defs)
	kn := kindNamer{names, g.numKinds}
	if len(names) != g.numKinds {
		return fmt.Errorf("%s: kind names UNMAPPED (%d names, %d kinds in the trailer); the policies need the right kinds",
			label(f), len(names), g.numKinds)
	}
	n := g.nodes()
	fmt.Fprintf(out, "\n=== collapse %s  %d bytes, %d nodes, %d edges, names mapped, %d workers\n", label(f), size, n, g.edgeSum, workers)
	ch := crateClasses(g)
	reportCrates(out, kn, g, ch, size)

	count := make([]int, g.numKinds)
	edgeSum := make([]int, g.numKinds)
	for p := range n {
		count[g.kind(p)]++
		edgeSum[g.kind(p)] += g.edges(p).n
	}
	maxInline := 1<<(16-g.kindBits-2) - 2
	space := g.indexSpace()
	keep := make([]bool, n)
	renum := make([]uint32, space)
	var rows []collapseRow
	for _, pol := range collapsePolicies(defs, count, edgeSum) {
		t0 := time.Now()
		pol.keep(g, ch.class, keep)
		// renum[i] is the stream rank of the kept node with index i. An index with no kept node keeps its value.
		for i := range renum {
			renum[i] = uint32(i)
		}
		var rank uint32
		for p := range n {
			if keep[p] {
				renum[g.index(p)] = rank
				rank++
			}
		}
		costs := make([]edgeCost, workers)
		sink := func(w, p int, list []uint32) {
			c := &costs[w]
			c.kept++
			c.edges += len(list)
			c.maxE = max(c.maxE, len(list))
			var m uint32
			ix := int64(g.index(p))
			ri := int64(renum[ix])
			po, pr := ix, ri
			for _, e := range list {
				m = max(m, e)
				c.newOrig += slebLen(int64(e) - po)
				po = int64(e)
				r := int64(e)
				if int(e) < space {
					r = int64(renum[e])
				}
				c.newRenum += slebLen(r - pr)
				pr = r
			}
			c.cur += len(list) * edgeWidth(m)
			if len(list) > maxInline {
				s := lebLen(uint64(len(list)))
				c.cur += s
				c.newOrig += s
				c.newRenum += s
			}
		}
		st, err := collapsePar(g, keep, defaultLiveCap, memoMax, workers, sink)
		if err != nil {
			fmt.Fprintf(out, "%s FAILED: %v\n", pol.name, err)
			continue
		}
		r := collapseRow{pol: pol, st: st}
		for _, c := range costs {
			r.cost.kept += c.kept
			r.cost.edges += c.edges
			r.cost.maxE = max(r.cost.maxE, c.maxE)
			r.cost.cur += c.cur
			r.cost.newOrig += c.newOrig
			r.cost.newRenum += c.newRenum
		}
		h := headerSizes(g, keep, renum)
		fixed := g.start + len(g.tail)
		r.sizes = layoutSizes{
			current:  fixed + h.current + r.cost.cur,
			newOrig:  fixed + h.newOrig + r.cost.newOrig,
			newRenum: fixed + h.newRenum + r.cost.newRenum,
		}
		if st.maxClosurePos >= 0 {
			r.closureKind = kn.name(g.kind(st.maxClosurePos))
		}
		r.elapsed = time.Since(t0)
		rows = append(rows, r)

		if pol.name == "P0" {
			if r.sizes.current != size {
				fmt.Fprintf(out, "P0 SANITY FAIL: 38-byte estimate %d != file %d\n", r.sizes.current, size)
			}
			if uint64(r.cost.edges) != g.edgeSum {
				fmt.Fprintf(out, "P0 SANITY FAIL: edges %d != %d\n", r.cost.edges, g.edgeSum)
			}
			continue
		}
		kept := map[uint16]*kindStat{}
		dropped := map[uint16]*kindStat{}
		for p := range n {
			m := dropped
			if keep[p] {
				m = kept
			}
			kd := g.kind(p)
			k := m[kd]
			if k == nil {
				k = &kindStat{kind: kd}
				m[kd] = k
			}
			k.count++
			k.edges += g.edges(p).n
		}
		fmt.Fprintf(out, "%s dropped nodes, top 25 kinds by count (of %d dropped kinds):\n", pol.name, len(dropped))
		for _, k := range sortedKinds(dropped, func(k *kindStat) int { return k.count })[:min(25, len(dropped))] {
			fmt.Fprintf(out, "  %-44s %9d dropped %5.1f%% of all nodes | %6.1f edges/node before\n",
				kn.name(k.kind), k.count, pct(k.count, n), float64(k.edges)/float64(k.count))
		}
		fmt.Fprintf(out, "%s kept nodes per kind (%d kinds):\n", pol.name, len(kept))
		list := sortedKinds(kept, func(k *kindStat) int { return k.count })
		sort.SliceStable(list, func(i, j int) bool {
			return list[i].count > list[j].count || (list[i].count == list[j].count && list[i].kind < list[j].kind)
		})
		for _, k := range list {
			fmt.Fprintf(out, "  %-44s %9d kept\n", kn.name(k.kind), k.count)
		}
	}
	mb := func(v int) string { return fmt.Sprintf("%.1f MB", float64(v)/1e6) }
	fmt.Fprintf(out, "| policy | kept nodes | kept edges | max edges | avg edges | 38-byte layout | new layout | new, renumbered | max closure (kind) | closures / entries / peak live / walked again | missing targets | time |\n")
	fmt.Fprintf(out, "|---|---|---|---|---|---|---|---|---|---|---|---|\n")
	for _, r := range rows {
		fmt.Fprintf(out, "| %s %s | %d (%.1f%%) | %d (%.1f%%) | %d | %.2f | %s | %s | %s | %d (%s) | %d / %d / %d / %d | %d | %.1fs |\n",
			r.pol.name, r.pol.desc, r.cost.kept, pct(r.cost.kept, n), r.cost.edges, pct(r.cost.edges, int(g.edgeSum)), r.cost.maxE,
			float64(r.cost.edges)/float64(max(r.cost.kept, 1)), mb(r.sizes.current), mb(r.sizes.newOrig), mb(r.sizes.newRenum),
			r.st.maxClosure, r.closureKind, r.st.closures, r.st.closureEntries, r.st.peakLive, r.st.big, r.st.missing, r.elapsed.Seconds())
	}
	return nil
}

// procStatus returns the named kB fields of /proc/self/status.
func procStatus(fields ...string) map[string]int {
	res := map[string]int{}
	fh, err := os.Open("/proc/self/status")
	if err != nil {
		return res
	}
	defer fh.Close()
	sc := bufio.NewScanner(fh)
	for sc.Scan() {
		name, rest, ok := strings.Cut(sc.Text(), ":")
		if !ok || !slices.Contains(fields, name) {
			continue
		}
		v, err := strconv.Atoi(strings.TrimSuffix(strings.TrimSpace(rest), " kB"))
		if err == nil {
			res[name] = v
		}
	}
	return res
}

// anonPeak samples RssAnon, the heap and stack pages that no file backs, until stop is closed.
func anonPeak(stop <-chan struct{}, peak *atomic.Int64) {
	t := time.NewTicker(20 * time.Millisecond)
	defer t.Stop()
	for {
		if v := int64(procStatus("RssAnon")["RssAnon"]); v > peak.Load() {
			peak.Store(v)
		}
		select {
		case <-stop:
			return
		case <-t.C:
		}
	}
}

func cpuTime() time.Duration {
	var ru syscall.Rusage
	if err := syscall.Getrusage(syscall.RUSAGE_SELF, &ru); err != nil {
		return 0
	}
	return time.Duration(ru.Utime.Nano() + ru.Stime.Nano())
}

func init() {
	var workers int
	cmd := &cobra.Command{
		Use:   "collapse QUERIES.rs DEP-GRAPH.bin...",
		Short: "Simulate a dep graph that persists only some nodes and splices edges through the rest",
		Args:  cobra.MinimumNArgs(2),
		RunE: func(cmd *cobra.Command, args []string) error {
			defs, err := readKinds(args[0])
			if err != nil {
				return err
			}
			if workers < 1 {
				return fmt.Errorf("--workers must be at least 1, got %d", workers)
			}
			// The closure lists churn, so the heap is kept close to its live size.
			debug.SetGCPercent(25)
			out := cmd.OutOrStdout()
			for _, f := range args[1:] {
				if err := collapseFile(out, defs, f, workers); err != nil {
					return err
				}
			}
			return nil
		},
	}
	cmd.Flags().IntVar(&workers, "workers", runtime.GOMAXPROCS(0), "number of workers that build one level")
	rootCmd.AddCommand(cmd)
}

func collapseFile(out io.Writer, defs []kindDef, f string, workers int) error {
	m, data, err := mapFile(f)
	if err != nil {
		return err
	}
	defer m.Unmap()
	t0, c0 := time.Now(), cpuTime()
	var peak atomic.Int64
	stop := make(chan struct{})
	done := make(chan struct{})
	go func() { anonPeak(stop, &peak); close(done) }()
	g, err := parseCompact(data)
	if err != nil {
		close(stop)
		<-done
		return fmt.Errorf("%s: %w", f, err)
	}
	parse := time.Since(t0)
	err = analyzeCollapse(out, defs, f, len(data), g, workers)
	close(stop)
	<-done
	wall, cpu := time.Since(t0), cpuTime()-c0
	st := procStatus("VmHWM", "RssFile", "RssShmem")
	fmt.Fprintf(out, "resources %s: wall %.1fs (index scan %.1fs), cpu %.1fs = %.0f%%, peak RssAnon %d MB, process VmHWM %d MB (includes mapped pages: RssFile %d MB, RssShmem %d MB now)\n",
		label(f), wall.Seconds(), parse.Seconds(), cpu.Seconds(), 100*cpu.Seconds()/wall.Seconds(), peak.Load()/1024,
		st["VmHWM"]/1024, st["RssFile"]/1024, st["RssShmem"]/1024)
	return err
}
