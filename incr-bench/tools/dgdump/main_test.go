package main

import (
	"bytes"
	"encoding/binary"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// Kind bits for a list of names: DepKind::MAX = 3 needs a couple of bits.
const testKindBits = 2

func head(kind, width, inlineLen int) []byte {
	h := uint16(kind) | uint16(width-1)<<testKindBits | uint16(inlineLen)<<(testKindBits+2)
	return binary.LittleEndian.AppendUint16(nil, h)
}

func nodeBytes(kind, index int, key, value byte, edges []int, spill bool) []byte {
	var b []byte
	inline := len(edges) + 1
	if spill {
		inline = 0
	}
	b = append(b, head(kind, 1, inline)...)
	b = binary.LittleEndian.AppendUint32(b, uint32(index))
	b = append(b, bytes.Repeat([]byte{key}, 16)...)
	b = append(b, bytes.Repeat([]byte{value}, 16)...)
	if spill {
		b = append(b, byte(len(edges)))
	}
	for _, e := range edges {
		b = append(b, byte(e))
	}
	return b
}

func graphFile(nodes [][]byte, nodeMax int) []byte {
	b := []byte("RSIC")
	b = append(b, 0, 0, 1, 'v')
	b = append(b, make([]byte, 8)...)
	for _, n := range nodes {
		b = append(b, n...)
	}
	b = binary.LittleEndian.AppendUint64(b, uint64(nodeMax))
	b = binary.LittleEndian.AppendUint64(b, uint64(len(nodes)))
	b = binary.LittleEndian.AppendUint64(b, 0)
	return append(b, "rust-end-file"...)
}

func TestDump(t *testing.T) {
	many := make([]int, 14)
	data := graphFile([][]byte{
		nodeBytes(1, 0, 0xaa, 0x11, nil, false),
		nodeBytes(2, 1, 0xbb, 0x22, []int{0}, false),
		nodeBytes(2, 2, 0xcc, 0x33, many, true),
		nodeBytes(3, 3, 0xdd, 0x44, []int{1}, false),
	}, 4)
	var out bytes.Buffer
	dump(&out, []string{"Null", "hir_owner", "layout_of", "typeck"}, data, 5, []string{"layout_of"})
	got := out.String()

	for _, want := range []string{
		"#1 layout_of(key=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb) value=22222222222222222222222222222222 edges=1",
		"    -> #0 hir_owner(key=aaaaaaaaaaaaaaaa)",
		"#2 layout_of(key=cccccccccccccccccccccccccccccccc) value=33333333333333333333333333333333 edges=14",
		"    ... 2 more",
	} {
		assert.Contains(t, got, want)

	}
	assert.NotContains(t, got, "typeck(")

	out.Reset()
	dump(&out, []string{"Null", "hir_owner", "layout_of", "typeck"}, data, 1, []string{"layout_of"})
	n := strings.Count(out.String(), " layout_of(")
	assert.Equal(t, 1, n)

}

func TestReadKinds(t *testing.T) {
	path := filepath.Join(t.TempDir(), "kinds.txt")
	require.NoError(t, os.WriteFile(path, []byte("Null\n layout_of \n"), 0o644))

	got := readKinds(path)
	assert.False(t, len(got) != 2 || got[1] != "layout_of")

}

func TestLebAndLeadingZeros(t *testing.T) {
	v, pos := leb([]byte{0x96, 0x01, 0x05}, 0)
	assert.False(t, v != 150 || pos != 2)

	got := leadingZeros16(3)
	assert.Equal(t, 14, got)

	got = leadingZeros16(0)
	assert.Equal(t, 16, got)

}
