//! The data that we will serialize and deserialize.
//!
//! Notionally, the dep-graph is a sequence of NodeInfo with the dependencies
//! specified inline. After the nodes come the per-kind node counts and the session count as leb128, then the node index bound. The node count and the edge count as fixed 8-byte integers. Nothing is aligned or padded on disk.
//!
//! The serialisation is performed on-demand when each node is emitted. Using this
//! scheme, we do not need to keep the current graph in memory.
//!
//! The deserialization is performed manually, in order to convert from the stored
//! sequence of NodeInfos to the different arrays in SerializedDepGraph. Since the
//! node and edge count are stored at the end of the file, all the arrays can be
//! pre-allocated with the right length.
//!
//! Each node record starts with a [`NodeHead`] and a flags byte. The head bit-packs the
//! [`DepKind`], the byte width of the largest edge index, and the edge count when it fits.
//! The flags say which of the optional fields follow: the index, unless it is the previous
//! index plus one. This also covers the 128-bit key fingerprint, or a leb128 distance back to
//! an earlier node with the same key. This also covers the 64-bit result fingerprint, or a
//! distance back, or nothing for zero. This also covers the edge count when the head has no
//! room for it. Each edge is one leb128 code: a zigzag delta from the previous target, or a
//! distance back into the edges of this block.
//!
//! When we decode this data, we do not immediately create [`SerializedDepNodeIndex`] and
//! instead keep the edges at the byte width that the head names. This turns the on-disk
//! size into a peak memory reduction. A fixed-size read of that array masks off the extra
//! bytes. The in-memory edge array ends in padding to permit this. The file has none.
//!
//! Dep-graph indices are bulk allocated to threads inside `LocalEncoderState`. Having threads
//! own these indices helps avoid races when they are conditionally used when marking nodes green.
//! It also reduces congestion on the shared index count.

use std::cell::RefCell;
use std::cmp::max;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::{iter, mem};

use rustc_data_structures::fingerprint::{Fingerprint, PackedFingerprint};
use rustc_data_structures::fx::FxHashMap;
use rustc_data_structures::outline;
use rustc_data_structures::profiling::SelfProfilerRef;
use rustc_data_structures::sync::{AtomicU64, Lock, WorkerLocal, broadcast};
use rustc_data_structures::unhash::UnhashMap;
use rustc_index::{IndexSlice, IndexVec};
use rustc_serialize::opaque::mem_encoder::MemEncoder;
use rustc_serialize::opaque::{FileEncodeResult, FileEncoder, IntEncodedWithFixedSize, MemDecoder};
use rustc_serialize::{Decodable, Decoder, Encodable, Encoder};
use rustc_session::Session;
use tracing::{debug, instrument};

use super::graph::{CurrentDepGraph, DepNodeColorMap, DesiredColor, TrySetColorResult};
use super::retained::RetainedDepGraph;
use super::{DepKind, DepNode, DepNodeIndex};

#[cfg(test)]
mod tests;

// The maximum value of `SerializedDepNodeIndex` leaves the upper two bits
// unused so that we can store multiple index types in `CompressedHybridIndex`,
// and use those bits to encode which index type it contains.
rustc_index::newtype_index! {
    #[encodable]
    #[max = 0x7FFF_FFFF]
    pub struct SerializedDepNodeIndex {}
}

impl SerializedDepNodeIndex {
    /// Converts a current-session dep node index to a "serialized" index,
    /// for the purpose of serializing data to be loaded by future sessions.
    #[inline(always)]
    pub fn from_curr_for_serialization(index: DepNodeIndex) -> Self {
        SerializedDepNodeIndex::from_u32(index.as_u32())
    }
}

const DEP_NODE_SIZE: usize = size_of::<SerializedDepNodeIndex>();
/// Amount of padding we need to add to the edge list data so that we can retrieve every
/// SerializedDepNodeIndex with a fixed-size read then mask.
const DEP_NODE_PAD: usize = DEP_NODE_SIZE - 1;
/// Number of bits we need to store the number of used bytes in a SerializedDepNodeIndex.
/// Note that wherever we encode byte widths like this we actually store the number of bytes used
/// minus 1; for a 4-byte value we technically would have 5 widths to store, but using one byte to
/// store zeroes (which are relatively rare) is a decent tradeoff to save a bit in our bitfields.
const DEP_NODE_WIDTH_BITS: usize = DEP_NODE_SIZE / 2;

/// Data for use when recompiling the **current crate**.
///
/// There may be unused indices with DepKind::Null in this graph due to batch allocation of
/// indices to threads.
#[derive(Default)]
pub struct SerializedDepGraph {
    /// The set of all DepNodes in the graph
    nodes: IndexVec<SerializedDepNodeIndex, DepNode>,
    /// A value fingerprint associated with each [`DepNode`] in [`Self::nodes`],
    /// typically a hash of the value returned by the node's query in the
    /// previous incremental-compilation session.
    ///
    /// Some nodes don't have a meaningful value hash (e.g. queries with `no_hash`),
    /// so they store a dummy value here instead (e.g. [`Fingerprint::ZERO`]).
    value_fingerprints: IndexVec<SerializedDepNodeIndex, Fingerprint>,
    /// For each DepNode, stores the list of edges originating from that
    /// DepNode. Encoded as a [start, end) pair indexing into edge_list_data,
    /// which holds the actual DepNodeIndices of the target nodes.
    edge_list_indices: IndexVec<SerializedDepNodeIndex, EdgeHeader>,
    /// A flattened list of all edge targets in the graph, stored in the same
    /// varint encoding that we use on disk. Edge sources are implicit in edge_list_indices.
    edge_list_data: Vec<u8>,
    /// The lazily-built inverse of `nodes`: maps a [`DepNode`] back to its
    /// [`SerializedDepNodeIndex`] via the node's key fingerprint. See
    /// [`LazyNodeIndex`].
    reverse_index: LazyNodeIndex,
    /// The number of nodes actually encoded, which is below [`Self::index_space_len`]
    /// whenever a thread left part of its batch of indices unused.
    live_node_count: usize,
    /// The number of previous compilation sessions. This is used to generate
    /// unique anon dep nodes per session.
    session_count: u64,
    /// Used to time the lazy per-`DepKind` reverse-index build. `None` only for
    /// the empty default graph, which is never looked up.
    profiler: Option<SelfProfilerRef>,
}

