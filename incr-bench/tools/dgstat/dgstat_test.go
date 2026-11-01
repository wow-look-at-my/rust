package main

import _ "embed"

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/stretchr/testify/require"
)

const testKinds = 10

func header(w *bytes.Buffer) {
	w.WriteString("RSIC")
	w.Write([]byte{0, 0, 3})
	w.WriteString("abc")
}

func putNode(w *bytes.Buffer, kind uint16, index uint32, key, val byte, edges []byte, spill bool) {
	head := kind | 0<<9
	if !spill {
		head |= uint16(len(edges)+1) << 11
	}
	var h [38]byte
	binary.LittleEndian.PutUint16(h[:], head)
	binary.LittleEndian.PutUint32(h[2:], index)
	h[6] = key
	h[22] = val
	w.Write(h[:])
	if spill {
		putLeb(w, uint64(len(edges)))
	}
	w.Write(edges)
}

func testGraph() []byte { return testGraphWith(true) }

// testGraphWith builds the test graph. spill puts the last node's edge count after its header,
// which rustc does only for a count that the header cannot hold.
func testGraphWith(spill bool) []byte {
	var w bytes.Buffer
	header(&w)
	w.Write(make([]byte, 8))
	putNode(&w, 1, 0, 1, 0, nil, false)
	putNode(&w, 2, 1, 1, 5, []byte{0}, false)
	putNode(&w, 2, 2, 2, 5, []byte{0, 1}, false)
	putNode(&w, 3, 4, 3, 6, []byte{0, 1}, spill)
	counts := make([]uint64, testKinds)
	counts[1], counts[2], counts[3] = 1, 2, 1
	for _, c := range counts {
		putLeb(&w, c)
	}
	putLeb(&w, 7)
	for _, v := range []uint64{5, 4, 5} {
		binary.Write(&w, binary.LittleEndian, v)
	}
	return w.Bytes()
}

func testQueryCache() []byte {
	var w bytes.Buffer
	header(&w)
	entry := func(idx uint64, payload int) uint64 {
		pos := uint64(w.Len())
		putLeb(&w, idx)
		w.Write(make([]byte, payload))
		putLeb(&w, uint64(payload+1))
		return pos
	}
	v1 := entry(1, 10)
	v2 := entry(2, 20)
	s1 := entry(4, 3)
	alloc := uint64(w.Len())
	w.Write(make([]byte, 7))
	hyg := uint64(w.Len())
	w.Write(make([]byte, 5))
	footer := uint64(w.Len())
	putLeb(&w, 0xC0FFEE)
	putLeb(&w, 1)
	putLeb(&w, 0)
	w.Write(make([]byte, 24))
	putLeb(&w, 2)
	putLeb(&w, 1)
	putLeb(&w, v1)
	putLeb(&w, 2)
	putLeb(&w, v2)
	putLeb(&w, 1)
	putLeb(&w, 4)
	putLeb(&w, s1)
	putLeb(&w, 1)
	putLeb(&w, alloc)
	putLeb(&w, 1)
	putLeb(&w, 0)
	putLeb(&w, hyg)
	putLeb(&w, 0)
	putLeb(&w, 1)
	w.Write(make([]byte, 16))
	putLeb(&w, 3)
	putLeb(&w, 99)
	binary.Write(&w, binary.LittleEndian, footer)
	return w.Bytes()
}

func TestRun(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "crate", "s-1")
	require.NoError(t, os.MkdirAll(dir, 0o755))
	dg := filepath.Join(dir, "dep-graph.bin")
	require.NoError(t, os.WriteFile(dg, testGraph(), 0o644))
	require.NoError(t, os.WriteFile(filepath.Join(dir, "query-cache.bin"), testQueryCache(), 0o644))
	names := make([]string, testKinds)
	for i := range names {
		names[i] = "k" + string(rune('a'+i))
	}
	kinds := filepath.Join(t.TempDir(), "kinds.txt")
	require.NoError(t, os.WriteFile(kinds, []byte(strings.Join(names, "\n")), 0o644))

	var out bytes.Buffer
	require.NoError(t, run(&out, []string{kinds, dg}, 2))
	s := out.String()
	require.Contains(t, s, "nodes 4 (index space 5), edges 5")
	require.Contains(t, s, "names mapped")
	require.Contains(t, s, "query values 34")
	require.Contains(t, s, "kc ")

	// A session without a query cache still reports the graph.
	require.NoError(t, os.Remove(filepath.Join(dir, "query-cache.bin")))
	out.Reset()
	require.NoError(t, run(&out, []string{kinds, dg}, 2))
	require.Contains(t, out.String(), "(no query-cache.bin)")
}

