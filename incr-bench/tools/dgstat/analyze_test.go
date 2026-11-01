package main

import (
	"bufio"
	"bytes"
	"encoding/binary"
	"fmt"
	"math/rand/v2"
	"slices"
	"sort"
	"strings"
	"testing"

	"github.com/stretchr/testify/require"
	"github.com/wow-look-at-my/go-containers/set"
)

type refNode struct {
	kind     uint16
	width    int
	index    uint32
	key, val [16]byte
	edges    []uint32
	spill    bool
}

func buildGraph(nodes []refNode, nodeMax int) []byte {
	var w bytes.Buffer
	header(&w)
	w.Write(make([]byte, 8))
	counts := make([]uint64, testKinds)
	edges := 0
	for _, n := range nodes {
		head := n.kind | uint16(n.width-1)<<9
		if !n.spill {
			head |= uint16(len(n.edges)+1) << 11
		}
		var h [38]byte
		binary.LittleEndian.PutUint16(h[:], head)
		binary.LittleEndian.PutUint32(h[2:], n.index)
		copy(h[6:], n.key[:])
		copy(h[22:], n.val[:])
		w.Write(h[:])
		if n.spill {
			putLeb(&w, uint64(len(n.edges)))
		}
		for _, e := range n.edges {
			var t [4]byte
			binary.LittleEndian.PutUint32(t[:], e)
			w.Write(t[:n.width])
		}
		counts[n.kind]++
		edges += len(n.edges)
	}
	for _, c := range counts {
		putLeb(&w, c)
	}
	putLeb(&w, 3)
	for _, v := range []int{nodeMax, len(nodes), edges} {
		binary.Write(&w, binary.LittleEndian, uint64(v))
	}
	return w.Bytes()
}

func randomGraph(r *rand.Rand, n int) ([]refNode, int) {
	fps := make([][16]byte, 40)
	for i := range fps {
		if i > 0 {
			binary.LittleEndian.PutUint64(fps[i][:], r.Uint64())
			binary.LittleEndian.PutUint64(fps[i][8:], r.Uint64())
		}
	}
	// Some fingerprints share the top byte, so one sort bucket holds more than one run.
	fps[2][7] = fps[3][7]
	nodes := make([]refNode, n)
	ix := uint32(0)
	var lists [][]uint32
	for i := range nodes {
		nd := &nodes[i]
		nd.kind = uint16(1 + r.IntN(testKinds-1))
		nd.width = 1 + r.IntN(4)
		if r.IntN(4) == 0 {
			ix += uint32(1 + r.IntN(300))
		}
		nd.index = ix
		ix++
		nd.key = fps[r.IntN(len(fps))]
		nd.val = fps[r.IntN(len(fps)/2)]
		if len(lists) > 0 && r.IntN(5) == 0 {
			// A repeat of an earlier list, at the widest width so every target fits.
			nd.edges = lists[r.IntN(len(lists))]
			nd.width = 4
		} else {
			n := r.IntN(12)
			if r.IntN(30) == 0 {
				n = 30 + r.IntN(5000)
			}
			nd.edges = make([]uint32, n)
			for j := range nd.edges {
				nd.edges[j] = uint32(r.Uint64N(min(uint64(1)<<(8*nd.width), 1<<20)))
			}
			lists = append(lists, nd.edges)
		}
		nd.spill = len(nd.edges)+1 >= 32 || r.IntN(10) == 0
	}
	return nodes, int(ix) + 7
}

// refDense is a direct port of layout B with maps, kept as the reference for writeDense.
func refDense(file []byte, start int, nodes []refNode, tail []byte) []byte {
	var w bytes.Buffer
	w.Write(file[:start])
	keys := map[[16]byte]uint64{}
	vals := map[[16]byte]uint64{}
	prev := uint32(1<<32 - 1)
	for _, n := range nodes {
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
		head := n.kind | uint16(n.width-1)<<9
		if !n.spill {
			head |= uint16(len(n.edges)+1) << 11
		}
		binary.Write(&w, binary.LittleEndian, head)
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
		if n.spill {
			putLeb(&w, uint64(len(n.edges)))
		}
		p := int64(n.index)
		for _, e := range n.edges {
			putLeb(&w, zigzag(int64(e)-p))
			p = int64(e)
		}
		prev = n.index
	}
	w.Write(tail)
	return w.Bytes()
}