// `SelfProfilerRef` is not `Debug`, so we can't derive this.
impl std::fmt::Debug for SerializedDepGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SerializedDepGraph")
            .field("nodes", &self.nodes)
            .field("value_fingerprints", &self.value_fingerprints)
            .field("edge_list_indices", &self.edge_list_indices)
            .field("edge_list_data", &self.edge_list_data)
            .field("reverse_index", &self.reverse_index)
            .field("live_node_count", &self.live_node_count)
            .field("session_count", &self.session_count)
            .finish_non_exhaustive()
    }
}

/// The inverse of [`SerializedDepGraph::nodes`], built lazily per [`DepKind`].
///
/// Only few nodes are ever looked up here, and those cluster into a handful of
/// `DepKind`s. Building a map for every kind up front would be wasted work.
#[derive(Debug, Default)]
struct LazyNodeIndex {
    /// All (non-`Null`) node indices, grouped into contiguous per-`DepKind`
    /// ranges described by `kinds`. For any non-`Null` `DepKind` `k`, all values in
    /// `nodes_by_kind[kinds[k].start..][..kinds[k].len]`
    /// must be `Some` and have kind `k`.
    nodes_by_kind: Vec<Option<SerializedDepNodeIndex>>,
    /// For each `DepKind`, the range of `nodes_by_kind` holding its node indices
    /// and the lazily-built fingerprint map over that range.
    kinds: Vec<LazyKindIndex>,
}

#[derive(Debug, Default)]
struct LazyKindIndex {
    /// Offset into `LazyNodeIndex::nodes_by_kind` of this kind's first node.
    start: u32,
    /// Number of nodes of this kind.
    len: u32,
    /// `key_fingerprint -> node index`, built from this kind's range on first
    /// lookup. Empty kinds (and kinds never looked up) never build a map.
    map: OnceLock<UnhashMap<PackedFingerprint, SerializedDepNodeIndex>>,
}

impl LazyKindIndex {
    /// Returns this kind's `key_fingerprint -> node index` map.
    fn fingerprint_map(
        &self,
        kind: DepKind,
        nodes: &IndexSlice<SerializedDepNodeIndex, DepNode>,
        nodes_by_kind: &[Option<SerializedDepNodeIndex>],
        profiler: &Option<SelfProfilerRef>,
    ) -> &UnhashMap<PackedFingerprint, SerializedDepNodeIndex> {
        self.map.get_or_init(|| {
            let _prof_timer = profiler
                .as_ref()
                .map(|p| p.generic_activity("incr_comp_load_dep_graph_reverse_index"));
            let range = (self.start as usize)..(self.start as usize + self.len as usize);
            let mut map =
                UnhashMap::with_capacity_and_hasher(self.len as usize, Default::default());
            for &idx in &nodes_by_kind[range] {
                let idx = idx.expect("counting sort fills every slot of a kind's range");
                let node = nodes[idx];
                debug_assert_eq!(node.kind, kind);
                if map.insert(node.key_fingerprint, idx).is_some()
                    // Side effect nodes can legitimately share a fingerprint.
                    && node.kind != DepKind::SideEffect
                {
                    panic!(
                        "Error: A dep graph node ({kind:?}) does not have an unique index. \
                         Running a clean build on a nightly compiler with \
                         `-Z incremental-verify-ich` can help narrow down the issue for reporting. \
                         A clean build may also work around the issue.\n
                         DepNode: {node:?}"
                    )
                }
            }
            map
        })
    }
}

impl SerializedDepGraph {
    #[inline]
    pub fn edge_targets_from(
        &self,
        source: SerializedDepNodeIndex,
    ) -> impl Iterator<Item = SerializedDepNodeIndex> + Clone {
        let header = self.edge_list_indices[source];
        let mut raw = &self.edge_list_data[header.start()..];

        let bytes_per_index = header.bytes_per_index();

        // LLVM doesn't hoist EdgeHeader::mask so we do it ourselves.
        let mask = header.mask();
        (0..header.num_edges).map(move |_| {
            // Doing this slicing in this order ensures that the first bounds check suffices for
            // all the others.
            let index = &raw[..DEP_NODE_SIZE];
            raw = &raw[bytes_per_index..];
            let index = u32::from_le_bytes(index.try_into().unwrap()) & mask;
            SerializedDepNodeIndex::from_u32(index)
        })
    }

    #[inline]
    pub fn index_to_node(&self, dep_node_index: SerializedDepNodeIndex) -> &DepNode {
        &self.nodes[dep_node_index]
    }

    #[inline]
    pub fn node_to_index_opt(&self, dep_node: &DepNode) -> Option<SerializedDepNodeIndex> {
        let kind = self.reverse_index.kinds.get(dep_node.kind.as_usize())?;
        let map = kind.fingerprint_map(
            dep_node.kind,
            &self.nodes,
            &self.reverse_index.nodes_by_kind,
            &self.profiler,
        );
        map.get(&dep_node.key_fingerprint).copied()
    }

    #[inline]
    pub fn value_fingerprint_for_index(
        &self,
        dep_node_index: SerializedDepNodeIndex,
    ) -> Fingerprint {
        self.value_fingerprints[dep_node_index]
    }

    /// The number of dep-node indices, counting those that hold no node.
    #[inline]
    pub fn index_space_len(&self) -> usize {
        self.nodes.len()
    }

    #[inline]
    pub fn live_node_count(&self) -> usize {
        self.live_node_count
    }

    #[inline]
    pub fn session_count(&self) -> u64 {
        self.session_count
    }
}

