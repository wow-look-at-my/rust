// closureStore holds the memoized closures.

package main

import (
	"fmt"
	"math/bits"
	"sync"
	"sync/atomic"
)

// closureStore holds the memoized closures. A node names its closure by an
// id, so the per-node cost is one int32.
type closureStore struct {
	mu     sync.Mutex
	chunks []*[1 << 16][]uint32
	freeID []int32
	nextID int32
	free   [8][][]uint32
}

func newClosureStore(maxIDs int) *closureStore {
	return &closureStore{chunks: make([]*[1 << 16][]uint32, maxIDs>>16+1)}
}

func (s *closureStore) slot(id int32) *[]uint32 { return &s.chunks[id>>16][id&(1<<16-1)] }

// put copies c into a pooled list and returns its id.
func (s *closureStore) put(c []uint32) int32 {
	class := bits.Len(uint(len(c) - 1))
	s.mu.Lock()
	var l []uint32
	if class < len(s.free) {
		if f := s.free[class]; len(f) > 0 {
			l = f[len(f)-1]
			s.free[class] = f[:len(f)-1]
		}
	}
	var id int32
	if n := len(s.freeID); n > 0 {
		id = s.freeID[n-1]
		s.freeID = s.freeID[:n-1]
	} else {
		id = s.nextID
		s.nextID++
		if s.chunks[id>>16] == nil {
			s.chunks[id>>16] = new([1 << 16][]uint32)
		}
	}
	s.mu.Unlock()
	if l == nil {
		l = make([]uint32, 0, 1<<class)
	}
	*s.slot(id) = append(l[:0], c...)
	return id
}

func (s *closureStore) get(id int32) []uint32 { return *s.slot(id) }

func (s *closureStore) release(id int32) {
	sl := s.slot(id)
	l := *sl
	*sl = nil
	class := bits.Len(uint(len(l) - 1))
	s.mu.Lock()
	if class < len(s.free) {
		s.free[class] = append(s.free[class], l)
	}
	s.freeID = append(s.freeID, id)
	s.mu.Unlock()
}

type parWalk struct {
	e edgeList
	i int
}

type parWorker struct {
	id      int
	mark    []uint32
	stamp   uint32
	walks   []parWalk
	buf     []uint32
	pending []edgeList
	st      collapseStats
}

