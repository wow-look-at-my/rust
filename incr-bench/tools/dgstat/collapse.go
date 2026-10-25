// collapse simulates a dep graph that persists only some nodes, with a sequential reference form.

package main

import (
	"encoding/binary"
	"fmt"
	"math/bits"
)

// defaultLiveCap bounds the closure entries in memory.
const defaultLiveCap = 400_000_000

// memoMax is the longest closure that collapse stores.
const memoMax = 64

type collapseStats struct {
	// maxClosure is the largest kept-closure of one dropped node, and maxClosurePos is that node.
	maxClosure, maxClosurePos int
	// closures counts the dropped nodes whose closure was computed. closureEntries is the sum of their sizes.
	closures, closureEntries int
	// peakLive is the most closure entries held in memory at one time.
	peakLive int
	// big counts the dropped nodes whose closure is longer than memoMax and is walked again at each use.
	big int
	// missing counts edges whose target index holds no node. Such a target is treated as kept.
	missing int
}

// collapse gives each kept node, as its edges, the kept nodes it reaches through chains of dropped nodes.
// The order is the first-visit order of a depth-first walk, without repeats. index[i] and edges[i] describe
// node i, and an edge names a target by its index. The result holds nil for a dropped node. It is the
// sequential reference that the tests hold collapsePar to.
func collapse(index []uint32, edges [][]uint32, keep []bool, indexSpace, liveCap, memo int) ([][]uint32, collapseStats, error) {
	var st collapseStats
	st.maxClosurePos = -1
	pos := make([]int32, indexSpace)
	for i := range pos {
		pos[i] = -1
	}
	for i, ix := range index {
		pos[ix] = int32(i)
	}
	lookup := func(t uint32) int {
		if int(t) >= indexSpace {
			return -1
		}
		return int(pos[t])
	}
	for _, es := range edges {
		for _, t := range es {
			if lookup(t) < 0 {
				st.missing++
			}
		}
	}

	const (
		unvisited = iota
		open
		done
		// big marks a done node whose closure is longer than memoMax.
		big
	)
	state := make([]uint8, len(index))
	clos := make([][]uint32, len(index))

	// mark[t] == stamp means that target t is already in the list under construction.
	mark := make([]uint32, max(indexSpace, 1))
	var stamp uint32
	type walk struct {
		es []uint32
		i  int
	}
	var walks []walk
	build := func(es []uint32) []uint32 {
		stamp++
		var out []uint32
		add := func(t uint32) {
			if int(t) >= indexSpace {
				out = append(out, t)
				return
			}
			if mark[t] != stamp {
				mark[t] = stamp
				out = append(out, t)
			}
		}
		walks = append(walks[:0], walk{es, 0})
		for len(walks) > 0 {
			w := &walks[len(walks)-1]
			if w.i == len(w.es) {
				walks = walks[:len(walks)-1]
				continue
			}
			t := w.es[w.i]
			w.i++
			p := lookup(t)
			switch {
			case p < 0 || keep[p]:
				add(t)
			case state[p] == big:
				if mark[t] != stamp {
					mark[t] = stamp
					walks = append(walks, walk{edges[p], 0})
				}
			default:
				for _, c := range clos[p] {
					add(c)
				}
			}
		}
		return out
	}
	// users[q] counts the edges into dropped node q whose source has not built its list yet.
	users := make([]int32, len(index))
	for _, es := range edges {
		for _, t := range es {
			if q := lookup(t); q >= 0 && !keep[q] {
				users[q]++
			}
		}
	}
	live := 0
	// release drops one use of each dropped target.
	var pending [][]uint32
	release := func(es []uint32) {
		pending = append(pending[:0], es)
		for len(pending) > 0 {
			es := pending[len(pending)-1]
			pending = pending[:len(pending)-1]
			for _, t := range es {
				q := lookup(t)
				if q < 0 || keep[q] {
					continue
				}
				users[q]--
				if users[q] > 0 {
					continue
				}
				live -= len(clos[q])
				clos[q] = nil
				if state[q] == big {
					pending = append(pending, edges[q])
				}
			}
		}
	}
	type frame struct{ p, i int }
	var stack []frame
	// ensure computes the closure of dropped node p and of every dropped node below it, without recursion.
	ensure := func(p int) error {
		if state[p] >= done {
			return nil
		}
		state[p] = open
		stack = append(stack[:0], frame{p, 0})
		for len(stack) > 0 {
			f := &stack[len(stack)-1]
			es := edges[f.p]
			pushed := false
			for f.i < len(es) {
				q := lookup(es[f.i])
				f.i++
				if q < 0 || keep[q] {
					continue
				}
				if state[q] == open {
					return fmt.Errorf("cycle through the node with index %d", index[q])
				}
				if state[q] == unvisited {
					state[q] = open
					stack = append(stack, frame{q, 0})
					pushed = true
					break
				}
			}
			if pushed {
				continue
			}
			c := build(es)
			state[f.p] = done
			st.closures++
			st.closureEntries += len(c)
			if len(c) > st.maxClosure || st.maxClosurePos < 0 {
				st.maxClosure, st.maxClosurePos = len(c), f.p
			}
			switch {
			case len(c) > memo && users[f.p] > 0:
				state[f.p] = big
				st.big++
			case users[f.p] > 0:
				release(es)
			default:
				release(es)
				c = nil
			}
			if c != nil && state[f.p] == done {
				clos[f.p] = c
				live += len(c)
				st.peakLive = max(st.peakLive, live)
				if live > liveCap {
					return fmt.Errorf("live closures hold more than %d entries after %d closures (largest %d)",
						liveCap, st.closures, st.maxClosure)
				}
			}
			stack = stack[:len(stack)-1]
		}
		return nil
	}

	out := make([][]uint32, len(index))
	for p := range index {
		if !keep[p] {
			if err := ensure(p); err != nil {
				return nil, st, err
			}
			continue
		}
		for _, t := range edges[p] {
			if q := lookup(t); q >= 0 && !keep[q] {
				if err := ensure(q); err != nil {
					return nil, st, err
				}
			}
		}
		out[p] = build(edges[p])
		if out[p] == nil {
			out[p] = []uint32{}
		}
		release(edges[p])
	}
	return out, st, nil
}

// slebLen is the length of v as signed leb128, the encoding of rustc's emit_i64.
func slebLen(v int64) int {
	n := 0
	for {
		b := v & 0x7f
		v >>= 7
		n++
		if (v == 0 && b&0x40 == 0) || (v == -1 && b&0x40 != 0) {
			return n
		}
	}
}

func edgeWidth(m uint32) int {
	return max(1, 4-bits.LeadingZeros32(m)/8)
}

type fpRefs struct {
	fp  [][16]byte
	idx []uint32
}

const fpSlots = 1 << 17

func newFPRefs() *fpRefs {
	r := &fpRefs{fp: make([][16]byte, fpSlots), idx: make([]uint32, fpSlots)}
	for i := range r.idx {
		r.idx[i] = 1<<32 - 1
	}
	return r
}

// replace returns the index of the last earlier node with fp, and records index in its place.
func (r *fpRefs) replace(fp [16]byte, index uint32) (uint32, bool) {
	slot := (binary.LittleEndian.Uint64(fp[:8]) ^ binary.LittleEndian.Uint64(fp[8:])) & (fpSlots - 1)
	oldFP, oldIdx := r.fp[slot], r.idx[slot]
	r.fp[slot], r.idx[slot] = fp, index
	return oldIdx, oldFP == fp && oldIdx < index
}
