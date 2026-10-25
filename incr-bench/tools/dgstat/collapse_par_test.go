package main

import (
	"fmt"
	"math/rand/v2"
	"runtime"
	"slices"
	"sync"
	"testing"
	"time"

	"github.com/stretchr/testify/require"
	"github.com/wow-look-at-my/go-containers/set"
)

// sliceDAG is a dag held in slices.
type sliceDAG struct {
	idx   []uint32
	es    [][]uint32
	pos   []int32
	space int
}

func newSliceDAG(idx []uint32, es [][]uint32, space int) *sliceDAG {
	d := &sliceDAG{idx: idx, es: es, pos: make([]int32, space), space: space}
	for i := range d.pos {
		d.pos[i] = -1
	}
	for p, ix := range idx {
		d.pos[ix] = int32(p)
	}
	return d
}

func (d *sliceDAG) nodes() int           { return len(d.idx) }
func (d *sliceDAG) indexSpace() int      { return d.space }
func (d *sliceDAG) index(p int) uint32   { return d.idx[p] }
func (d *sliceDAG) edges(p int) edgeList { return edgeList{ids: d.es[p], n: len(d.es[p])} }
func (d *sliceDAG) position(t uint32) int {
	if int(t) >= d.space {
		return -1
	}
	return int(d.pos[t])
}

// randomDAG builds a DAG whose node indices and file positions are both shuffled, so edges point to
// higher and lower indices alike. Some indices hold no node, and some edges name them.
func randomDAG(r *rand.Rand, n, maxDeg int) ([]uint32, [][]uint32, int) {
	space := n + n/10 + 1
	perm := r.Perm(space)
	// Topological rank t gets index perm[t]. A node reads only nodes of lower rank.
	idx := make([]uint32, n)
	es := make([][]uint32, n)
	for t := range n {
		idx[t] = uint32(perm[t])
		if t == 0 {
			continue
		}
		deg := r.IntN(maxDeg + 1)
		seen := set.New[uint32]()
		for range deg {
			var e uint32
			if r.IntN(50) == 0 {
				e = uint32(perm[n+r.IntN(space-n)])
			} else {
				// Prefer near ranks, so chains form.
				lo := max(0, t-1-r.IntN(min(t, 40)))
				if r.IntN(4) == 0 {
					lo = r.IntN(t)
				}
				e = uint32(perm[lo])
			}
			if !seen.Contains(e) {
				seen.Add(e)
				es[t] = append(es[t], e)
			}
		}
	}
	order := r.Perm(n)
	sIdx := make([]uint32, n)
	sEs := make([][]uint32, n)
	for p, t := range order {
		sIdx[p], sEs[p] = idx[t], es[t]
	}
	return sIdx, sEs, space
}

func parResult(t *testing.T, d *sliceDAG, keep []bool, memo, workers int) ([][]uint32, collapseStats) {
	got := make([][]uint32, d.nodes())
	var mu sync.Mutex
	st, err := collapsePar(d, keep, defaultLiveCap, memo, workers, func(w, p int, list []uint32) {
		require.Less(t, w, max(workers, 1))
		mu.Lock()
		got[p] = append([]uint32{}, list...)
		mu.Unlock()
	})
	require.NoError(t, err)
	return got, st
}