/// A packed representation of an edge's start index and byte width.
///
/// This is packed by stealing 2 bits from the start index, which means we only accommodate edge
/// data arrays up to a quarter of our address space. Which seems fine.
#[derive(Debug, Clone, Copy)]
struct EdgeHeader {
    repr: usize,
    num_edges: u32,
}

impl EdgeHeader {
    #[inline]
    fn start(self) -> usize {
        self.repr >> DEP_NODE_WIDTH_BITS
    }

    #[inline]
    fn bytes_per_index(self) -> usize {
        (self.repr & mask(DEP_NODE_WIDTH_BITS)) + 1
    }

    #[inline]
    fn mask(self) -> u32 {
        mask(self.bytes_per_index() * 8) as u32
    }
}

#[inline]
fn mask(bits: usize) -> usize {
    usize::MAX >> ((size_of::<usize>() * 8) - bits)
}

impl SerializedDepGraph {
    #[instrument(level = "debug", skip(d, profiler))]
    pub fn decode(d: &mut MemDecoder<'_>, profiler: &SelfProfilerRef) -> Arc<SerializedDepGraph> {
        // The last 16 bytes are the node count and edge count.
        debug!("position: {:?}", d.position());

        // `node_max` is the number of indices including empty nodes while `node_count`
        // is the number of actually encoded nodes.
        let (node_max, node_count, edge_count) =
            d.with_position(d.len() - 3 * IntEncodedWithFixedSize::ENCODED_SIZE, |d| {
                debug!("position: {:?}", d.position());
                let node_max = IntEncodedWithFixedSize::decode(d).0 as usize;
                let node_count = IntEncodedWithFixedSize::decode(d).0 as usize;
                let edge_count = IntEncodedWithFixedSize::decode(d).0 as usize;
                (node_max, node_count, edge_count)
            });
        debug!("position: {:?}", d.position());

        debug!(?node_count, ?edge_count);

        let graph_bytes = d.len() - (3 * IntEncodedWithFixedSize::ENCODED_SIZE) - d.position();

        let mut nodes = IndexVec::from_elem_n(
            DepNode {
                kind: DepKind::Null,
                key_fingerprint: PackedFingerprint::from(Fingerprint::ZERO),
            },
            node_max,
        );
        let mut value_fingerprints = IndexVec::from_elem_n(Fingerprint::ZERO, node_max);
        let mut edge_list_indices =
            IndexVec::from_elem_n(EdgeHeader { repr: 0, num_edges: 0 }, node_max);

        // This estimation assumes that all of the encoded bytes are for the edge lists or for the
        // fixed-size node headers. But that's not necessarily true; if any edge list has a length
        // that spills out of the size we can bit-pack into NodeHead then some of the
        // total serialized size is also used by leb128-encoded edge list lengths. Neglecting that
        // contribution to graph_bytes means our estimation of the bytes needed for edge_list_data
        // slightly overshoots. But it cannot overshoot by much; consider that the worse case is
        // for a node with length 64, which means the spilled 1-byte leb128 length is 1 byte of at
        // least (34 byte header + 1 byte len + 64 bytes edge data), which is ~1%. A 2-byte leb128
        // length is about the same fractional overhead and it amortizes for yet greater lengths.
        let mut edge_list_data =
            Vec::with_capacity(graph_bytes.saturating_sub(node_count * NodeRecord::MIN_SIZE));

        let mut prev_index = None;
        let mut edge_history = Vec::new();
        for _ in 0..node_count {
            let record = NodeRecord::decode(d, prev_index, &nodes, &value_fingerprints);
            let index = record.index;
            prev_index = Some(index);

            let node = &mut nodes[index];
            // Make sure there's no duplicate indices in the dep graph.
            assert!(record.node.kind != DepKind::Null && node.kind == DepKind::Null);
            *node = record.node;

            value_fingerprints[index] = record.value_fingerprint;

            // In memory, the edges index list uses the same varint strategy as rmeta tables. We
            // select the number of byte elements per-array not per-element. Consider the
            // The in-memory structure for the edges list stores the byte width of the edges on
            // this node with the offset into the global edge data array.
            let edges_header = EdgeHeader {
                repr: (edge_list_data.len() << DEP_NODE_WIDTH_BITS)
                    | (record.head.bytes_per_index() - 1),
                num_edges: record.num_edges,
            };

            record.decode_edges(d, &mut edge_list_data, &mut edge_history);

            edge_list_indices[index] = edges_header;
        }

        // When we access the edge list data, we do a fixed-size read from the edge list data then
        // mask off the bytes that aren't for that edge index, so the last read may dangle off the
        // end of the array. This padding ensure it doesn't.
        edge_list_data.extend(&[0u8; DEP_NODE_PAD]);

        // Read the number of nodes of each dep kind, and perform
        // counting sort for `LazyNodeIndex`.
        let mut kinds = Vec::with_capacity(DepKind::NUM_VARIANTS);
        let mut offset = 0u32;
        for k in 0..(DepKind::NUM_VARIANTS) {
            let len = d.read_u32();
            // An anon key is not stored, so an anon kind is never looked up.
            let len = if DepKind::from_u16(k as u16).is_anon() { 0 } else { len };
            kinds.push(LazyKindIndex { start: offset, len, map: OnceLock::new() });
            offset += len;
        }
        debug_assert!(offset as usize <= node_count);

        let session_count = d.read_u64();

        // Counting sort: place each node index into its kind's range. `fill[k]`
        // points at the next free slot in kind `k`'s range, so a kind's nodes end
        // up contiguous. Slots start as `None` and are each filled exactly once
        // (the counts sum to the number of non-`Null` nodes).
        let mut nodes_by_kind = vec![None; offset as usize];
        let mut fill: Vec<u32> = kinds.iter().map(|k| k.start).collect();
        for (idx, node) in nodes.iter_enumerated() {
            // Unused indices from batch allocation stay `Null`; they carry no
            // encoded node and are never looked up by fingerprint, so skip them.
            if node.kind == DepKind::Null || node.kind.is_anon() {
                continue;
            }
            let k = node.kind.as_usize();
            nodes_by_kind[fill[k] as usize] = Some(idx);
            fill[k] += 1;
        }
        // Each kind's range was filled exactly to its end.
        debug_assert!(kinds.iter().zip(&fill).all(|(k, &f)| f == k.start + k.len));
        let reverse_index = LazyNodeIndex { nodes_by_kind, kinds };

        Arc::new(SerializedDepGraph {
            nodes,
            value_fingerprints,
            edge_list_indices,
            edge_list_data,
            reverse_index,
            live_node_count: node_count,
            session_count,
            profiler: Some(profiler.clone()),
        })
    }
}

