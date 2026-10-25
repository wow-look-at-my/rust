// edgeList is the edge list of one node.

package main

import (
	"bytes"
	"encoding/binary"
	"errors"
	"fmt"

	mmap "github.com/wow-look-at-my/go-mmap"
)

// edgeList is the edge list of one node. It reads targets either from fixed-width bytes of the file or
// from ids.
type edgeList struct {
	raw   []byte
	width int
	ids   []uint32
	n     int
}

func (l edgeList) at(j int) uint32 {
	if l.ids != nil {
		return l.ids[j]
	}
	o := j * l.width
	switch l.width {
	case 1:
		return uint32(l.raw[o])
	case 2:
		return uint32(binary.LittleEndian.Uint16(l.raw[o:]))
	case 3:
		return uint32(l.raw[o]) | uint32(l.raw[o+1])<<8 | uint32(l.raw[o+2])<<16
	default:
		return binary.LittleEndian.Uint32(l.raw[o:])
	}
}

// dag is the node and edge view that collapsePar reads. A position is the place of a node in the
// file. An edge names a target by its index.
type dag interface {
	nodes() int
	indexSpace() int
	index(p int) uint32
	edges(p int) edgeList
	position(t uint32) int
}

// cgraph is a dep graph read in place from a mapped file. Per node it holds only the file offset.
type cgraph struct {
	b        []byte
	start    int
	kindBits int
	nodeMax  uint64
	off      []uint64
	pos      []int32
	edgeSum  uint64
	tail     []byte
	numKinds int
}

func (g *cgraph) nodes() int             { return len(g.off) }
func (g *cgraph) indexSpace() int        { return int(g.nodeMax) }
func (g *cgraph) head(p int) uint16      { return binary.LittleEndian.Uint16(g.b[g.off[p]:]) }
func (g *cgraph) kind(p int) uint16      { return g.head(p) & uint16(1<<g.kindBits-1) }
func (g *cgraph) index(p int) uint32     { return binary.LittleEndian.Uint32(g.b[g.off[p]+2:]) }
func (g *cgraph) key(p int) (k [16]byte) { copy(k[:], g.b[g.off[p]+6:]); return }
func (g *cgraph) val(p int) (v [16]byte) { copy(v[:], g.b[g.off[p]+22:]); return }

func (g *cgraph) position(t uint32) int {
	if uint64(t) >= g.nodeMax {
		return -1
	}
	return int(g.pos[t])
}

// edgesAt decodes the edge count of node p. It returns the list in place and the offset where the
// list starts.
func (g *cgraph) edgesAt(p int) (edgeList, int) {
	o := int(g.off[p])
	h := binary.LittleEndian.Uint16(g.b[o:])
	width := int((h>>g.kindBits)&3) + 1
	l := int(h >> (g.kindBits + 2))
	o += 38
	if l == 0 {
		v, np, _ := leb(g.b, o)
		l, o = int(v), np
	} else {
		l--
	}
	return edgeList{raw: g.b[o : o+l*width], width: width, n: l}, o
}

func (g *cgraph) edges(p int) edgeList {
	e, _ := g.edgesAt(p)
	return e
}

// nodeBytes is the size of node p in the current 38-byte layout.
func (g *cgraph) nodeBytes(p int) int {
	e, o := g.edgesAt(p)
	return o - int(g.off[p]) + len(e.raw)
}

// parseCompact indexes the node stream of b, which must stay mapped while the graph is used.
func parseCompact(b []byte) (*cgraph, error) {
	if len(b) < 7 || len(b) < 7+int(b[6])+8+24 {
		return nil, errors.New("too short")
	}
	var errs []error
	for kb := 7; kb <= 12; kb++ {
		g, err := parseCompactBits(b, kb)
		if err == nil {
			return g, nil
		}
		errs = append(errs, fmt.Errorf("kind bits %d: %w", kb, err))
	}
	return nil, errors.Join(errs...)
}

func parseCompactBits(b []byte, kindBits int) (*cgraph, error) {
	start := 7 + int(b[6]) + 8
	tail := b[len(b)-24:]
	nodeMax := binary.LittleEndian.Uint64(tail[0:])
	nodeCount := binary.LittleEndian.Uint64(tail[8:])
	edgeCount := binary.LittleEndian.Uint64(tail[16:])
	end := len(b) - 24
	if nodeCount > uint64(end) || nodeMax > uint64(end) {
		return nil, errors.New("bad counts")
	}
	g := &cgraph{b: b, start: start, kindBits: kindBits, nodeMax: nodeMax}
	g.off = make([]uint64, 0, nodeCount)
	g.pos = make([]int32, nodeMax)
	for i := range g.pos {
		g.pos[i] = -1
	}
	kmask := uint16(1<<kindBits - 1)
	p := start
	for i := uint64(0); i < nodeCount; i++ {
		if p+38 > end {
			return nil, errors.New("node past end")
		}
		h := binary.LittleEndian.Uint16(b[p:])
		if h&kmask == 0 {
			return nil, errors.New("null kind")
		}
		ix := binary.LittleEndian.Uint32(b[p+2:])
		if uint64(ix) >= nodeMax || g.pos[ix] >= 0 {
			return nil, errors.New("bad index")
		}
		g.pos[ix] = int32(len(g.off))
		g.off = append(g.off, uint64(p))
		width := int((h>>kindBits)&3) + 1
		l := uint64(h) >> (kindBits + 2)
		q := p + 38
		if l == 0 {
			v, np, err := leb(b, q)
			if err != nil {
				return nil, err
			}
			l, q = v, np
		} else {
			l--
		}
		if l > uint64(end) || q+int(l)*width > end {
			return nil, errors.New("edges past end")
		}
		g.edgeSum += l
		p = q + int(l)*width
	}
	if g.edgeSum != edgeCount {
		return nil, errors.New("edge count mismatch")
	}
	var nums []uint64
	for q := p; q < end; {
		v, np, err := leb(b, q)
		if err != nil {
			return nil, err
		}
		nums = append(nums, v)
		q = np
	}
	if len(nums) < 2 {
		return nil, errors.New("no kind table")
	}
	var sum uint64
	for _, v := range nums[:len(nums)-1] {
		sum += v
	}
	if sum != nodeCount {
		return nil, errors.New("kind count mismatch")
	}
	g.numKinds = len(nums) - 1
	g.tail = b[p:]
	return g, nil
}

// mapFile maps path read-only and returns the mapping and its bytes without the end magic.
func mapFile(path string) (mmap.MMap, []byte, error) {
	m, err := mmap.MapFile(path)
	if err != nil {
		return nil, nil, fmt.Errorf("%s: %w", path, err)
	}
	return m, bytes.TrimSuffix([]byte(m), endMagic), nil
}