// collapsePar gives each kept node of g, as its edges, the kept nodes it reaches through chains of
// dropped nodes. That collapsePar is in the first-visit order of a depth-first walk without repeats. It
// is the parallel form of collapse. All nodes of one level read only closures of lower levels, so
// workers build one level at a time.
//
// sink receives each kept node with its list. The list is a worker buffer, valid only during the call.
// Calls for different nodes run at the same time, and w names the worker.
func collapsePar(g dag, keep []bool, liveCap, memo, workers int, sink func(w, p int, list []uint32)) (collapseStats, error) {
	var st collapseStats
	st.maxClosurePos = -1
	n := g.nodes()
	space := g.indexSpace()
	users := make([]int32, n)
	for p := range n {
		e := g.edges(p)
		for j := range e.n {
			q := g.position(e.at(j))
			if q < 0 {
				st.missing++
			} else if !keep[q] {
				users[q]++
			}
		}
	}

	byLevel, starts, err := levelOrder(g, keep)
	if err != nil {
		return st, err
	}
	st.levels = len(starts) - 1
	for l := 0; l+1 < len(starts); l++ {
		st.widest = max(st.widest, starts[l+1]-starts[l])
	}

	store := newClosureStore(n)
	closID := make([]int32, n)
	for i := range closID {
		closID[i] = -1
	}
	big := make([]bool, n)
	var live, peak atomic.Int64
	var failed atomic.Bool

	ws := make([]*parWorker, max(workers, 1))
	for i := range ws {
		ws[i] = &parWorker{id: i, mark: make([]uint32, max(space, 1)), st: collapseStats{maxClosurePos: -1}}
	}
	build := func(w *parWorker, e edgeList) []uint32 {
		w.stamp++
		res := w.buf[:0]
		add := func(t uint32) {
			if int(t) >= space {
				res = append(res, t)
				return
			}
			if w.mark[t] != w.stamp {
				w.mark[t] = w.stamp
				res = append(res, t)
			}
		}
		w.walks = append(w.walks[:0], parWalk{e, 0})
		for len(w.walks) > 0 {
			k := &w.walks[len(w.walks)-1]
			if k.i == k.e.n {
				w.walks = w.walks[:len(w.walks)-1]
				continue
			}
			t := k.e.at(k.i)
			k.i++
			p := g.position(t)
			switch {
			case p < 0 || keep[p]:
				add(t)
			case big[p]:
				if w.mark[t] != w.stamp {
					w.mark[t] = w.stamp
					w.walks = append(w.walks, parWalk{g.edges(p), 0})
				}
			case closID[p] >= 0:
				for _, c := range store.get(closID[p]) {
					add(c)
				}
			}
		}
		w.buf = res
		return res
	}
	// release drops one use of each dropped target. The worker that drops the last use frees the
	// closure. Every other user has built its list by then. A big node keeps its own targets until its
	// last use is gone, because each use walks them again.
	release := func(w *parWorker, e edgeList) {
		w.pending = append(w.pending[:0], e)
		for len(w.pending) > 0 {
			e := w.pending[len(w.pending)-1]
			w.pending = w.pending[:len(w.pending)-1]
			for j := range e.n {
				q := g.position(e.at(j))
				if q < 0 || keep[q] || atomic.AddInt32(&users[q], -1) > 0 {
					continue
				}
				if id := closID[q]; id >= 0 {
					live.Add(-int64(len(store.get(id))))
					closID[q] = -1
					store.release(id)
				}
				if big[q] {
					w.pending = append(w.pending, g.edges(q))
				}
			}
		}
	}
	process := func(w *parWorker, p int) {
		e := g.edges(p)
		c := build(w, e)
		if keep[p] {
			sink(w.id, p, c)
			release(w, e)
			return
		}
		w.st.closures++
		w.st.closureEntries += len(c)
		if len(c) > w.st.maxClosure || w.st.maxClosurePos < 0 || (len(c) == w.st.maxClosure && p < w.st.maxClosurePos) {
			w.st.maxClosure, w.st.maxClosurePos = len(c), p
		}
		// No user of p runs before this level ends, so users[p] does not change here.
		u := atomic.LoadInt32(&users[p])
		switch {
		case u > 0 && len(c) > memo:
			big[p] = true
			w.st.big++
			return
		case u > 0 && len(c) > 0:
			closID[p] = store.put(c)
			l := live.Add(int64(len(c)))
			for {
				old := peak.Load()
				if l <= old || peak.CompareAndSwap(old, l) {
					break
				}
			}
			if l > int64(liveCap) {
				failed.Store(true)
			}
		}
		release(w, e)
	}

	for l := 0; l+1 < len(starts); l++ {
		nodes := byLevel[starts[l]:starts[l+1]]
		if len(nodes) < 2*len(ws) || len(ws) == 1 {
			for _, p := range nodes {
				process(ws[0], int(p))
			}
		} else {
			// A node can walk a long chain of big nodes, so even a small level splits across workers.
			chunk := max(1, min(256, len(nodes)/(8*len(ws))))
			var next atomic.Int64
			var wg sync.WaitGroup
			for _, w := range ws {
				wg.Add(1)
				go func() {
					defer wg.Done()
					for {
						s := int(next.Add(int64(chunk))) - chunk
						if s >= len(nodes) {
							return
						}
						for _, p := range nodes[s:min(s+chunk, len(nodes))] {
							process(w, int(p))
						}
					}
				}()
			}
			wg.Wait()
		}
		if failed.Load() {
			return st, fmt.Errorf("live closures hold more than %d entries at level %d of %d", liveCap, l, len(starts)-2)
		}
	}
	for _, w := range ws {
		st.closures += w.st.closures
		st.closureEntries += w.st.closureEntries
		st.big += w.st.big
		if w.st.maxClosurePos >= 0 && (st.maxClosurePos < 0 || w.st.maxClosure > st.maxClosure ||
			(w.st.maxClosure == st.maxClosure && w.st.maxClosurePos < st.maxClosurePos)) {
			st.maxClosure, st.maxClosurePos = w.st.maxClosure, w.st.maxClosurePos
		}
	}
	st.peakLive = int(peak.Load())
	return st, nil
}

// levelOrder returns the positions sorted by level, and where each level starts. An edge may name a
// higher index, so the walk is a depth-first search without recursion.
func levelOrder(g dag, keep []bool) ([]int32, []int, error) {
	const unset, open = -1, -2
	n := g.nodes()
	level := make([]int32, n)
	for i := range level {
		level[i] = unset
	}
	maxLevel := 0
	type frame struct {
		p int
		e edgeList
		i int
	}
	var stack []frame
	for root := range n {
		if level[root] != unset {
			continue
		}
		level[root] = open
		stack = append(stack[:0], frame{root, g.edges(root), 0})
		for len(stack) > 0 {
			f := &stack[len(stack)-1]
			pushed := false
			for f.i < f.e.n {
				q := g.position(f.e.at(f.i))
				f.i++
				if q < 0 || keep[q] {
					continue
				}
				if level[q] == open {
					return nil, nil, fmt.Errorf("cycle through the node with index %d", g.index(q))
				}
				if level[q] == unset {
					level[q] = open
					stack = append(stack, frame{q, g.edges(q), 0})
					pushed = true
					break
				}
			}
			if pushed {
				continue
			}
			var l int32
			for j := range f.e.n {
				if q := g.position(f.e.at(j)); q >= 0 && !keep[q] {
					l = max(l, level[q]+1)
				}
			}
			level[f.p] = l
			maxLevel = max(maxLevel, int(l))
			stack = stack[:len(stack)-1]
		}
	}
	starts := make([]int, maxLevel+2)
	for _, l := range level {
		starts[l+1]++
	}
	for l := 1; l < len(starts); l++ {
		starts[l] += starts[l-1]
	}
	// fill[l] is the next free slot of level l.
	fill := append([]int(nil), starts[:maxLevel+1]...)
	byLevel := make([]int32, n)
	for p, l := range level {
		byLevel[fill[l]] = int32(p)
		fill[l]++
	}
	return byLevel, starts, nil
}