/// The first a couple of bytes of a node record.
#[derive(Clone, Copy, Debug, PartialEq)]
struct NodeHead(u16);

// Bit fields, where
// M: bits used to store the length of a node's edge list
// N: bits used to store the byte width of elements of the edge list
// are
// 0..M    length of the edge
// M..M+N  bytes per index
// M+N..16 kind
impl NodeHead {
    const TOTAL_BITS: usize = size_of::<DepKind>() * 8;
    const LEN_BITS: usize = Self::TOTAL_BITS - Self::KIND_BITS - Self::WIDTH_BITS;
    const WIDTH_BITS: usize = DEP_NODE_WIDTH_BITS;
    const KIND_BITS: usize = Self::TOTAL_BITS - DepKind::MAX.leading_zeros() as usize;
    const MAX_INLINE_LEN: usize = (u16::MAX as usize >> (Self::TOTAL_BITS - Self::LEN_BITS)) - 1;

    #[inline]
    fn new(kind: DepKind, edge_max_index: u32, edge_count: usize) -> Self {
        debug_assert_eq!(Self::TOTAL_BITS, Self::LEN_BITS + Self::WIDTH_BITS + Self::KIND_BITS);

        let mut head = kind.as_u16();

        let free_bytes = edge_max_index.leading_zeros() as usize / 8;
        let bytes_per_index = (DEP_NODE_SIZE - free_bytes).saturating_sub(1);
        head |= (bytes_per_index as u16) << Self::KIND_BITS;

        // Encode number of edges + 1 so that we can reserve 0 to indicate that the len doesn't fit
        // in this bitfield.
        if edge_count <= Self::MAX_INLINE_LEN {
            head |= (edge_count as u16 + 1) << (Self::KIND_BITS + Self::WIDTH_BITS);
        }

        let head = Self(head);
        debug_assert_eq!(head.kind(), kind);
        debug_assert!(head.len().is_none_or(|len| len as usize == edge_count));
        head
    }

    #[inline]
    fn kind(self) -> DepKind {
        DepKind::from_u16(self.0 & mask(Self::KIND_BITS) as u16)
    }

    #[inline]
    fn bytes_per_index(self) -> usize {
        ((self.0 >> Self::KIND_BITS) & mask(Self::WIDTH_BITS) as u16) as usize + 1
    }

    #[inline]
    fn len(self) -> Option<u32> {
        ((self.0 as u32) >> (Self::WIDTH_BITS + Self::KIND_BITS)).checked_sub(1)
    }
}

/// The index is the node's index plus one. Otherwise it follows as a fixed `u32`.
const FLAG_INDEX_IS_NEXT: u8 = 1 << 0;
/// The key fingerprint is the key of an earlier node. A leb128 distance back to it follows.
const FLAG_KEY_IS_REF: u8 = 1 << 1;
/// The value fingerprint is the value of an earlier node. A leb128 distance back to it follows.
const FLAG_VALUE_IS_REF: u8 = 1 << 2;
/// The value fingerprint is `Fingerprint::ZERO` and is not stored.
const FLAG_VALUE_IS_ZERO: u8 = 1 << 3;

/// Keeps the low 64 bits of a result fingerprint. The graph stores only those
/// bits.
#[inline]
pub fn short_value_fingerprint(fingerprint: Fingerprint) -> Fingerprint {
    Fingerprint::new(fingerprint.split().0, 0u64)
}
/// The node is anon. Its key is never looked up again, so it is not stored and decodes as zero.
const FLAG_KEY_IS_ABSENT: u8 = 1 << 4;
/// The node is the first of a block that one thread wrote. Edge back-references start again here.
const FLAG_EDGES_RESTART: u8 = 1 << 5;

/// One node of the serialized stream, without its edge targets.
struct NodeRecord {
    head: NodeHead,
    index: SerializedDepNodeIndex,
    node: DepNode,
    value_fingerprint: Fingerprint,
    num_edges: u32,
    edges_restart: bool,
}

fn leb_len(x: u64) -> u32 {
    (64 - x.leading_zeros()).max(1).div_ceil(7)
}

impl NodeRecord {
    /// The head, the flags, and nothing else.
    const MIN_SIZE: usize = 3;

    fn decode(
        d: &mut MemDecoder<'_>,
        prev_index: Option<SerializedDepNodeIndex>,
        nodes: &IndexSlice<SerializedDepNodeIndex, DepNode>,
        value_fingerprints: &IndexSlice<SerializedDepNodeIndex, Fingerprint>,
    ) -> Self {
        let head = NodeHead(u16::from_le_bytes(d.read_array()));
        let flags = d.read_u8();

        let index = if flags & FLAG_INDEX_IS_NEXT != 0 {
            let prev = prev_index.expect("dep graph node refers to a missing previous node");
            SerializedDepNodeIndex::from_u32(prev.as_u32() + 1)
        } else {
            SerializedDepNodeIndex::from_u32(u32::from_le_bytes(d.read_array()))
        };

        let key_fingerprint = if flags & FLAG_KEY_IS_ABSENT != 0 {
            Fingerprint::ZERO.into()
        } else if flags & FLAG_KEY_IS_REF != 0 {
            nodes[Self::decode_ref(d, index, nodes)].key_fingerprint
        } else {
            Fingerprint::from_le_bytes(d.read_array()).into()
        };

        let value_fingerprint = if flags & FLAG_VALUE_IS_ZERO != 0 {
            Fingerprint::ZERO
        } else if flags & FLAG_VALUE_IS_REF != 0 {
            value_fingerprints[Self::decode_ref(d, index, nodes)]
        } else {
            Fingerprint::new(u64::from_le_bytes(d.read_array()), 0u64)
        };

        let num_edges = head.len().unwrap_or_else(|| d.read_u32());
        let node = DepNode { kind: head.kind(), key_fingerprint };
        let edges_restart = flags & FLAG_EDGES_RESTART != 0;
        NodeRecord { head, index, node, value_fingerprint, num_edges, edges_restart }
    }