func TestErrors(t *testing.T) {
	require.Error(t, run(&bytes.Buffer{}, nil, 1))
	require.Error(t, run(&bytes.Buffer{}, []string{"/nonexistent", "x"}, 1))
	require.ErrorContains(t, run(&bytes.Buffer{}, []string{"k", "x"}, 0), "--workers")
	g := testGraph()
	_, err := parseCompact(g[:len(g)-30])
	require.Error(t, err)
	_, err = parseCompact([]byte("RSIC"))
	require.Error(t, err)
	_, _, err = leb([]byte{0x80}, 0)
	require.Error(t, err)
	gr, err := parseCompact(g)
	require.NoError(t, err)
	require.Error(t, analyzeQueryCache(&bytes.Buffer{}, nil, "q", []byte("short"), gr))
	q := testQueryCache()
	require.Error(t, analyzeQueryCache(&bytes.Buffer{}, nil, "q", q[:len(q)-9], gr))
	require.Equal(t, "kind#3", kindNamer{nil, 4}.name(3))
}

func TestCollapse(t *testing.T) {
	cases := []struct {
		name  string
		edges [][]uint32
		keep  []bool
		want  [][]uint32
		max   int
	}{
		{"chain", [][]uint32{nil, {0}, {1}, {2}}, []bool{true, false, false, true}, [][]uint32{{}, nil, nil, {0}}, 1},
		{"diamond", [][]uint32{nil, {0}, {0}, {1, 2}}, []bool{true, false, false, true}, [][]uint32{{}, nil, nil, {0}}, 1},
		{"first-visit order", [][]uint32{nil, {3, 0}, {0, 5}, nil, {1, 2}, nil}, []bool{true, false, false, true, true, true},
			[][]uint32{{}, nil, nil, {}, {3, 0, 5}, {}}, 2},
		{"dedupe of a kept edge and a spliced edge", [][]uint32{nil, {0}, {0, 1, 0}}, []bool{true, false, true}, [][]uint32{{}, nil, {0}}, 1},
		{"kept node with only dropped deps", [][]uint32{nil, {}, {1, 0}, {2}}, []bool{false, false, false, true}, [][]uint32{nil, nil, nil, {}}, 0},
		{"edges to higher indices", [][]uint32{{2}, nil, {1}}, []bool{true, true, false}, [][]uint32{{1}, {}, nil}, 1},
	}
	cases = append(cases, struct {
		name  string
		edges [][]uint32
		keep  []bool
		want  [][]uint32
		max   int
	}{"shared dropped child", [][]uint32{nil, {3, 2, 0}, {0, 5}, nil, nil, nil, {1, 2, 4}},
		[]bool{true, false, false, true, true, true, true}, [][]uint32{{}, nil, nil, {}, {}, {}, {3, 0, 5, 4}}, 3})
	for _, c := range cases {
		// A memo limit of a value of zero makes every closure big, so build walks it again at each use.
		for _, memo := range []int{memoMax, 0} {
			t.Run(fmt.Sprintf("%s/memo=%d", c.name, memo), func(t *testing.T) {
				index := make([]uint32, len(c.edges))
				for i := range index {
					index[i] = uint32(i)
				}
				got, st, err := collapse(index, c.edges, c.keep, len(index), defaultLiveCap, memo)
				require.NoError(t, err)
				require.Equal(t, c.want, got)
				require.Equal(t, c.max, st.maxClosure)
				require.Zero(t, st.missing)
			})
		}
	}

	// Positions and indices differ, and one target holds no node.
	got, st, err := collapse([]uint32{5, 2, 0}, [][]uint32{{2, 7}, {0}, nil}, []bool{true, false, true}, 8, defaultLiveCap, memoMax)
	require.NoError(t, err)
	require.Equal(t, [][]uint32{{0, 7}, nil, {}}, got)
	require.Equal(t, 1, st.missing)

	// A chain frees each closure as soon as its only user has built its own list.
	_, st, err = collapse([]uint32{0, 1, 2, 3, 4}, [][]uint32{nil, {0}, {1}, {2}, {3}}, []bool{true, false, false, false, true}, 5, defaultLiveCap, memoMax)
	require.NoError(t, err)
	require.Equal(t, 1, st.peakLive)
	require.Equal(t, 3, st.closureEntries)

	_, _, err = collapse([]uint32{0, 1, 2, 3}, [][]uint32{nil, nil, {0, 1}, {2}}, []bool{true, true, false, true}, 4, 1, memoMax)
	require.ErrorContains(t, err, "live closures hold more than 1 entries")

	_, _, err = collapse([]uint32{0, 1, 2}, [][]uint32{{1}, {2}, {1}}, []bool{true, false, false}, 3, defaultLiveCap, memoMax)
	require.ErrorContains(t, err, "cycle")
}

