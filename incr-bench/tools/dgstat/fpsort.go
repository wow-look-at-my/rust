// fpRec is a 128-bit fingerprint or hash and the position of the node that holds it.

package main

import (
	"cmp"
	"encoding/binary"
	"slices"
	"sync"
)

// fpRec is a 128-bit fingerprint or hash and the position of the node that holds it.
type fpRec struct {
	hi, lo uint64
	pos    uint32
}

func fpAt(b []byte) (uint64, uint64) {
	return binary.LittleEndian.Uint64(b), binary.LittleEndian.Uint64(b[8:])
}

func cmpRec(a, b fpRec) int {
	if c := cmp.Compare(a.hi, b.hi); c != 0 {
		return c
	}
	if c := cmp.Compare(a.lo, b.lo); c != 0 {
		return c
	}
	return cmp.Compare(a.pos, b.pos)
}

// sortRecs sorts r by fingerprint, then position. It splits r in place by the top byte, then sorts
// the buckets on the given number of workers. It allocates no copy of r.
func sortRecs(r []fpRec, workers int) {
	var start, next [257]int
	for _, x := range r {
		start[x.hi>>56+1]++
	}
	for b := 1; b <= 256; b++ {
		start[b] += start[b-1]
	}
	copy(next[:], start[:])
	for b := range 256 {
		for next[b] < start[b+1] {
			d := int(r[next[b]].hi >> 56)
			if d == b {
				next[b]++
				continue
			}
			r[next[b]], r[next[d]] = r[next[d]], r[next[b]]
			next[d]++
		}
	}
	jobs := make(chan int, 256)
	for b := range 256 {
		jobs <- b
	}
	close(jobs)
	var wg sync.WaitGroup
	for range max(workers, 1) {
		wg.Go(func() {
			for b := range jobs {
				slices.SortFunc(r[start[b]:start[b+1]], cmpRec)
			}
		})
	}
	wg.Wait()
}

// groups calls f for each run of equal fingerprints in sorted r. The first record of a run is the
// earliest position.
func groups(r []fpRec, f func(run []fpRec)) {
	for i := 0; i < len(r); {
		j := i + 1
		for j < len(r) && r[j].hi == r[i].hi && r[j].lo == r[i].lo {
			j++
		}
		f(r[i:j])
		i = j
	}
}

const firstUse = 1 << 31

// fpIDs numbers the distinct fingerprints of sorted r in the order of their first use. ids must
// hold one slot per position. After the call, ids[p] is the number of the fingerprint at p, with
// firstUse set when p is its first use. It returns the distinct count and the largest run.
func fpIDs(r []fpRec, ids []uint32) (distinct, largest int) {
	groups(r, func(run []fpRec) {
		distinct++
		largest = max(largest, len(run))
		for _, x := range run {
			ids[x.pos] = run[0].pos
		}
	})
	c := uint32(0)
	for p, first := range ids {
		if int(first) == p {
			ids[p] = c | firstUse
			c++
		} else {
			ids[p] = ids[first] &^ firstUse
		}
	}
	return distinct, largest
}