    /// Reads this node's edge targets and appends them to `out` at the head's byte width.
    /// `history` holds the targets of the block so far, for back-references.
    fn decode_edges(&self, d: &mut MemDecoder<'_>, out: &mut Vec<u8>, history: &mut Vec<u32>) {
        if self.edges_restart {
            history.clear();
        }
        let width = self.head.bytes_per_index();
        let mut prev = self.index.as_u32() as i64;
        for _ in 0..self.num_edges {
            let code = d.read_u64();
            let target = if code & 1 != 0 {
                let zigzag = code >> 1;
                prev + ((zigzag >> 1) as i64 ^ -((zigzag & 1) as i64))
            } else {
                let distance = (code >> 1) as usize;
                let at = history
                    .len()
                    .checked_sub(distance)
                    .filter(|_| distance != 0)
                    .expect("dep graph edge back-reference does not point at an earlier edge");
                i64::from(history[at])
            };
            let target = u32::try_from(target).expect("dep graph edge target out of range");
            history.push(target);
            out.extend_from_slice(&target.to_le_bytes()[..width]);
            prev = i64::from(target);
        }
    }

    fn decode_ref(
        d: &mut MemDecoder<'_>,
        index: SerializedDepNodeIndex,
        nodes: &IndexSlice<SerializedDepNodeIndex, DepNode>,
    ) -> SerializedDepNodeIndex {
        let distance = d.read_u32();
        let target = index
            .as_u32()
            .checked_sub(distance)
            .filter(|_| distance != 0)
            .map(SerializedDepNodeIndex::from_u32)
            .expect("dep graph back-reference does not point at an earlier node");
        assert!(nodes[target].kind != DepKind::Null, "dep graph back-reference to a missing node");
        target
    }
}

/// A direct-mapped table from a fingerprint to the last node this thread
/// wrote with it.
struct FingerprintRefs {
    slots: Box<[(Fingerprint, u32)]>,
}

impl FingerprintRefs {
    const SLOTS: usize = 1 << 17;

    fn new() -> Self {
        FingerprintRefs { slots: vec![(Fingerprint::ZERO, u32::MAX); Self::SLOTS].into() }
    }

    /// Returns the last earlier node with `fingerprint`, and records `index` in its place.
    #[inline]
    fn replace(&mut self, fingerprint: Fingerprint, index: u32) -> Option<u32> {
        // Both halves feed the slot, because a `DefPathHash` key has the crate id as one half.
        let (a, b) = fingerprint.split();
        let slot = (a.as_u64() ^ b.as_u64()) as usize & (Self::SLOTS - 1);
        let (old_fingerprint, old_index) =
            mem::replace(&mut self.slots[slot], (fingerprint, index));
        (old_fingerprint == fingerprint && old_index < index).then_some(old_index)
    }
}

/// What a thread's encoder remembers about the nodes it already wrote.
struct EncoderRefs {
    /// The index of the last node in the unflushed buffer.
    prev_index: Option<u32>,
    keys: FingerprintRefs,
    values: FingerprintRefs,
    /// The position of the last edge of the unflushed buffer to each target.
    edge_positions: FxHashMap<u32, u32>,
    /// The number of edges in the unflushed buffer.
    edge_count: u32,
}

impl EncoderRefs {
    fn new() -> Self {
        EncoderRefs {
            prev_index: None,
            keys: FingerprintRefs::new(),
            values: FingerprintRefs::new(),
            edge_positions: FxHashMap::default(),
            edge_count: 0,
        }
    }
}

#[derive(Debug)]
struct NodeInfo<'a> {
    node: DepNode,
    value_fingerprint: Fingerprint,
    edges: &'a [DepNodeIndex],
}

/// The bytes one encoded node spends on each part of its record.
#[derive(Clone, Copy, Default)]
struct NodeBytes {
    head: u64,
    key: u64,
    value: u64,
    edges: u64,
}

impl NodeBytes {
    fn add(&mut self, other: NodeBytes) {
        self.head += other.head;
        self.key += other.key;
        self.value += other.value;
        self.edges += other.edges;
    }

    fn total(&self) -> u64 {
        self.head + self.key + self.value + self.edges
    }
}