func TestSlebLen(t *testing.T) {
	for v, n := range map[int64]int{0: 1, 63: 1, 64: 2, -64: 1, -65: 2, 8191: 2, 8192: 3, -1 << 40: 6} {
		require.Equal(t, n, slebLen(v), "%d", v)
	}
	require.Equal(t, 1, edgeWidth(0))
	require.Equal(t, 2, edgeWidth(256))
	require.Equal(t, 4, edgeWidth(1<<24))
}

//go:embed testdata/test-queries.cpp
var testQueries string

func TestParseKinds(t *testing.T) {
	defs, err := parseKinds(testQueries)
	require.NoError(t, err)
	require.Equal(t, []string{"Null", "Red", "TraitSelect", "a", "b", "c"}, kindNames(defs))
	require.False(t, defs[3].cacheOnDisk || defs[3].evalAlways)
	require.True(t, defs[4].cacheOnDisk)
	require.True(t, defs[5].evalAlways)
	_, err = parseKinds("    query x(_: ()) {\n")
	require.ErrorContains(t, err, "no closing brace")
	_, err = parseKinds("")
	require.Error(t, err)
}

func TestCollapseCommand(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "crate", "s-1")
	require.NoError(t, os.MkdirAll(dir, 0o755))
	dg := filepath.Join(dir, "dep-graph.bin")
	require.NoError(t, os.WriteFile(dg, append(testGraphWith(false), endMagic...), 0o644))
	// One kind is TraitSelect, a later kind is a plain query and a later kind is cache_on_disk.
	src := testCollapseCommandCpp
	for i := 4; i < testKinds; i++ {
		src += "    query q" + string(rune('0'+i)) + "(_: ()) {\n    }\n"
	}
	q := filepath.Join(t.TempDir(), "queries.rs")
	require.NoError(t, os.WriteFile(q, []byte(src), 0o644))

	var out bytes.Buffer
	rootCmd.SetOut(&out)
	rootCmd.SetArgs([]string{"collapse", q, dg})
	require.NoError(t, rootCmd.Execute())
	s := out.String()
	require.NotContains(t, s, "SANITY FAIL")
	// P1 keeps the first index (no edges) and a later index (cache_on_disk).
	require.Contains(t, s, "| P1 non_query but Null + cache_on_disk/eval_always queries + nodes with 0 edges | 2 (50.0%) | 1 (20.0%) | 1 |")
	require.Contains(t, s, "| P0 keep everything | 4 (100.0%) | 5 (100.0%) |")
	require.Contains(t, s, "local (top value)")

	out.Reset()
	rootCmd.SetArgs([]string{"kinds", q})
	require.NoError(t, rootCmd.Execute())
	require.True(t, strings.HasPrefix(out.String(), "Null\nTraitSelect\nq2\n"))

	// A kind list that does not fit the trailer is an error.
	require.NoError(t, os.WriteFile(q, []byte(testCollapseCommandCpp2), 0o644))
	rootCmd.SetArgs([]string{"collapse", q, dg})
	require.ErrorContains(t, rootCmd.Execute(), "UNMAPPED")
}

//go:embed testdata/test-collapse-command.cpp
var testCollapseCommandCpp string

//go:embed testdata/test-collapse-command-2.cpp
var testCollapseCommandCpp2 string