func TestCollapseParMatchesSequential(t *testing.T) {
	r := rand.New(rand.NewPCG(1, 2))
	for trial := range 12 {
		n := 50 + r.IntN(400)
		if trial%4 == 3 {
			// Wide levels go to several workers. A narrow level runs on one.
			n = 6000
		}
		idx, es, space := randomDAG(r, n, 1+r.IntN(8))
		keep := make([]bool, n)
		frac := r.IntN(10)
		for p := range keep {
			keep[p] = r.IntN(10) < frac
		}
		want, wst, err := collapse(idx, es, keep, space, defaultLiveCap, memoMax)
		require.NoError(t, err)
		d := newSliceDAG(idx, es, space)
		for _, memo := range []int{0, 2, memoMax} {
			for _, workers := range []int{1, 4} {
				t.Run(fmt.Sprintf("trial%d/memo%d/w%d", trial, memo, workers), func(t *testing.T) {
					got, st := parResult(t, d, keep, memo, workers)
					require.Equal(t, want, got)
					require.Equal(t, wst.maxClosure, st.maxClosure)
					require.Equal(t, wst.missing, st.missing)
					require.Equal(t, wst.closures, st.closures)
					require.Equal(t, wst.closureEntries, st.closureEntries)
				})
			}
		}
	}

	// The parallel form reports a cycle too.
	d := newSliceDAG([]uint32{0, 1, 2}, [][]uint32{{1}, {2}, {1}}, 3)
	_, err := collapsePar(d, []bool{true, false, false}, defaultLiveCap, memoMax, 2, func(int, int, []uint32) {})
	require.ErrorContains(t, err, "cycle")

	// A wide level that holds too much fails loud.
	idx, es := []uint32{0}, [][]uint32{nil}
	for i := 1; i <= 2000; i++ {
		idx = append(idx, uint32(i))
		es = append(es, []uint32{0})
	}
	idx, es = append(idx, 2001), append(es, slices.Collect(func(yield func(uint32) bool) {
		for i := uint32(1); i <= 2000; i++ {
			yield(i)
		}
	}))
	keep := make([]bool, len(idx))
	keep[0], keep[len(idx)-1] = true, true
	_, err = collapsePar(newSliceDAG(idx, es, len(idx)), keep, 100, memoMax, 4, func(int, int, []uint32) {})
	require.ErrorContains(t, err, "live closures hold more than 100 entries")
}

// TestCollapseParMemoryBounded runs a long ladder where every dropped node reaches many kept nodes.
func TestCollapseParMemoryBounded(t *testing.T) {
	const rungs, width = 3000, 8
	n := rungs * 2
	idx := make([]uint32, n)
	es := make([][]uint32, n)
	keep := make([]bool, n)
	for i := range n {
		idx[i] = uint32(i)
	}
	// Even positions are kept leaves. Odd position 2k+1 is a dropped node that
	// reads the dropped node and the last few leaves.
	for k := range rungs {
		keep[2*k] = true
		d := 2*k + 1
		for j := range width {
			if l := 2 * (k - j); l >= 0 {
				es[d] = append(es[d], uint32(l))
			}
		}
		if k > 0 {
			es[d] = append(es[d], uint32(d-2))
		}
	}
	// One kept root reads the last dropped node, so its list holds every leaf.
	idx = append(idx, uint32(n))
	es = append(es, []uint32{uint32(n - 1)})
	keep = append(keep, true)
	d := newSliceDAG(idx, es, n+1)

	runtime.GC()
	var before runtime.MemStats
	runtime.ReadMemStats(&before)
	var peak uint64
	stop := make(chan struct{})
	var wg sync.WaitGroup
	wg.Add(1)
	go func() {
		defer wg.Done()
		for {
			var m runtime.MemStats
			runtime.ReadMemStats(&m)
			peak = max(peak, m.HeapAlloc)
			select {
			case <-stop:
				return
			case <-time.After(2 * time.Millisecond):
			}
		}
	}()
	rootLen := 0
	st, err := collapsePar(d, keep, defaultLiveCap, memoMax, 4, func(w, p int, list []uint32) {
		if p == n {
			rootLen = len(list)
		}
	})
	close(stop)
	wg.Wait()
	require.NoError(t, err)
	require.Equal(t, rungs, rootLen)
	require.Equal(t, rungs, st.maxClosure)
	require.LessOrEqual(t, st.peakLive, 2*memoMax)
	require.Equal(t, rungs*(rungs+1)/2, st.closureEntries)
	limit := uint64(n)*64 + 4<<20
	require.Less(t, peak-min(peak, before.HeapAlloc), limit, "peak heap %d", peak)
}