impl NodeInfo<'_> {
    fn encode(&self, e: &mut MemEncoder, index: DepNodeIndex, refs: &mut EncoderRefs) -> NodeBytes {
        let start = e.position();
        let NodeInfo { ref node, value_fingerprint, edges } = *self;
        // The largest index picks the byte width of the edge list.
        let edge_max = edges.iter().map(|e| e.as_u32()).max().unwrap_or(0);
        let head = NodeHead::new(node.kind, edge_max, edges.len());

        let index = index.as_u32();
        let key_fingerprint: Fingerprint = node.key_fingerprint.into();
        let key_is_absent = node.kind.is_anon();
        let key_ref = if key_is_absent { None } else { refs.keys.replace(key_fingerprint, index) };
        debug_assert_eq!(value_fingerprint, short_value_fingerprint(value_fingerprint));
        let value_is_zero = value_fingerprint == Fingerprint::ZERO;
        let value_ref =
            if value_is_zero { None } else { refs.values.replace(value_fingerprint, index) };
        let index_is_next = refs.prev_index.is_some_and(|prev| prev + 1 == index);
        // A flush resets `prev_index`, so this node starts a block.
        let edges_restart = refs.prev_index.is_none();
        refs.prev_index = Some(index);

        let mut flags = 0;
        if index_is_next {
            flags |= FLAG_INDEX_IS_NEXT;
        }
        if edges_restart {
            flags |= FLAG_EDGES_RESTART;
            refs.edge_positions.clear();
            refs.edge_count = 0;
        }
        if key_is_absent {
            flags |= FLAG_KEY_IS_ABSENT;
        } else if key_ref.is_some() {
            flags |= FLAG_KEY_IS_REF;
        }
        if value_is_zero {
            flags |= FLAG_VALUE_IS_ZERO;
        } else if value_ref.is_some() {
            flags |= FLAG_VALUE_IS_REF;
        }

        e.write_array(head.0.to_le_bytes());
        e.emit_u8(flags);
        if !index_is_next {
            e.write_array(index.to_le_bytes());
        }
        let key_start = e.position();
        match key_ref {
            _ if key_is_absent => {}
            Some(target) => e.emit_u32(index - target),
            None => e.write_array(key_fingerprint.to_le_bytes()),
        }
        let value_start = e.position();
        if !value_is_zero {
            match value_ref {
                Some(target) => e.emit_u32(index - target),
                None => e.write_array(value_fingerprint.split().0.as_u64().to_le_bytes()),
            }
        }
        let edges_start = e.position();

        if head.len().is_none() {
            // The edges are all unique and the number of unique indices is less than u32::MAX.
            e.emit_u32(edges.len().try_into().unwrap());
        }

        // Each target is the shorter of leb128 codes.
        let mut prev = index as i64;
        for node_index in edges.iter() {
            let target = node_index.as_u32();
            let delta = i64::from(target) - prev;
            let delta_code = ((((delta << 1) ^ (delta >> 63)) as u64) << 1) | 1;
            let code = match refs.edge_positions.insert(target, refs.edge_count) {
                Some(at) if leb_len(u64::from(refs.edge_count - at) << 1) < leb_len(delta_code) => {
                    u64::from(refs.edge_count - at) << 1
                }
                _ => delta_code,
            };
            e.emit_u64(code);
            refs.edge_count += 1;
            prev = i64::from(target);
        }
        NodeBytes {
            head: (key_start - start) as u64,
            key: (value_start - key_start) as u64,
            value: (edges_start - value_start) as u64,
            edges: (e.position() - edges_start) as u64,
        }
    }
}

struct Stat {
    kind: DepKind,
    node_counter: u64,
    edge_counter: u64,
    bytes: NodeBytes,
}

struct LocalEncoderState {
    next_node_index: u32,
    remaining_node_index: u32,
    encoder: MemEncoder,
    refs: EncoderRefs,
    node_count: usize,
    edge_count: usize,

    /// Stores the number of times we've encoded each dep kind.
    kind_stats: Vec<u32>,
}

struct LocalEncoderResult {
    node_max: u32,
    node_count: usize,
    edge_count: usize,

    /// Stores the number of times we've encoded each dep kind.
    kind_stats: Vec<u32>,
}

struct EncoderState {
    next_node_index: AtomicU64,
    previous: Arc<SerializedDepGraph>,
    file: Lock<Option<FileEncoder<'static>>>,
    local: WorkerLocal<RefCell<LocalEncoderState>>,
    stats: Option<Lock<FxHashMap<DepKind, Stat>>>,
}

impl EncoderState {
    fn new(
        encoder: FileEncoder<'static>,
        record_stats: bool,
        previous: Arc<SerializedDepGraph>,
    ) -> Self {
        Self {
            previous,
            next_node_index: AtomicU64::new(0),
            stats: record_stats.then(|| Lock::new(FxHashMap::default())),
            file: Lock::new(Some(encoder)),
            local: WorkerLocal::new(|_| {
                RefCell::new(LocalEncoderState {
                    next_node_index: 0,
                    remaining_node_index: 0,
                    edge_count: 0,
                    node_count: 0,
                    encoder: MemEncoder::new(),
                    refs: EncoderRefs::new(),
                    kind_stats: iter::repeat_n(0, DepKind::NUM_VARIANTS).collect(),
                })
            }),
        }
    }

    #[inline]
    fn next_index(&self, local: &mut LocalEncoderState) -> DepNodeIndex {
        if local.remaining_node_index == 0 {
            const COUNT: u32 = 256;

            // We assume that there won't be enough active threads to overflow `u64` from `u32::MAX` here.
            // This can exceed u32::MAX by at most `N` * `COUNT` where `N` is the thread pool count since
            // `try_into().unwrap()` will make threads panic when `self.next_node_index` exceeds u32::MAX.
            local.next_node_index =
                self.next_node_index.fetch_add(COUNT as u64, Ordering::Relaxed).try_into().unwrap();

            // Check that we'll stay within `u32`
            local.next_node_index.checked_add(COUNT).unwrap();

            local.remaining_node_index = COUNT;
        }

        DepNodeIndex::from_u32(local.next_node_index)
    }

    /// Marks the index previously returned by `next_index` as used.
    #[inline]
    fn bump_index(&self, local: &mut LocalEncoderState) {
        local.remaining_node_index -= 1;
        local.next_node_index += 1;
        local.node_count += 1;
    }