func TestMeasureMatchesReference(t *testing.T) {
	r := rand.New(rand.NewPCG(1, 2))
	nodes, nodeMax := randomGraph(r, 3000)
	data := buildGraph(nodes, nodeMax)
	g, err := parseCompact(data)
	require.NoError(t, err)

	keys := set.New[[16]byte]()
	vals := map[[16]byte]int{}
	lists := set.New[string]()
	var ec []int
	var zero [16]byte
	dups, dupBytes, zeroVal, edges := 0, 0, 0, 0
	for _, n := range nodes {
		keys.Add(n.key)
		vals[n.val]++
		if n.val == zero {
			zeroVal++
		}
		ec = append(ec, len(n.edges))
		edges += len(n.edges)
		if len(n.edges) > 1 {
			k := fmt.Sprint(n.edges)
			if lists.Contains(k) {
				dups++
				dupBytes += n.width * len(n.edges)
			}
			lists.Add(k)
		}
	}
	maxVal := 0
	for _, c := range vals {
		maxVal = max(maxVal, c)
	}
	sort.Ints(ec)
	require.Positive(t, dups)

	for _, workers := range []int{1, 3, 8} {
		t.Run(fmt.Sprint("workers=", workers), func(t *testing.T) {
			s, sz, err := measureGraph(g, workers)
			require.NoError(t, err)
			require.Equal(t, len(nodes), s.nodes)
			require.Equal(t, edges, s.edges)
			require.Equal(t, keys.Len(), s.distinctKeys)
			require.Equal(t, len(vals), s.distinctVals)
			require.Equal(t, maxVal, s.maxVal)
			require.Equal(t, zeroVal, s.zeroVal)
			require.Equal(t, dups, s.dupLists)
			require.Equal(t, dupBytes, s.dupListByte)
			for _, rank := range []int{0, len(ec) / 2, len(ec) * 90 / 100, len(ec) - 1} {
				require.Equal(t, ec[rank], s.edgeCountAt(rank), "rank %d", rank)
			}
			want := refDense(data, g.start, nodes, g.tail)
			require.Equal(t, len(want), sz.dense)

			keyID, valID := make([]uint32, len(nodes)), make([]uint32, len(nodes))
			rec := make([]fpRec, len(nodes))
			fpStats(g, 6, rec, keyID, workers)
			fpStats(g, 22, rec, valID, workers)
			var got bytes.Buffer
			require.NoError(t, writeDense(bufio.NewWriter(&got), g, keyID, valID))
			require.Equal(t, want, got.Bytes())
			require.Positive(t, sz.dense1)
			require.Positive(t, sz.plain3)
		})
	}

	var out bytes.Buffer
	require.NoError(t, analyzeGraph(&out, nil, "a/b/dep-graph.bin", g, 2))
	require.Contains(t, out.String(), fmt.Sprintf("nodes %d (index space %d), edges %d", len(nodes), nodeMax, edges))
	require.Contains(t, out.String(), "names UNMAPPED")
	require.True(t, strings.HasSuffix(out.String(), "x\n"))
}

func TestSortRecs(t *testing.T) {
	r := rand.New(rand.NewPCG(3, 4))
	recs := make([]fpRec, 5000)
	for i := range recs {
		recs[i] = fpRec{r.Uint64() >> uint(i%16), r.Uint64N(4), uint32(r.IntN(100))}
	}
	want := slices.Clone(recs)
	slices.SortFunc(want, cmpRec)
	sortRecs(recs, 3)
	require.Equal(t, want, recs)
}

func TestFPIDs(t *testing.T) {
	recs := []fpRec{{1, 0, 0}, {2, 0, 1}, {1, 0, 2}, {3, 0, 3}, {2, 0, 4}}
	sortRecs(recs, 2)
	ids := make([]uint32, 5)
	d, l := fpIDs(recs, ids)
	require.Equal(t, 3, d)
	require.Equal(t, 2, l)
	require.Equal(t, []uint32{0 | firstUse, 1 | firstUse, 0, 2 | firstUse, 1}, ids)
}
