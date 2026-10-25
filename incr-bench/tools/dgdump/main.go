// dgdump prints sample nodes of a rustc dep-graph.bin in the upstream 38-byte node layout.
// Kind, index, key and value fingerprints, and each edge with the kind of its target.
package main

import (
	"bufio"
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"io"
	"os"

	"github.com/wow-look-at-my/go-containers/set"
	"strconv"
	"strings"
)

type node struct {
	kind     uint16
	key      []byte
	value    []byte
	edgesOff int
	width    int
	nEdges   int
}

func readKinds(path string) []string {
	f, err := os.Open(path)
	if err != nil {
		panic(err)
	}
	defer f.Close()
	var names []string
	s := bufio.NewScanner(f)
	for s.Scan() {
		names = append(names, strings.TrimSpace(s.Text()))
	}
	return names
}

func leb(b []byte, pos int) (uint64, int) {
	var v uint64
	var shift uint
	for {
		c := b[pos]
		pos++
		v |= uint64(c&0x7f) << shift
		if c < 0x80 {
			return v, pos
		}
		shift += 7
	}
}

func main() {
	if len(os.Args) < 5 {
		fmt.Fprintln(os.Stderr, "usage: dgdump KINDS.txt DEP-GRAPH.bin SAMPLES-PER-KIND KIND[,KIND...]")
		os.Exit(2)
	}
	data, err := os.ReadFile(os.Args[2])
	if err != nil {
		panic(err)
	}
	perKind, err := strconv.Atoi(os.Args[3])
	if err != nil {
		panic(err)
	}
	dump(os.Stdout, readKinds(os.Args[1]), data, perKind, strings.Split(os.Args[4], ","))
}

func dump(out io.Writer, names []string, data []byte, perKind int, kinds []string) {
	want := set.New[string]()
	for _, k := range kinds {
		want.Add(k)
	}

	kindBits := 16 - leadingZeros16(uint16(len(names)-1))
	const widthBits = 2

	// Trailer: u64 counts before the 13-byte end magic.
	end := len(data) - len("rust-end-file")
	nodeMax := int(binary.LittleEndian.Uint64(data[end-24:]))
	nodeCount := int(binary.LittleEndian.Uint64(data[end-16:]))

	// File header: magic, u16 format version, u8 length, version string, then an 8-byte options hash.
	pos := 4 + 2
	vlen := int(data[pos])
	pos += 1 + vlen + 8

	nodes := make([]node, nodeMax)
	order := make([]int, 0, nodeCount)
	for i := 0; i < nodeCount; i++ {
		head := binary.LittleEndian.Uint16(data[pos:])
		index := int(binary.LittleEndian.Uint32(data[pos+2:]))
		n := node{
			kind:  head & (1<<kindBits - 1),
			key:   data[pos+6 : pos+22],
			value: data[pos+22 : pos+38],
		}
		n.width = int((head>>kindBits)&(1<<widthBits-1)) + 1
		inline := int(head >> (kindBits + widthBits))
		pos += 38
		if inline == 0 {
			var c uint64
			c, pos = leb(data, pos)
			n.nEdges = int(c)
		} else {
			n.nEdges = inline - 1
		}
		n.edgesOff = pos
		pos += n.width * n.nEdges
		nodes[index] = n
		order = append(order, index)
	}

	edge := func(n node, j int) int {
		var buf [4]byte
		copy(buf[:], data[n.edgesOff+j*n.width:n.edgesOff+(j+1)*n.width])
		return int(binary.LittleEndian.Uint32(buf[:]))
	}

	shown := map[uint16]int{}
	for _, idx := range order {
		n := nodes[idx]
		name := names[n.kind]
		if !want.Contains(name) || shown[n.kind] >= perKind {
			continue
		}
		shown[n.kind]++
		fmt.Fprintf(out, "#%d %s(key=%s) value=%s edges=%d\n", idx, name, hex.EncodeToString(n.key), hex.EncodeToString(n.value), n.nEdges)
		limit := n.nEdges
		if limit > 12 {
			limit = 12
		}
		for j := 0; j < limit; j++ {
			t := edge(n, j)
			fmt.Fprintf(out, "    -> #%d %s(key=%s)\n", t, names[nodes[t].kind], hex.EncodeToString(nodes[t].key)[:16])
		}
		if n.nEdges > limit {
			fmt.Fprintf(out, "    ... %d more\n", n.nEdges-limit)
		}
	}
}

func leadingZeros16(v uint16) int {
	n := 0
	for i := 15; i >= 0; i-- {
		if v&(1<<uint(i)) != 0 {
			break
		}
		n++
	}
	return n
}