    #[inline]
    fn record(
        &self,
        node: &DepNode,
        index: DepNodeIndex,
        edge_count: usize,
        edges: &[DepNodeIndex],
        bytes: NodeBytes,
        retained_graph: &Option<Lock<RetainedDepGraph>>,
        local: &mut LocalEncoderState,
    ) {
        local.kind_stats[node.kind.as_usize()] += 1;
        local.edge_count += edge_count;

        if let Some(retained_graph) = &retained_graph {
            // Outline the build of the full dep graph as it's typically disabled and cold.
            outline(move || {
                // Block on the lock rather than using `try_lock`: under the parallel frontend
                // several threads record nodes concurrently, and dropping a node on lock
                // contention would make the retained graph nondeterministic. Readers take a
                // clone of the graph (`retained_dep_graph`) rather than holding the lock, so
                // this never deadlocks against a reentrant `record`.
                retained_graph.lock().push(index, *node, edges);
            });
        }

        if let Some(stats) = &self.stats {
            let kind = node.kind;

            // Outline the stats code as it's typically disabled and cold.
            outline(move || {
                let mut stats = stats.lock();
                let stat = stats.entry(kind).or_insert(Stat {
                    kind,
                    node_counter: 0,
                    edge_counter: 0,
                    bytes: NodeBytes::default(),
                });
                stat.node_counter += 1;
                stat.edge_counter += edge_count as u64;
                stat.bytes.add(bytes);
            });
        }
    }

    #[inline]
    fn flush_mem_encoder(&self, local: &mut LocalEncoderState) {
        let data = &mut local.encoder.data;
        if data.len() > 64 * 1024 {
            self.file.lock().as_mut().unwrap().emit_raw_bytes(&data[..]);
            data.clear();
            local.refs.prev_index = None;
        }
    }

    /// Encodes a node to the current graph.
    fn encode_node(
        &self,
        index: DepNodeIndex,
        node: &NodeInfo<'_>,
        retained_graph: &Option<Lock<RetainedDepGraph>>,
        local: &mut LocalEncoderState,
    ) {
        let bytes = node.encode(&mut local.encoder, index, &mut local.refs);
        self.flush_mem_encoder(&mut *local);
        self.record(
            &node.node,
            index,
            node.edges.len(),
            node.edges,
            bytes,
            retained_graph,
            &mut *local,
        );
    }

    /// Encodes a node that was promoted from the previous graph, reading the node and its
    /// fingerprint directly from the previous dep graph. It expects all edges to already
    /// have a new dep node index assigned.
    #[inline]
    fn encode_promoted_node(
        &self,
        index: DepNodeIndex,
        prev_index: SerializedDepNodeIndex,
        retained_graph: &Option<Lock<RetainedDepGraph>>,
        local: &mut LocalEncoderState,
        edges: &[DepNodeIndex],
    ) {
        let node = NodeInfo {
            node: *self.previous.index_to_node(prev_index),
            value_fingerprint: self.previous.value_fingerprint_for_index(prev_index),
            edges,
        };
        self.encode_node(index, &node, retained_graph, local);
    }

    fn finish(&self, profiler: &SelfProfilerRef, current: &CurrentDepGraph) -> FileEncodeResult {
        // Prevent more indices from being allocated.
        self.next_node_index.store(u32::MAX as u64 + 1, Ordering::SeqCst);

        let results = broadcast(|_| {
            let mut local = self.local.borrow_mut();

            // Prevent more indices from being allocated on this thread.
            local.remaining_node_index = 0;

            let data = mem::take(&mut local.encoder.data);
            self.file.lock().as_mut().unwrap().emit_raw_bytes(&data);

            LocalEncoderResult {
                kind_stats: local.kind_stats.clone(),
                node_max: local.next_node_index,
                node_count: local.node_count,
                edge_count: local.edge_count,
            }
        });

        let mut encoder = self.file.lock().take().unwrap();

        let mut kind_stats: Vec<u32> = iter::repeat_n(0, DepKind::NUM_VARIANTS).collect();

        let mut node_max = 0;
        let mut node_count = 0;
        let mut edge_count = 0;

        for result in results {
            node_max = max(node_max, result.node_max);
            node_count += result.node_count;
            edge_count += result.edge_count;
            for (i, stat) in result.kind_stats.iter().enumerate() {
                kind_stats[i] += stat;
            }
        }

        // Encode the number of each dep kind encountered
        for count in kind_stats.iter() {
            count.encode(&mut encoder);
        }

        self.previous.session_count.checked_add(1).unwrap().encode(&mut encoder);

        debug!(?node_max, ?node_count, ?edge_count);
        debug!("position: {:?}", encoder.position());
        IntEncodedWithFixedSize(node_max.try_into().unwrap()).encode(&mut encoder);
        IntEncodedWithFixedSize(node_count.try_into().unwrap()).encode(&mut encoder);
        IntEncodedWithFixedSize(edge_count.try_into().unwrap()).encode(&mut encoder);
        debug!("position: {:?}", encoder.position());
        // Drop the encoder so that nothing is written after the counts.
        let result = encoder.finish();
        if let Ok(position) = result {
            // FIXME(rylev): we hardcode the dep graph file name so we
            // don't need a dependency on rustc_incremental just for that.
            profiler.artifact_size("dep_graph", "dep-graph.bin", position as u64);
        }

        self.print_incremental_info(current, node_count, edge_count);

        result
    }

    fn print_incremental_info(
        &self,
        current: &CurrentDepGraph,
        total_node_count: usize,
        total_edge_count: usize,
    ) {
        if let Some(record_stats) = &self.stats {
            let record_stats = record_stats.lock();
            // `stats` is sorted below so we can allow this lint here.
            #[allow(rustc::potential_query_instability)]
            let mut stats: Vec<_> = record_stats.values().collect();
            stats.sort_by_key(|s| -(s.node_counter as i64));

            const SEPARATOR: &str = "[incremental] --------------------------------\
                                     ----------------------------------------------\
                                     ------------";

            eprintln!("[incremental]");
            eprintln!("[incremental] DepGraph Statistics");
            eprintln!("{SEPARATOR}");
            eprintln!("[incremental]");
            eprintln!("[incremental] Total Node Count: {}", total_node_count);
            eprintln!("[incremental] Total Edge Count: {}", total_edge_count);
            let mut bytes = NodeBytes::default();
            for stat in &stats {
                bytes.add(stat.bytes);
            }
            eprintln!(
                "[incremental] dep graph node bytes: total {} heads {} keys {} values {} edges {}",
                bytes.total(),
                bytes.head,
                bytes.key,
                bytes.value,
                bytes.edges,
            );

            if cfg!(debug_assertions) {
                let total_read_count = current.total_read_count.load(Ordering::Relaxed);
                let total_duplicate_read_count =
                    current.total_duplicate_read_count.load(Ordering::Relaxed);
                eprintln!("[incremental] Total Edge Reads: {total_read_count}");
                eprintln!("[incremental] Total Duplicate Edge Reads: {total_duplicate_read_count}");
            }

            eprintln!("[incremental]");
            eprintln!(
                "[incremental]  {:<36}| {:<17}| {:<12}| {:<17}| {:<12}|",
                "Node Kind", "Node Frequency", "Node Count", "Avg. Edge Count", "Bytes"
            );
            eprintln!("{SEPARATOR}");

            for stat in stats {
                let node_kind_ratio =
                    (100.0 * (stat.node_counter as f64)) / (total_node_count as f64);
                let node_kind_avg_edges = (stat.edge_counter as f64) / (stat.node_counter as f64);

                eprintln!(
                    "[incremental]  {:<36}|{:>16.1}% |{:>12} |{:>17.1} |{:>12} |",
                    format!("{:?}", stat.kind),
                    node_kind_ratio,
                    stat.node_counter,
                    node_kind_avg_edges,
                    stat.bytes.total(),
                );
            }

            eprintln!("{SEPARATOR}");
            eprintln!("[incremental]");
        }
    }
}

pub(crate) struct GraphEncoder {
    profiler: SelfProfilerRef,
    status: EncoderState,
    /// In-memory copy of the dep graph; only present if `-Zquery-dep-graph` is set.
    retained_graph: Option<Lock<RetainedDepGraph>>,
}

impl GraphEncoder {
    pub(crate) fn new(
        sess: &Session,
        encoder: FileEncoder<'static>,
        prev_index_space_len: usize,
        previous: Arc<SerializedDepGraph>,
    ) -> Self {
        let retained_graph = sess
            .opts
            .unstable_opts
            .query_dep_graph
            .then(|| Lock::new(RetainedDepGraph::new(prev_index_space_len)));
        let status = EncoderState::new(encoder, sess.opts.unstable_opts.incremental_info, previous);
        GraphEncoder { status, retained_graph, profiler: sess.prof.clone() }
    }

    pub(crate) fn retained_dep_graph(&self) -> Option<RetainedDepGraph> {
        self.retained_graph.as_ref().map(|retained_graph| retained_graph.lock().clone())
    }

    /// Encodes a node that does not exists in the previous graph.
    pub(crate) fn send_new(
        &self,
        node: DepNode,
        value_fingerprint: Fingerprint,
        edges: &[DepNodeIndex],
    ) -> DepNodeIndex {
        let _prof_timer = self.profiler.generic_activity("incr_comp_encode_dep_graph");
        let node = NodeInfo { node, value_fingerprint, edges };
        let mut local = self.status.local.borrow_mut();
        let index = self.status.next_index(&mut *local);
        self.status.bump_index(&mut *local);
        self.status.encode_node(index, &node, &self.retained_graph, &mut *local);
        index
    }

    /// Encodes a node that exists in the previous graph, but was re-executed.
    ///
    /// This will also ensure the dep node is colored either red or green.
    pub(crate) fn send_and_color(
        &self,
        prev_index: SerializedDepNodeIndex,
        colors: &DepNodeColorMap,
        node: DepNode,
        value_fingerprint: Fingerprint,
        edges: &[DepNodeIndex],
        is_green: bool,
    ) -> DepNodeIndex {
        let _prof_timer = self.profiler.generic_activity("incr_comp_encode_dep_graph");
        let node = NodeInfo { node, value_fingerprint, edges };

        let mut local = self.status.local.borrow_mut();

        let index = self.status.next_index(&mut *local);
        let color = if is_green { DesiredColor::Green { index } } else { DesiredColor::Red };

        // Use `try_set_color` to avoid racing when `send_promoted` is called concurrently
        // on the same index.
        match colors.try_set_color(prev_index, color) {
            TrySetColorResult::Success => {}
            TrySetColorResult::AlreadyRed => panic!("dep node {prev_index:?} is unexpectedly red"),
            TrySetColorResult::AlreadyGreen { index } => return index,
        }

        self.status.bump_index(&mut *local);
        self.status.encode_node(index, &node, &self.retained_graph, &mut *local);
        index
    }

    /// Encodes a node that was promoted from the previous graph. It reads the information directly
    /// from the previous dep graph and expects all edges to already have a new dep node index
    /// assigned.
    ///
    /// Tries to mark the dep node green, and returns Some if it is now green,
    /// or None if had already been concurrently marked red.
    #[inline]
    pub(crate) fn send_promoted(
        &self,
        prev_index: SerializedDepNodeIndex,
        colors: &DepNodeColorMap,
        edges: &[DepNodeIndex],
    ) -> Option<DepNodeIndex> {
        let _prof_timer = self.profiler.generic_activity("incr_comp_encode_dep_graph");

        let mut local = self.status.local.borrow_mut();
        let index = self.status.next_index(&mut *local);

        // Use `try_set_color` to avoid racing when `send_promoted` or `send_and_color`
        // is called concurrently on the same index.
        match colors.try_set_color(prev_index, DesiredColor::Green { index }) {
            TrySetColorResult::Success => {
                self.status.bump_index(&mut *local);
                self.status.encode_promoted_node(
                    index,
                    prev_index,
                    &self.retained_graph,
                    &mut *local,
                    edges,
                );
                Some(index)
            }
            TrySetColorResult::AlreadyRed => None,
            TrySetColorResult::AlreadyGreen { index } => Some(index),
        }
    }

    pub(crate) fn finish(&self, current: &CurrentDepGraph) -> FileEncodeResult {
        let _prof_timer = self.profiler.generic_activity("incr_comp_encode_dep_graph_finish");

        self.status.finish(&self.profiler, current)
    }
}
