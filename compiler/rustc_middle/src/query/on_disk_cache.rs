use std::cell::RefCell;
use std::collections::hash_map::Entry;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::{fmt, mem};

use rustc_data_structures::fingerprint::Fingerprint;
use rustc_data_structures::flock;
use rustc_data_structures::fx::{FxHashMap, FxHashSet, FxIndexSet};
use rustc_data_structures::memmap::Mmap;
use rustc_data_structures::sync::{HashMapExt, Lock, RwLock};
use rustc_data_structures::unhash::UnhashMap;
use rustc_data_structures::unord::{UnordMap, UnordSet};
use rustc_hir::def_id::{CrateNum, DefId, DefIndex, LOCAL_CRATE, LocalDefId, StableCrateId};
use rustc_hir::definitions::DefPathHash;
use rustc_index::IndexVec;
use rustc_macros::{Decodable, Encodable};
use rustc_serialize::opaque::mem_encoder::MemEncoder;
use rustc_serialize::opaque::{FileEncodeResult, FileEncoder, IntEncodedWithFixedSize, MemDecoder};
use rustc_serialize::{ContextOrd, Decodable, Decoder, Encodable, Encoder};
use rustc_session::Session;
use rustc_span::hygiene::{
    ExpnId, ExpnKind, HygieneDecodeContext, HygieneEncodeContext, SyntaxContext, SyntaxContextKey,
};
use rustc_span::{
    BlobDecoder, BytePos, ByteSymbol, CachingSourceMapView, ExpnData, ExpnHash, RelativeBytePos,
    SourceFile, Span, SpanDecoder, SpanEncoder, StableSourceFileId, Symbol, bug,
};

use crate::dep_graph::{DepNodeIndex, QuerySideEffect, SerializedDepNodeIndex};
use crate::ich::SourceSpanCache;
use crate::mir::interpret::{AllocDecodingSession, AllocDecodingState};
use crate::mir::{self, interpret};
use crate::mono::MonoItemUse;
use crate::ty::codec::{
    RefDecodable, SHORTHAND_OFFSET, TyDecoder, TyEncoder, forward_all_decoder_methods_to,
};
use crate::ty::{self, Ty, TyCtxt};

#[cfg(test)]
mod tests;

const TAG_FILE_FOOTER: u128 = 0xC0FFEE_C0FFEE_C0FFEE_C0FFEE_C0FFEE;
/// The footer of a session file whose values live in the shared values and pool files.
const TAG_FILE_FOOTER_INPLACE: u128 = 0xC0FFEE_C0FFEE_C0FFEE_C0FFEE_1F1AC2;

/// The first bytes of a values file and of a pool file. An 8-byte id follows each.
const VALUES_MAGIC: &[u8; 8] = b"RSQVALS3";
const POOL_MAGIC: &[u8; 8] = b"RSQPOOL3";
const HEADER_LEN: usize = 16;
const POOL_FILENAME: &str = "query-pool.bin";

// A span starts with a head byte. `SPAN_KIND_MASK` picks its kind out of it.
const TAG_FULL_SPAN: u8 = 0;
// A partial span with no location information, encoded only with a `SyntaxContext`
const TAG_PARTIAL_SPAN: u8 = 1;
// The start is an offset from the start of the parent.
const TAG_RELATIVE_SPAN: u8 = 2;
// The start is an offset from the start of the span before, which has the same parent.
const TAG_PREVIOUS_SPAN: u8 = 3;
const SPAN_KIND_MASK: u8 = 0b11;
// The context is the context of the span before, and is not stored.
const SPAN_SAME_CTXT: u8 = 1 << 2;
// The parent is the parent of the span before, and is not stored.
const SPAN_SAME_PARENT: u8 = 1 << 3;

/// What a span can take from the span before it. The encoder and the decoder
/// see the spans of one value in the same order.
#[derive(Clone, Copy, Default)]
struct PrevSpan {
    ctxt: Option<SyntaxContext>,
    parent: Option<Option<LocalDefId>>,
    /// The start of the span before, when its start was relative to `parent`.
    lo: Option<BytePos>,
}

const TAG_SYNTAX_CONTEXT: u8 = 0;
const TAG_EXPN_DATA: u8 = 1;

// Tags for encoding Symbols and ByteSymbols
const SYMBOL_STR: u8 = 0;
const SYMBOL_OFFSET: u8 = 1;
const SYMBOL_PREDEFINED: u8 = 2;

/// Provides an interface to incremental compilation data cached from the
/// previous compilation session. This data will eventually include the results
/// of a few selected queries (like `typeck` and `mir_optimized`) and
/// any side effects that have been emitted during a query.
pub struct OnDiskCache {
    // The complete cache data in serialized form.
    serialized_data: RwLock<Option<Mmap>>,

    file_index_to_stable_id: FxHashMap<SourceFileIndex, EncodedSourceFileId>,

    // Caches that are populated lazily during decoding.
    file_index_to_file: Lock<FxHashMap<SourceFileIndex, Arc<SourceFile>>>,

    /// For query dep nodes that have a disk-cached return value, maps the node
    /// to the position and length of its serialized value in `serialized_data`.
    query_values_index: FxHashMap<SerializedDepNodeIndex, (AbsoluteBytePos, u32)>,

    /// For `DepKind::SideEffect` dep nodes, maps the node index to the position
    /// and length of its serialized [`QuerySideEffect`] in `serialized_data`.
    side_effects_index: FxHashMap<SerializedDepNodeIndex, (AbsoluteBytePos, u32)>,

    alloc_decoding_state: AllocDecodingState,

    // A map from syntax context ids to the position of their associated
    // `SyntaxContextData`. We use a `u32` instead of a `SyntaxContext`
    // to represent the fact that we are storing *encoded* ids. When we decode
    // a `SyntaxContext`, a new id will be allocated from the global `HygieneData`,
    // which will almost certainly be different than the serialized id.
    syntax_contexts: FxHashMap<u32, AbsoluteBytePos>,
    // A map from the `DefPathHash` of an `ExpnId` to the position
    // of their associated `ExpnData`. Ideally, we would store a `DefId`,
    // but we need to decode this before we've constructed a `TyCtxt` (which
    // makes it difficult to decode a `DefId`).

    // Note that these `DefPathHashes` correspond to both local and foreign
    // `ExpnData` (e.g `ExpnData.krate` may not be `LOCAL_CRATE`). Alternatively,
    // we could look up the `ExpnData` from the metadata of foreign crates,
    // but it seemed easier to have `OnDiskCache` be independent of the `CStore`.
    expn_data: UnhashMap<ExpnHash, AbsoluteBytePos>,
    // Additional information used when decoding hygiene data.
    hygiene_context: HygieneDecodeContext,
    // Maps `ExpnHash`es to their raw value from the *previous*
    // compilation session. This is used as an initial 'guess' when
    // we try to map an `ExpnHash` to its value in the current
    // compilation session.
    foreign_expn_data: UnhashMap<ExpnHash, u32>,
    /// The `DefPathHash` of each `DefId` index in the file. Empty in the inplace format, which writes the hash itself.
    def_path_hashes: Vec<DefPathHash>,

    /// Set when the loaded `query-cache.bin` indexes the shared values file.
    inplace: Option<InplaceState>,
    /// The mapped pool file of `inplace`.
    pool_data: RwLock<Option<Mmap>>,
    /// What this session decoded from the pool. The save reuses those pool items.
    pool_decoded: PoolDecoded,
}

/// What the loaded `query-cache.bin` records about the shared values and pool files.
struct InplaceState {
    values_id: u64,
    /// The file length when the session committed. Later bytes are not its.
    values_len: usize,
    pool_id: u64,
    pool_len: usize,
    /// The pool length after the save that started this pool.
    pool_base_len: usize,
    query_values: Vec<IndexEntry>,
    side_effects: Vec<IndexEntry>,
}

/// Pool items that this session decoded, by pool position.
#[derive(Default)]
struct PoolDecoded {
    ctxts: Lock<FxHashMap<usize, SyntaxContext>>,
    expns: Lock<FxHashMap<usize, ExpnId>>,
    files: Lock<FxHashMap<usize, Arc<SourceFile>>>,
    /// The pool position of each symbol, by symbol index.
    symbols: Lock<FxHashMap<u32, usize>>,
}

/// One entry of the values file: the position of its alloc table and its byte
/// length.
#[derive(Clone, Copy)]
struct IndexEntry {
    index: SerializedDepNodeIndex,
    pos: AbsoluteBytePos,
    len: u32,
}

impl IndexEntry {
    fn span(&self) -> (SerializedDepNodeIndex, (AbsoluteBytePos, u32)) {
        (self.index, (self.pos, self.len))
    }
}

/// Index entries sorted by node index, with the index stored as a delta.
fn encode_index_entries<E: Encoder>(e: &mut E, entries: &mut [IndexEntry]) {
    entries.sort_unstable_by_key(|x| x.index.as_u32());
    e.emit_usize(entries.len());
    let mut prev = 0u32;
    for x in entries.iter() {
        e.emit_u32(x.index.as_u32() - prev);
        e.emit_u64(x.pos.0);
        e.emit_u32(x.len);
        prev = x.index.as_u32();
    }
}

/// An id for a fresh values file, so a session file never reads another values file
/// that took the same path.
fn new_values_id() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let mut hasher = std::hash::DefaultHasher::new();
    std::hash::Hash::hash(&(nanos, std::process::id()), &mut hasher);
    std::hash::Hasher::finish(&hasher)
}

fn decode_index_entries<D: Decoder>(d: &mut D) -> Vec<IndexEntry> {
    let n = d.read_usize();
    let mut prev = 0u32;
    (0..n)
        .map(|_| {
            prev += d.read_u32();
            let pos = AbsoluteBytePos(d.read_u64());
            IndexEntry { index: SerializedDepNodeIndex::from_u32(prev), pos, len: d.read_u32() }
        })
        .collect()
}

/// Reads the 16-byte header of the file at `path`, and the file length.
fn read_header(path: &Path) -> Option<([u8; HEADER_LEN], u64)> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut header = [0u8; HEADER_LEN];
    f.read_exact(&mut header).ok()?;
    Some((header, f.metadata().ok()?.len()))
}

/// Maps the file at `path` if its header holds `magic` and `id` and it holds `len` bytes.
fn map_checked(path: &Path, magic: &[u8; 8], id: u64, len: usize) -> Result<Mmap, ()> {
    let file = std::fs::File::open(path).map_err(|_| ())?;
    // SAFETY: Writers only append. A reader reads only its committed prefix.
    let data = unsafe { Mmap::map(file) }.map_err(|_| ())?;
    if data.len() < len
        || len < HEADER_LEN
        || data[..8] != magic[..]
        || data[8..16] != id.to_le_bytes()
    {
        return Err(());
    }
    MemDecoder::new(&data[..len], 0)?;
    Ok(data)
}

/// The values of a session file, in file order.
struct ValuesIndex(Vec<IndexEntry>);

impl<E: Encoder> Encodable<E> for ValuesIndex {
    fn encode(&self, e: &mut E) {
        e.emit_usize(self.0.len());
        let Some(first) = self.0.first() else { return };
        e.emit_u64(first.pos.0);
        let mut prev_index = 0u32;
        let mut next_pos = first.pos.0;
        for x in &self.0 {
            assert_eq!(x.pos.0, next_pos, "query cache values are not contiguous");
            let step = i64::from(x.index.as_u32()) - i64::from(prev_index);
            e.emit_u64(((step << 1) ^ (step >> 63)) as u64);
            e.emit_u32(x.len);
            prev_index = x.index.as_u32();
            next_pos += u64::from(x.len);
        }
    }
}

impl<D: Decoder> Decodable<D> for ValuesIndex {
    fn decode(d: &mut D) -> Self {
        let n = d.read_usize();
        if n == 0 {
            return ValuesIndex(Vec::new());
        }
        let mut pos = d.read_u64();
        let mut index = 0i64;
        ValuesIndex(
            (0..n)
                .map(|_| {
                    let zigzag = d.read_u64();
                    index += ((zigzag >> 1) as i64) ^ -((zigzag & 1) as i64);
                    let len = d.read_u32();
                    let entry = IndexEntry {
                        index: SerializedDepNodeIndex::from_u32(index as u32),
                        pos: AbsoluteBytePos(pos),
                        len,
                    };
                    pos += u64::from(len);
                    entry
                })
                .collect(),
        )
    }
}

/// The `DefPathHash` of each `DefId` index in the file.
struct DefPathHashes(Vec<DefPathHash>);

impl<E: Encoder> Encodable<E> for DefPathHashes {
    fn encode(&self, e: &mut E) {
        let mut crates = FxIndexSet::default();
        let crate_indices: Vec<usize> =
            self.0.iter().map(|h| crates.insert_full(h.stable_crate_id().as_u64()).0).collect();
        e.emit_usize(crates.len());
        for krate in &crates {
            e.emit_raw_bytes(&krate.to_le_bytes());
        }
        e.emit_usize(self.0.len());
        for (hash, krate) in self.0.iter().zip(crate_indices) {
            e.emit_usize(krate);
            e.emit_raw_bytes(&hash.local_hash().as_u64().to_le_bytes());
        }
    }
}

impl<D: Decoder> Decodable<D> for DefPathHashes {
    fn decode(d: &mut D) -> Self {
        fn read_u64_raw<D: Decoder>(d: &mut D) -> u64 {
            u64::from_le_bytes(d.read_raw_bytes(8).try_into().unwrap())
        }
        let crates: Vec<u64> = (0..d.read_usize()).map(|_| read_u64_raw(d)).collect();
        let n = d.read_usize();
        DefPathHashes(
            (0..n)
                .map(|_| {
                    let krate = crates[d.read_usize()];
                    DefPathHash(Fingerprint::new(krate, read_u64_raw(d)))
                })
                .collect(),
        )
    }
}

// This type is used only for serialization and deserialization.
// `CacheEncoder::encode_footer` writes the fields in the order `Decodable` reads them.
#[derive(Decodable)]
struct Footer {
    file_index_to_stable_id: FxHashMap<SourceFileIndex, EncodedSourceFileId>,
    query_values_index: ValuesIndex,
    side_effects_index: ValuesIndex,
    // The location of all allocations.
    // Most uses only need values up to u32::MAX, but benchmarking indicates that we can use a u64
    // without measurable overhead. This permits larger const allocations without ICEing.
    interpret_alloc_index: Vec<u64>,
    // See `OnDiskCache.syntax_contexts`
    syntax_contexts: FxHashMap<u32, AbsoluteBytePos>,
    // See `OnDiskCache.expn_data`
    expn_data: UnhashMap<ExpnHash, AbsoluteBytePos>,
    foreign_expn_data: UnhashMap<ExpnHash, u32>,
    // Each `DefId` in the file is an index into this table.
    def_path_hashes: DefPathHashes,
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Encodable, Decodable)]
struct SourceFileIndex(u32);

// this is never used by the on disk cache, since it is not required to be deterministic
impl<E> ContextOrd<E> for SourceFileIndex {}

#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq, Encodable, Decodable)]
struct AbsoluteBytePos(u64);

impl AbsoluteBytePos {
    #[inline]
    fn new(pos: usize) -> AbsoluteBytePos {
        AbsoluteBytePos(pos.try_into().expect("Incremental cache file size overflowed u64."))
    }

    #[inline]
    fn to_usize(self) -> usize {
        self.0 as usize
    }
}

#[derive(Encodable, Decodable, Clone, Debug)]
struct EncodedSourceFileId {
    stable_source_file_id: StableSourceFileId,
    stable_crate_id: StableCrateId,
}

impl EncodedSourceFileId {
    #[inline]
    fn new(tcx: TyCtxt<'_>, file: &SourceFile) -> EncodedSourceFileId {
        EncodedSourceFileId {
            stable_source_file_id: file.stable_id,
            stable_crate_id: tcx.stable_crate_id(file.cnum),
        }
    }
}

impl OnDiskCache {
    /// Creates a new `OnDiskCache` instance from the serialized data in `data`.
    ///
    /// The serialized cache has some basic integrity checks, if those checks indicate that the
    /// on-disk data is corrupt, an error is returned.
    pub fn new(
        sess: &Session,
        data: Mmap,
        start_pos: usize,
        values_path: &Path,
    ) -> Result<Self, ()> {
        assert!(sess.opts.incremental.is_some());

        let mut decoder = MemDecoder::new(&data, start_pos)?;

        // Decode the *position* of the footer, which can be found in the
        // last 8 bytes of the file.
        let footer_pos = decoder
            .with_position(decoder.len() - IntEncodedWithFixedSize::ENCODED_SIZE, |decoder| {
                IntEncodedWithFixedSize::decode(decoder).0 as usize
            });
        match decoder.with_position(footer_pos, |decoder| u128::decode(decoder)) {
            TAG_FILE_FOOTER => {}
            TAG_FILE_FOOTER_INPLACE => {
                return Self::new_inplace(&mut decoder, footer_pos, values_path);
            }
            _ => return Err(()),
        }
        // Decode the file footer, which contains all the lookup tables, etc.
        let footer: Footer =
            decoder.with_position(footer_pos, |decoder| decode_tagged(decoder, TAG_FILE_FOOTER));

        Ok(Self {
            serialized_data: RwLock::new(Some(data)),
            file_index_to_stable_id: footer.file_index_to_stable_id,
            file_index_to_file: Default::default(),
            query_values_index: footer.query_values_index.0.iter().map(IndexEntry::span).collect(),
            side_effects_index: footer.side_effects_index.0.iter().map(IndexEntry::span).collect(),
            alloc_decoding_state: AllocDecodingState::new(footer.interpret_alloc_index),
            syntax_contexts: footer.syntax_contexts,
            expn_data: footer.expn_data,
            foreign_expn_data: footer.foreign_expn_data,
            def_path_hashes: footer.def_path_hashes.0,
            hygiene_context: Default::default(),
            inplace: None,
            pool_data: RwLock::new(None),
            pool_decoded: Default::default(),
        })
    }

    fn new_inplace(
        decoder: &mut MemDecoder<'_>,
        footer_pos: usize,
        values_path: &Path,
    ) -> Result<Self, ()> {
        let state = decoder.with_position(footer_pos, |d| {
            let _tag = u128::decode(d);
            InplaceState {
                values_id: d.read_u64(),
                values_len: d.read_u64() as usize,
                pool_id: d.read_u64(),
                pool_len: d.read_u64() as usize,
                pool_base_len: d.read_u64() as usize,
                query_values: decode_index_entries(d),
                side_effects: decode_index_entries(d),
            }
        });
        let values = map_checked(values_path, VALUES_MAGIC, state.values_id, state.values_len)?;
        let pool_path = values_path.with_file_name(POOL_FILENAME);
        let pool = map_checked(&pool_path, POOL_MAGIC, state.pool_id, state.pool_len)?;
        let mut cache = Self::new_empty();
        cache.query_values_index = state.query_values.iter().map(IndexEntry::span).collect();
        cache.side_effects_index = state.side_effects.iter().map(IndexEntry::span).collect();
        cache.serialized_data = RwLock::new(Some(values));
        cache.pool_data = RwLock::new(Some(pool));
        cache.inplace = Some(state);
        Ok(cache)
    }

    /// Whether this session keeps the entries of green nodes from the shared
    /// values file without loading them.
    pub fn inplace_keeps_entries(&self) -> bool {
        self.inplace.as_ref().is_some_and(|s| {
            let grown = s.pool_len.saturating_sub(s.pool_base_len);
            grown * 4 <= s.pool_base_len.max(1 << 20)
        })
    }

    pub fn new_empty() -> Self {
        Self {
            serialized_data: RwLock::new(None),
            file_index_to_stable_id: Default::default(),
            file_index_to_file: Default::default(),
            query_values_index: Default::default(),
            side_effects_index: Default::default(),
            alloc_decoding_state: AllocDecodingState::new(Vec::new()),
            syntax_contexts: FxHashMap::default(),
            expn_data: UnhashMap::default(),
            foreign_expn_data: UnhashMap::default(),
            def_path_hashes: Vec::new(),
            hygiene_context: Default::default(),
            inplace: None,
            pool_data: RwLock::new(None),
            pool_decoded: Default::default(),
        }
    }

    /// Release the serialized backing `Mmap`.
    pub fn close_serialized_data_mmap(&self) {
        // Obtain a write lock, and replace the mmap with None to drop it.
        *self.serialized_data.write() = None;
        *self.pool_data.write() = None;
    }

    /// Serialize the current-session data that will be loaded by [`OnDiskCache`]
    /// in a subsequent incremental compilation session.
    pub fn serialize(tcx: TyCtxt<'_>, encoder: FileEncoder<'static>) -> FileEncodeResult {
        // Serializing the `DepGraph` should not modify it.
        tcx.dep_graph.with_ignore(|| {
            // Allocate `SourceFileIndex`es.
            let (file_to_file_index, file_index_to_stable_id) = {
                let files = tcx.sess.source_map().files();
                let mut file_to_file_index =
                    FxHashMap::with_capacity_and_hasher(files.len(), Default::default());
                let mut file_index_to_stable_id =
                    FxHashMap::with_capacity_and_hasher(files.len(), Default::default());

                for (index, file) in files.iter().enumerate() {
                    let index = SourceFileIndex(index as u32);
                    let file_ptr: *const SourceFile = &raw const **file;
                    file_to_file_index.insert(file_ptr, index);
                    let source_file_id = EncodedSourceFileId::new(tcx, file);
                    file_index_to_stable_id.insert(index, source_file_id);
                }

                (file_to_file_index, file_index_to_stable_id)
            };

            let mut encoder = CacheEncoder {
                tcx,
                encoder,
                type_shorthands: Default::default(),
                predicate_shorthands: Default::default(),
                interpret_allocs: Default::default(),
                caching_source_map_view: CachingSourceMapView::new(tcx.sess.source_map()),
                file_to_file_index,
                hygiene_context: Default::default(),
                symbol_index_table: Default::default(),
                source_span_cache: Default::default(),
                query_values_index: Default::default(),
                side_effects_index: Default::default(),
                def_path_hashes: Default::default(),
                span_bytes: 0,
                shorthand_refs: 0,
                shorthand_ref_bytes: 0,
                instance_shorthands: Default::default(),
                prev_span: PrevSpan::default(),
                prev_span_saved: Vec::new(),
                inplace: None,
            };

            let values_start = encoder.position();
            // Encode query return values.
            tcx.sess.time("encode_query_values", || {
                tcx.encode_query_values(&mut encoder);
            });
            let values_end = encoder.position();

            // Encode side effects.
            for (&dep_node_index, side_effect) in tcx.query_system.side_effects.borrow().iter() {
                encoder.encode_side_effect(dep_node_index, side_effect);
            }
            let side_effects_end = encoder.position();

            let interpret_alloc_index = {
                let mut interpret_alloc_index = Vec::new();
                let mut n = 0;
                loop {
                    let new_n = encoder.interpret_allocs.len();
                    // If we have found new IDs, serialize those too.
                    if n == new_n {
                        // Otherwise, abort.
                        break;
                    }
                    interpret_alloc_index.reserve(new_n - n);
                    for idx in n..new_n {
                        let id = encoder.interpret_allocs[idx];
                        let pos: u64 = encoder.position().try_into().unwrap();
                        interpret_alloc_index.push(pos);
                        // The decoder reads each allocation from a jump, with fresh span state.
                        encoder.prev_span = PrevSpan::default();
                        interpret::specialized_encode_alloc_id(&mut encoder, tcx, id);
                    }
                    n = new_n;
                }
                interpret_alloc_index
            };
            let allocs_end = encoder.position();

            let mut syntax_contexts = FxHashMap::default();
            let mut expn_data = UnhashMap::default();
            let mut foreign_expn_data = UnhashMap::default();
            let mut ctxt_bytes = 0;
            let mut expn_bytes = 0;

            // Encode all hygiene data (`SyntaxContextData` and `ExpnData`) from the current
            // session.

            HygieneEncodeContext::encode(
                &Rc::clone(&encoder.hygiene_context),
                &mut encoder,
                |encoder, ctxt, ctxt_data| {
                    let pos = AbsoluteBytePos::new(encoder.position());
                    encoder.encode_tagged(TAG_SYNTAX_CONTEXT, ctxt_data);
                    ctxt_bytes += encoder.position() - pos.to_usize();
                    syntax_contexts.insert(ctxt.as_u32(), pos);
                },
                |encoder, expn_id, data, hash| {
                    if expn_id.krate == LOCAL_CRATE {
                        let data = data.expect("local expn");
                        // Macro expansion runs in full in every session,
                        // before a query reads this cache.
                        if matches!(data.kind, ExpnKind::Macro(..) | ExpnKind::AstPass(_)) {
                            return;
                        }
                        let pos = AbsoluteBytePos::new(encoder.position());
                        encoder.encode_tagged(TAG_EXPN_DATA, data);
                        expn_bytes += encoder.position() - pos.to_usize();
                        expn_data.insert(hash, pos);
                    } else {
                        foreign_expn_data.insert(hash, expn_id.local_id.as_u32());
                    }
                },
            );

            // Encode the file footer.
            let footer_pos = encoder.position() as u64;
            let query_values_index = ValuesIndex(mem::take(&mut encoder.query_values_index));
            let side_effects_index = ValuesIndex(mem::take(&mut encoder.side_effects_index));
            let def_path_hashes: Vec<_> =
                mem::take(&mut encoder.def_path_hashes).into_iter().collect();
            let counts = (
                query_values_index.0.len(),
                def_path_hashes.len(),
                syntax_contexts.len(),
                expn_data.len(),
            );
            let footer_parts = encoder.encode_footer(&Footer {
                file_index_to_stable_id,
                query_values_index,
                side_effects_index,
                interpret_alloc_index,
                syntax_contexts,
                expn_data,
                foreign_expn_data,
                def_path_hashes: DefPathHashes(def_path_hashes),
            });
            if tcx.sess.opts.unstable_opts.incremental_info {
                let footer_end = encoder.position();
                let (values, def_paths, ctxts, expns) = counts;
                let [
                    files,
                    values_index,
                    side_effects_index,
                    allocs_index,
                    ctxts_map,
                    expns_map,
                    foreign_expns,
                    def_paths_table,
                ] = footer_parts;
                eprintln!(
                    "[incremental] query cache footer: files {files} values-index {values_index} \
                     side-effects-index {side_effects_index} allocs-index {allocs_index} \
                     ctxts {ctxts_map} expns {expns_map} foreign-expns {foreign_expns} \
                     def-paths {def_paths_table}"
                );
                eprintln!(
                    "[incremental] query cache hygiene: ctxt-data {ctxt_bytes} \
                     expn-data {expn_bytes}"
                );
                eprintln!(
                    "[incremental] query cache sections: values {} side-effects {} allocs {} \
                     hygiene {} footer {} entries {values} def-paths {def_paths} \
                     ctxts {ctxts} expns {expns}",
                    values_end - values_start,
                    side_effects_end - values_end,
                    allocs_end - side_effects_end,
                    footer_pos as usize - allocs_end,
                    footer_end - footer_pos as usize,
                );
            }

            // Encode the position of the footer as the last 8 bytes of the
            // file so we know where to look for it.
            IntEncodedWithFixedSize(footer_pos).encode(&mut encoder.encoder);

            // DO NOT WRITE ANYTHING TO THE ENCODER AFTER THIS POINT! The address
            // of the footer must be the last thing in the data stream.

            encoder.finish()
        })
    }

    /// Saves this session's values to the shared values file at `values_path`
    /// and to the pool file beside it, and its index of them to `session`.
    /// This ends up as the session's `query-cache.bin`. The entry of a green
    /// node stays where an earlier session left it. Only the values of other
    /// nodes are encoded. Their types, predicates, symbols, source files,
    /// hygiene data and allocations go to the pool, which only grows, so the
    /// positions in old entries stay valid. When more than a quarter of the
    /// values file would be dead, the live entries are copied as bytes into a
    /// fresh values file. When `inplace_keeps_entries` is false, every value
    /// is encoded into a fresh values file and a fresh pool.
    pub fn serialize_inplace(
        &self,
        tcx: TyCtxt<'_>,
        mut session: FileEncoder<'static>,
        values_path: &Path,
    ) -> FileEncodeResult {
        tcx.dep_graph.with_ignore(|| {
            let lock_path = values_path.with_extension("lock");
            // Another rustc of this crate holds the lock only while it saves, so wait for it.
            let _lock = loop {
                match flock::Lock::try_lock(&lock_path, true, true) {
                    Ok(lock) => break Some(lock),
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(err) if flock::Lock::error_unsupported(&err) => break None,
                    Err(err) => return Err((lock_path, err)),
                }
            };

            let old = self.inplace.as_ref().filter(|_| self.inplace_keeps_entries());
            let serialized_data = self.serialized_data.read();
            let pool_data = self.pool_data.read();
            let data = tcx.dep_graph.data().unwrap();
            let carry = |old: &[IndexEntry]| -> (Vec<IndexEntry>, FxHashSet<DepNodeIndex>) {
                let mut entries = Vec::with_capacity(old.len());
                let mut set = FxHashSet::default();
                for x in old {
                    if let Some(current) = data.green_index(x.index) {
                        let index = SerializedDepNodeIndex::from_curr_for_serialization(current);
                        entries.push(IndexEntry { index, ..*x });
                        set.insert(current);
                    }
                }
                (entries, set)
            };
            let (mut query_values, carried_values) =
                carry(old.map_or(&[][..], |i| &i.query_values));
            let (mut side_effects, carried_side_effects) =
                carry(old.map_or(&[][..], |i| &i.side_effects));
            let live: u64 = query_values.iter().chain(&side_effects).map(|x| x.len as u64).sum();

            // A file on disk that another session replaced since this session loaded it
            // has a different id. This session then writes a fresh copy of what it loaded.
            let on_disk = |path: &Path, magic: &[u8; 8], id: u64, len: usize| {
                read_header(path).filter(|(header, file_len)| {
                    header[..8] == magic[..]
                        && header[8..] == id.to_le_bytes()
                        && *file_len >= len as u64
                })
            };

            let pool_path = values_path.with_file_name(POOL_FILENAME);
            let pool_tmp = pool_path.with_extension("tmp");
            let pool_appended = old
                .is_some_and(|o| on_disk(&pool_path, POOL_MAGIC, o.pool_id, o.pool_len).is_some());
            let (pool, pool_id) = if pool_appended {
                let pool = FileEncoder::append(&pool_path).map_err(|e| (pool_path.clone(), e))?;
                (pool, old.unwrap().pool_id)
            } else {
                let mut pool = FileEncoder::new(&pool_tmp).map_err(|e| (pool_tmp.clone(), e))?;
                let id = new_values_id();
                pool.emit_raw_bytes(POOL_MAGIC);
                pool.emit_raw_bytes(&id.to_le_bytes());
                // The carried entries hold positions in the loaded pool, so a fresh pool
                // starts as a copy of it.
                if let (Some(o), Some(loaded)) = (old, &*pool_data) {
                    pool.emit_raw_bytes(&loaded[HEADER_LEN..o.pool_len]);
                }
                (pool, id)
            };

            let values_appended = old.is_some_and(|o| {
                on_disk(values_path, VALUES_MAGIC, o.values_id, o.values_len)
                    .is_some_and(|(_, file_len)| (file_len - live) * 4 <= file_len)
            });
            let values_tmp = values_path.with_extension("tmp");
            let (encoder, values_id) = if values_appended {
                let encoder =
                    FileEncoder::append(values_path).map_err(|e| (values_path.into(), e))?;
                (encoder, old.unwrap().values_id)
            } else {
                let mut encoder =
                    FileEncoder::new(&values_tmp).map_err(|e| (values_tmp.clone(), e))?;
                let id = new_values_id();
                encoder.emit_raw_bytes(VALUES_MAGIC);
                encoder.emit_raw_bytes(&id.to_le_bytes());
                let loaded = match (&*serialized_data, old) {
                    (Some(data), Some(o)) => &data[..o.values_len],
                    _ => &[],
                };
                for x in query_values.iter_mut().chain(side_effects.iter_mut()) {
                    let mut d = MemDecoder::new(loaded, x.pos.to_usize()).unwrap();
                    let back = d.read_usize();
                    let start = x.pos.to_usize() - back;
                    let new_start = encoder.position();
                    encoder.emit_raw_bytes(&loaded[start..start + x.len as usize]);
                    x.pos = AbsoluteBytePos::new(new_start + back);
                }
                (encoder, id)
            };

            let inplace = InplaceEncode {
                pool,
                stack: Vec::new(),
                symbols: Default::default(),
                ctxts: Default::default(),
                expns: Default::default(),
                expns_in_progress: Default::default(),
                files: Default::default(),
                carried_values,
                carried_side_effects,
                query_values: Vec::new(),
                side_effects: Vec::new(),
                reuse_decoded: old.is_some(),
            };
            let mut cache_encoder = CacheEncoder {
                tcx,
                encoder,
                type_shorthands: Default::default(),
                predicate_shorthands: Default::default(),
                interpret_allocs: Default::default(),
                caching_source_map_view: CachingSourceMapView::new(tcx.sess.source_map()),
                file_to_file_index: Default::default(),
                hygiene_context: Default::default(),
                symbol_index_table: Default::default(),
                source_span_cache: Default::default(),
                query_values_index: Default::default(),
                side_effects_index: Default::default(),
                def_path_hashes: Default::default(),
                span_bytes: 0,
                shorthand_refs: 0,
                shorthand_ref_bytes: 0,
                instance_shorthands: Default::default(),
                prev_span: PrevSpan::default(),
                prev_span_saved: Vec::new(),
                inplace: Some(Box::new(inplace)),
            };
            tcx.sess.time("encode_query_values", || tcx.encode_query_values(&mut cache_encoder));
            for (&dep_node_index, side_effect) in tcx.query_system.side_effects.borrow().iter() {
                cache_encoder.encode_side_effect(dep_node_index, side_effect);
            }
            let inplace = *cache_encoder.inplace.take().unwrap();
            query_values.extend(inplace.query_values);
            side_effects.extend(inplace.side_effects);
            let mut pool = inplace.pool;
            let pool_len = pool.finish()?;
            let values_len = cache_encoder.finish()?;
            if !pool_appended {
                std::fs::rename(&pool_tmp, &pool_path).map_err(|e| (pool_path.clone(), e))?;
            }
            if !values_appended {
                std::fs::rename(&values_tmp, values_path).map_err(|e| (values_path.into(), e))?;
            }
            let pool_base_len = old.map_or(pool_len, |o| o.pool_base_len);

            let footer_pos = session.position() as u64;
            session.emit_u128(TAG_FILE_FOOTER_INPLACE);
            session.emit_u64(values_id);
            session.emit_u64(values_len as u64);
            session.emit_u64(pool_id);
            session.emit_u64(pool_len as u64);
            session.emit_u64(pool_base_len as u64);
            encode_index_entries(&mut session, &mut query_values);
            encode_index_entries(&mut session, &mut side_effects);
            IntEncodedWithFixedSize(footer_pos).encode(&mut session);
            session.finish()
        })
    }

    /// Loads a `QuerySideEffect` created during the previous compilation session.
    pub(crate) fn load_side_effect(
        &self,
        tcx: TyCtxt<'_>,
        dep_node_index: SerializedDepNodeIndex,
    ) -> Option<QuerySideEffect> {
        let side_effect: Option<QuerySideEffect> =
            self.load_indexed(tcx, dep_node_index, &self.side_effects_index);
        side_effect
    }

    /// Returns the disk-cached query return value for the given node, if there is one.
    pub fn try_load_query_value<'tcx, T>(
        &self,
        tcx: TyCtxt<'tcx>,
        dep_node_index: SerializedDepNodeIndex,
    ) -> Option<T>
    where
        T: for<'a> Decodable<CacheDecoder<'a, 'tcx>>,
    {
        self.load_indexed(tcx, dep_node_index, &self.query_values_index)
    }

    fn load_indexed<'tcx, T>(
        &self,
        tcx: TyCtxt<'tcx>,
        dep_node_index: SerializedDepNodeIndex,
        index: &FxHashMap<SerializedDepNodeIndex, (AbsoluteBytePos, u32)>,
    ) -> Option<T>
    where
        T: for<'a> Decodable<CacheDecoder<'a, 'tcx>>,
    {
        let (pos, len) = index.get(&dep_node_index).cloned()?;
        if self.inplace.is_some() {
            return Some(self.with_entry_decoder(tcx, pos, |decoder| T::decode(decoder)));
        }
        let value = self.with_decoder(tcx, pos, |decoder| decode_value(decoder, len));
        Some(value)
    }

    /// Decodes one entry of the values file. `pos` is the position of the entry's alloc table.
    fn with_entry_decoder<'tcx, T, F: for<'s> FnOnce(&mut CacheDecoder<'s, 'tcx>) -> T>(
        &self,
        tcx: TyCtxt<'tcx>,
        pos: AbsoluteBytePos,
        f: F,
    ) -> T {
        let state = self.inplace.as_ref().unwrap();
        let serialized_data = self.serialized_data.read();
        let pool_data = self.pool_data.read();
        let data = &serialized_data.as_deref().unwrap()[..state.values_len];
        let pool = MemDecoder::new(&pool_data.as_deref().unwrap()[..state.pool_len], 0).unwrap();
        let mut table = MemDecoder::new(data, pos.to_usize()).unwrap();
        let start = pos.to_usize() - table.read_usize();
        let alloc_decoding_state = AllocDecodingState::new(Vec::<u64>::decode(&mut table));
        let mut decoder = CacheDecoder {
            tcx,
            opaque: MemDecoder::new(data, start).unwrap(),
            pool: Some(pool),
            pool_decoded: &self.pool_decoded,
            file_index_to_file: &self.file_index_to_file,
            file_index_to_stable_id: &self.file_index_to_stable_id,
            alloc_decoding_session: alloc_decoding_state.new_decoding_session(),
            syntax_contexts: &self.syntax_contexts,
            expn_data: &self.expn_data,
            foreign_expn_data: &self.foreign_expn_data,
            hygiene_context: &self.hygiene_context,
            source_span_cache: SourceSpanCache::default(),
            def_path_hashes: None,
            prev_span: PrevSpan::default(),
        };
        f(&mut decoder)
    }

    fn with_decoder<'a, 'tcx, T, F: for<'s> FnOnce(&mut CacheDecoder<'s, 'tcx>) -> T>(
        &self,
        tcx: TyCtxt<'tcx>,
        pos: AbsoluteBytePos,
        f: F,
    ) -> T
    where
        T: Decodable<CacheDecoder<'a, 'tcx>>,
    {
        let serialized_data = self.serialized_data.read();
        let mut decoder = CacheDecoder {
            tcx,
            opaque: MemDecoder::new(serialized_data.as_deref().unwrap_or(&[]), pos.to_usize())
                .unwrap(),
            pool: None,
            pool_decoded: &self.pool_decoded,
            file_index_to_file: &self.file_index_to_file,
            file_index_to_stable_id: &self.file_index_to_stable_id,
            alloc_decoding_session: self.alloc_decoding_state.new_decoding_session(),
            syntax_contexts: &self.syntax_contexts,
            expn_data: &self.expn_data,
            foreign_expn_data: &self.foreign_expn_data,
            hygiene_context: &self.hygiene_context,
            source_span_cache: SourceSpanCache::default(),
            def_path_hashes: self.inplace.is_none().then_some(&self.def_path_hashes[..]),
            prev_span: PrevSpan::default(),
        };
        f(&mut decoder)
    }
}

//- DECODING -------------------------------------------------------------------

/// A decoder that can read from the incremental compilation cache. It is similar to the one
/// we use for crate metadata decoding in that it can rebase spans and eventually
/// will also handle things that contain `Ty` instances.
pub struct CacheDecoder<'a, 'tcx> {
    tcx: TyCtxt<'tcx>,
    opaque: MemDecoder<'a>,
    /// The pool file, when `opaque` is an entry of the values file.
    pool: Option<MemDecoder<'a>>,
    pool_decoded: &'a PoolDecoded,
    file_index_to_file: &'a Lock<FxHashMap<SourceFileIndex, Arc<SourceFile>>>,
    file_index_to_stable_id: &'a FxHashMap<SourceFileIndex, EncodedSourceFileId>,
    alloc_decoding_session: AllocDecodingSession<'a>,
    syntax_contexts: &'a FxHashMap<u32, AbsoluteBytePos>,
    expn_data: &'a UnhashMap<ExpnHash, AbsoluteBytePos>,
    foreign_expn_data: &'a UnhashMap<ExpnHash, u32>,
    hygiene_context: &'a HygieneDecodeContext,
    /// The table that a `DefId` index refers to, or `None` when the bytes hold the hash itself.
    def_path_hashes: Option<&'a [DefPathHash]>,
    source_span_cache: SourceSpanCache,
    prev_span: PrevSpan,
}

impl<'a, 'tcx> CacheDecoder<'a, 'tcx> {
    #[inline]
    fn file_index_to_file(&self, index: SourceFileIndex) -> Arc<SourceFile> {
        let CacheDecoder { tcx, file_index_to_file, file_index_to_stable_id, .. } = *self;

        Arc::clone(
            file_index_to_file.borrow_mut().entry(index).or_insert_with(|| {
                Self::resolve_source_file(tcx, &file_index_to_stable_id[&index])
            }),
        )
    }

    /// The source file of a source file item of the pool at `pos`.
    fn pool_file(&mut self, pos: usize) -> Arc<SourceFile> {
        if let Some(file) = self.pool_decoded.files.lock().get(&pos) {
            return Arc::clone(file);
        }
        let id = self.with_position(pos, |d| EncodedSourceFileId::decode(d));
        let file = Self::resolve_source_file(self.tcx, &id);
        self.pool_decoded.files.lock().insert(pos, Arc::clone(&file));
        file
    }

    /// Decodes the expansion of an expansion item of the pool, at the decoder position.
    fn decode_pool_expn(&mut self) -> ExpnId {
        let hash = ExpnHash::decode(self);
        if let Some(expn_id) = ExpnId::from_hash(hash) {
            return expn_id;
        }
        let krate = self.tcx.stable_crate_id_to_crate_num(hash.stable_crate_id());
        if self.read_u8() == 1 {
            let data = ExpnData::decode(self);
            rustc_span::hygiene::register_local_expn_id(data, hash)
        } else {
            let index_guess = self.read_u32();
            self.tcx.expn_hash_to_expn_id(krate, index_guess, hash)
        }
    }

    fn resolve_source_file(
        tcx: TyCtxt<'tcx>,
        source_file_id: &EncodedSourceFileId,
    ) -> Arc<SourceFile> {
        let source_file_cnum = tcx.stable_crate_id_to_crate_num(source_file_id.stable_crate_id);

        // If this `SourceFile` is from a foreign crate, then make sure that
        // we've imported all of the source files from that crate.
        if source_file_cnum != LOCAL_CRATE {
            tcx.import_source_files(source_file_cnum);
        }

        tcx.sess
            .source_map()
            .source_file_by_stable_id(source_file_id.stable_source_file_id)
            .expect("failed to lookup `SourceFile` in new context")
    }

    // copy&paste impl from rustc_metadata
    #[inline]
    fn decode_symbol_or_byte_symbol<S>(
        &mut self,
        new_from_index: impl Fn(u32) -> S,
        read_and_intern_str_or_byte_str_this: impl Fn(&mut Self) -> S,
        read_and_intern_str_or_byte_str_opaque: impl Fn(&mut MemDecoder<'a>) -> S,
        index_of: impl Fn(&S) -> u32,
    ) -> S {
        let tag = self.read_u8();

        match tag {
            SYMBOL_STR => read_and_intern_str_or_byte_str_this(self),
            SYMBOL_OFFSET if self.pool.is_some() => {
                let pos = self.read_usize();
                let s = read_and_intern_str_or_byte_str_opaque(
                    &mut self.pool.as_ref().unwrap().split_at(pos),
                );
                self.pool_decoded.symbols.lock().insert(index_of(&s), pos);
                s
            }
            SYMBOL_OFFSET => {
                // read str offset
                let pos = self.read_usize();

                // move to str offset and read
                self.opaque.with_position(pos, |d| read_and_intern_str_or_byte_str_opaque(d))
            }
            SYMBOL_PREDEFINED => new_from_index(self.read_u32()),
            _ => unreachable!(),
        }
    }
}

// Decodes something that was encoded with `encode_tagged()` and verify that the
// tag matches and the correct amount of bytes was read.
fn decode_tagged<D, T, V>(decoder: &mut D, expected_tag: T) -> V
where
    T: Decodable<D> + Eq + fmt::Debug,
    V: Decodable<D>,
    D: Decoder,
{
    let start_pos = decoder.position();

    let actual_tag = T::decode(decoder);
    assert_eq!(actual_tag, expected_tag);
    let value = V::decode(decoder);
    let end_pos = decoder.position();

    let expected_len: u64 = Decodable::decode(decoder);
    assert_eq!((end_pos - start_pos) as u64, expected_len);

    value
}

/// Decodes a value that `encode_value` wrote. The index names the value and holds its
/// length, which catches a decoder that reads more or less than the encoder wrote.
fn decode_value<D: Decoder, V: Decodable<D>>(decoder: &mut D, len: u32) -> V {
    let start_pos = decoder.position();
    let value = V::decode(decoder);
    assert_eq!(decoder.position() - start_pos, len as usize);
    value
}

impl<'a, 'tcx> TyDecoder<'tcx> for CacheDecoder<'a, 'tcx> {
    const CLEAR_CROSS_CRATE: bool = false;

    #[inline]
    fn relative_shorthands(&self) -> bool {
        self.pool.is_none()
    }

    #[inline]
    fn instance_shorthands(&self) -> bool {
        self.pool.is_none()
    }

    fn cached_ty_for_shorthand<F>(&mut self, shorthand: usize, or_insert_with: F) -> Ty<'tcx>
    where
        F: FnOnce(&mut Self) -> Ty<'tcx>,
    {
        let tcx = self.tcx;

        let cache_key = ty::CReaderCacheKey { cnum: None, pos: shorthand };

        if let Some(&ty) = tcx.caches.ty_rcache.borrow().get(&cache_key) {
            return ty;
        }

        let ty = or_insert_with(self);
        // This may overwrite the entry, but it should overwrite with the same value.
        tcx.caches.ty_rcache.borrow_mut().insert_same(cache_key, ty);
        ty
    }

    fn with_position<F, R>(&mut self, pos: usize, f: F) -> R
    where
        F: FnOnce(&mut Self) -> R,
    {
        let new_opaque = match &self.pool {
            Some(pool) => pool.split_at(pos),
            None => {
                debug_assert!(pos < self.opaque.len());
                self.opaque.split_at(pos)
            }
        };
        let old_opaque = mem::replace(&mut self.opaque, new_opaque);
        let old_prev_span = mem::take(&mut self.prev_span);
        let r = f(self);
        self.opaque = old_opaque;
        self.prev_span = old_prev_span;
        r
    }

    fn decode_alloc_id(&mut self) -> interpret::AllocId {
        let alloc_decoding_session = self.alloc_decoding_session;
        alloc_decoding_session.decode_alloc_id(self)
    }
}

impl<'a, 'tcx> rustc_type_ir::InternerDecoder for CacheDecoder<'a, 'tcx> {
    type Interner = TyCtxt<'tcx>;

    #[inline]
    fn interner(&self) -> Self::Interner {
        self.tcx
    }
}

impl<'a, 'tcx> Decoder for CacheDecoder<'a, 'tcx> {
    forward_all_decoder_methods_to!(|self| self.opaque);
}

// This ensures that the `Decodable<opaque::Decoder>::decode` specialization for `Vec<u8>` is used
// when a `CacheDecoder` is passed to `Decodable::decode`. Unfortunately, we have to manually opt
// into specializations this way, given how `CacheDecoder` and the decoding traits currently work.
impl<'a, 'tcx> Decodable<CacheDecoder<'a, 'tcx>> for Vec<u8> {
    fn decode(d: &mut CacheDecoder<'a, 'tcx>) -> Self {
        Decodable::decode(&mut d.opaque)
    }
}

impl<'a, 'tcx> SpanDecoder for CacheDecoder<'a, 'tcx> {
    fn decode_syntax_context(&mut self) -> SyntaxContext {
        if self.pool.is_some() {
            let pos = self.read_usize();
            if pos == 0 {
                return SyntaxContext::root();
            }
            if let Some(&ctxt) = self.pool_decoded.ctxts.lock().get(&pos) {
                return ctxt;
            }
            let key = self.with_position(pos, |d| SyntaxContextKey::decode(d));
            let ctxt = rustc_span::hygiene::syntax_context_from_key(key);
            self.pool_decoded.ctxts.lock().insert(pos, ctxt);
            return ctxt;
        }
        let syntax_contexts = self.syntax_contexts;
        rustc_span::hygiene::decode_syntax_context(
            self,
            self.hygiene_context,
            |this, external_ctxt| {
                // This closure is invoked if we haven't already decoded the data for the `SyntaxContext` we are deserializing.
                // We look up the position of the associated `SyntaxData` and decode it.
                let pos = syntax_contexts.get(&external_ctxt.as_u32()).unwrap();
                this.with_position(pos.to_usize(), |decoder| {
                    let data: SyntaxContextKey = decode_tagged(decoder, TAG_SYNTAX_CONTEXT);
                    data
                })
            },
        )
    }

    fn decode_expn_id(&mut self) -> ExpnId {
        if self.pool.is_some() {
            let pos = self.read_usize();
            if pos == 0 {
                return ExpnId::root();
            }
            if let Some(&expn_id) = self.pool_decoded.expns.lock().get(&pos) {
                return expn_id;
            }
            let expn_id = self.with_position(pos, |d| d.decode_pool_expn());
            self.pool_decoded.expns.lock().insert(pos, expn_id);
            return expn_id;
        }
        let hash = ExpnHash::decode(self);
        if hash.is_root() {
            return ExpnId::root();
        }

        if let Some(expn_id) = ExpnId::from_hash(hash) {
            return expn_id;
        }

        let krate = self.tcx.stable_crate_id_to_crate_num(hash.stable_crate_id());

        let expn_id = if krate == LOCAL_CRATE {
            // We look up the position of the associated `ExpnData` and decode it.
            let pos = self
                .expn_data
                .get(&hash)
                .unwrap_or_else(|| panic!("Bad hash {:?} (map {:?})", hash, self.expn_data));

            let data: ExpnData =
                self.with_position(pos.to_usize(), |decoder| decode_tagged(decoder, TAG_EXPN_DATA));
            let expn_id = rustc_span::hygiene::register_local_expn_id(data, hash);

            #[cfg(debug_assertions)]
            {
                use rustc_data_structures::stable_hash::{StableHash, StableHasher};
                let local_hash = self.tcx.with_stable_hashing_context(|mut hcx| {
                    let mut hasher = StableHasher::new();
                    expn_id.expn_data().stable_hash(&mut hcx, &mut hasher);
                    hasher.finish()
                });
                debug_assert_eq!(hash.local_hash(), local_hash);
            }

            expn_id
        } else {
            let index_guess = self.foreign_expn_data[&hash];
            self.tcx.expn_hash_to_expn_id(krate, index_guess, hash)
        };

        debug_assert_eq!(expn_id.krate, krate);
        expn_id
    }

    fn decode_span(&mut self) -> Span {
        let head = self.read_u8();
        let prev = self.prev_span;
        let ctxt = if head & SPAN_SAME_CTXT != 0 {
            prev.ctxt.expect("span repeats the context of no earlier span")
        } else {
            SyntaxContext::decode(self)
        };
        let parent = if head & SPAN_SAME_PARENT != 0 {
            prev.parent.expect("span repeats the parent of no earlier span")
        } else {
            Option::<LocalDefId>::decode(self)
        };
        let kind = head & SPAN_KIND_MASK;

        let (lo, hi) = match kind {
            TAG_PARTIAL_SPAN => (BytePos(0), BytePos(0)),
            TAG_RELATIVE_SPAN | TAG_PREVIOUS_SPAN => {
                let zigzag = u32::decode(self);
                let dlo = ((zigzag >> 1) as i32 ^ -((zigzag & 1) as i32)) as u32;
                let len = u32::decode(self);

                let base = if kind == TAG_PREVIOUS_SPAN {
                    prev.lo.expect("span is relative to no earlier span")
                } else {
                    self.source_span_cache
                        .lookup(parent.unwrap(), &self.tcx.untracked().source_span)
                        .lo
                };
                let lo = base.0.wrapping_add(dlo);
                (BytePos(lo), BytePos(lo.wrapping_add(len)))
            }
            TAG_FULL_SPAN => {
                let file_lo_index = SourceFileIndex::decode(self);
                let line_lo = usize::decode(self);
                let col_lo = RelativeBytePos::decode(self);
                let len = BytePos::decode(self);

                let file_lo = if self.pool.is_some() {
                    self.pool_file(file_lo_index.0 as usize)
                } else {
                    self.file_index_to_file(file_lo_index)
                };
                let lo = file_lo.lines()[line_lo - 1] + col_lo;
                let lo = file_lo.absolute_position(lo);
                let hi = lo + len;
                (lo, hi)
            }
            _ => unreachable!(),
        };

        let relative = matches!(kind, TAG_RELATIVE_SPAN | TAG_PREVIOUS_SPAN);
        self.prev_span =
            PrevSpan { ctxt: Some(ctxt), parent: Some(parent), lo: relative.then_some(lo) };
        Span::new(lo, hi, ctxt, parent)
    }

    fn decode_crate_num(&mut self) -> CrateNum {
        let stable_id = StableCrateId::decode(self);
        let cnum = self.tcx.stable_crate_id_to_crate_num(stable_id);
        cnum
    }

    // Both the `CrateNum` and the `DefIndex` of a `DefId` can change in between two
    // compilation sessions. We use the `DefPathHash`, which is stable across
    // sessions, to map the old `DefId` to the new one.
    fn decode_def_id(&mut self) -> DefId {
        // Load the `DefPathHash` which is was we encoded the `DefId` as.
        let def_path_hash = match self.def_path_hashes {
            Some(table) => table[self.read_usize()],
            None => DefPathHash::decode(self),
        };

        // Using the `DefPathHash`, we can lookup the new `DefId`.
        // Subtle: We only encode a `DefId` as part of a query result.
        // If we get to this point, then all of the query inputs were green,
        // which means that the definition with this hash is guaranteed to
        // still exist in the current compilation session.
        match self.tcx.def_path_hash_to_def_id(def_path_hash) {
            Some(r) => r,
            None => panic!("Failed to convert DefPathHash {def_path_hash:?}"),
        }
    }

    fn decode_attr_id(&mut self) -> rustc_span::AttrId {
        panic!("cannot decode `AttrId` with `CacheDecoder`");
    }
}

impl<'a, 'tcx> BlobDecoder for CacheDecoder<'a, 'tcx> {
    fn decode_symbol(&mut self) -> Symbol {
        self.decode_symbol_or_byte_symbol(
            Symbol::new,
            |this| Symbol::intern(this.read_str()),
            |opaque| Symbol::intern(opaque.read_str()),
            |sym| sym.as_u32(),
        )
    }

    fn decode_byte_symbol(&mut self) -> ByteSymbol {
        self.decode_symbol_or_byte_symbol(
            ByteSymbol::new,
            |this| ByteSymbol::intern(this.read_byte_str()),
            |opaque| ByteSymbol::intern(opaque.read_byte_str()),
            |sym| sym.as_u32(),
        )
    }

    // This impl makes sure that we get a runtime error when we try decode a
    // `DefIndex` that is not contained in a `DefId`. Such a case would be problematic
    // because we would not know how to transform the `DefIndex` to the current
    // context.
    fn decode_def_index(&mut self) -> DefIndex {
        panic!("trying to decode `DefIndex` outside the context of a `DefId`")
    }

    fn decode_local_def_id(&mut self) -> LocalDefId {
        self.decode_def_id().expect_local()
    }
}

/// Implements [`Decodable`] for `&'tcx T`, where [`T: RefDecodable`](RefDecodable).
///
/// Due to orphan-rule restrictions, these foreign impls cannot use a blanket
/// [`D: TyDecoder`](TyDecoder), and must instead specify a specific decoder.
///
/// For impls on types defined in `rustc_middle`, see
/// `impl_decodable_via_ref_decodable_for_local_type!` instead.
macro_rules! impl_decodable_via_ref_decodable_for_foreign_types {
    (
        $(
            &'tcx $T:ty,
        )*
    ) => {
        $(
            impl<'tcx> Decodable<CacheDecoder<'_, 'tcx>> for &'tcx $T {
                fn decode(decoder: &mut CacheDecoder<'_, 'tcx>) -> Self {
                    RefDecodable::decode(decoder)
                }
            }
        )*
    }
}

impl_decodable_via_ref_decodable_for_foreign_types! {
    // tidy-alphabetical-start
    &'tcx IndexVec<mir::Promoted, mir::Body<'tcx>>,
    &'tcx UnordMap<DefId, ty::EarlyBinder<'tcx, Ty<'tcx>>>,
    &'tcx UnordSet<LocalDefId>,
    &'tcx [(
        rustc_middle::middle::exported_symbols::ExportedSymbol<'tcx>,
        rustc_middle::middle::exported_symbols::SymbolExportInfo,
    )],
    &'tcx [(ty::Clause<'tcx>, Span)],
    &'tcx [DefId],
    &'tcx [MonoItemUse<'tcx>],
    &'tcx [rustc_middle::middle::dead_code::DeadCodeBodyUse],
    &'tcx [rustc_middle::middle::reachable::ReachableBodyRef],
    &'tcx [ty::Variance],
    &'tcx rustc_ast::tokenstream::TokenStream,
    // tidy-alphabetical-end
}

//- ENCODING -------------------------------------------------------------------

/// An encoder that can write to the incremental compilation cache.
pub struct CacheEncoder<'tcx> {
    tcx: TyCtxt<'tcx>,
    encoder: FileEncoder<'static>,
    type_shorthands: FxHashMap<Ty<'tcx>, usize>,
    predicate_shorthands: FxHashMap<ty::PredicateKind<'tcx>, usize>,
    interpret_allocs: FxIndexSet<interpret::AllocId>,
    caching_source_map_view: CachingSourceMapView<'tcx>,
    file_to_file_index: FxHashMap<*const SourceFile, SourceFileIndex>,
    hygiene_context: Rc<RefCell<HygieneEncodeContext>>,
    // Used for both `Symbol`s and `ByteSymbol`s.
    symbol_index_table: FxHashMap<u32, usize>,
    source_span_cache: SourceSpanCache,

    query_values_index: Vec<IndexEntry>,
    side_effects_index: Vec<IndexEntry>,
    /// The `DefPathHash` of each `DefId` index written so far, outside the inplace format.
    def_path_hashes: FxIndexSet<DefPathHash>,
    /// The bytes that spans took, for `-Zincremental-info`.
    span_bytes: usize,
    /// The type and predicate shorthand references, and their bytes, for `-Zincremental-info`.
    shorthand_refs: usize,
    shorthand_ref_bytes: usize,
    instance_shorthands: FxHashMap<ty::Instance<'tcx>, usize>,
    prev_span: PrevSpan,
    /// The span state of each pool item under construction, outermost first.
    prev_span_saved: Vec<PrevSpan>,

    inplace: Option<Box<InplaceEncode>>,
}

/// The state of a session that writes entries to the values file and items to the pool.
struct InplaceEncode {
    pool: FileEncoder<'static>,
    /// The pool items that are under construction, innermost last. Every write goes to the last one.
    stack: Vec<MemEncoder>,
    /// The pool position of each item that is in the pool, by what it encodes.
    symbols: FxHashMap<u32, usize>,
    ctxts: FxHashMap<SyntaxContext, usize>,
    expns: FxHashMap<ExpnId, usize>,
    expns_in_progress: FxHashSet<ExpnId>,
    files: FxHashMap<*const SourceFile, usize>,
    /// Green nodes whose entry from an earlier session stays in use.
    carried_values: FxHashSet<DepNodeIndex>,
    carried_side_effects: FxHashSet<DepNodeIndex>,
    query_values: Vec<IndexEntry>,
    side_effects: Vec<IndexEntry>,
    /// Whether the pool items that this session decoded still have to go into the maps above.
    reuse_decoded: bool,
}

impl<'tcx> fmt::Debug for CacheEncoder<'tcx> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Add more details here if/when necessary.
        f.write_str("CacheEncoder")
    }
}

// Starts a pool item.

impl<'tcx> CacheEncoder<'tcx> {
    /// The bytes that spans took so far.
    pub fn span_bytes(&self) -> usize {
        self.span_bytes
    }

    /// The type and predicate shorthand references so far, and their bytes.
    pub fn shorthand_refs(&self) -> (usize, usize) {
        (self.shorthand_refs, self.shorthand_ref_bytes)
    }

    #[inline]
    fn source_file_index(&mut self, source_file: Arc<SourceFile>) -> SourceFileIndex {
        if self.inplace.is_some() {
            let key = Arc::as_ptr(&source_file);
            let pos = match self.inplace.as_ref().unwrap().files.get(&key) {
                Some(&pos) => pos,
                None => {
                    self.begin_pool_item();
                    EncodedSourceFileId::new(self.tcx, &source_file).encode(self);
                    let pos = self.end_pool_item();
                    self.inplace.as_mut().unwrap().files.insert(key, pos);
                    pos
                }
            };
            return SourceFileIndex(pos.try_into().expect("query pool over 4 GiB"));
        }
        self.file_to_file_index[&(&raw const *source_file)]
    }

    /// Records the pool items that this session decoded, so that they are reused rather
    /// than written to the pool again.
    fn reuse_decoded_pool_items(&mut self) {
        let tcx = self.tcx;
        let decoded = &tcx.query_system.on_disk_cache.as_ref().unwrap().pool_decoded;
        let inplace = self.inplace.as_mut().unwrap();
        #[allow(rustc::potential_query_instability)]
        inplace.symbols.extend(decoded.symbols.lock().iter().map(|(&sym, &pos)| (sym, pos)));
        #[allow(rustc::potential_query_instability)]
        for (&pos, &ctxt) in decoded.ctxts.lock().iter() {
            inplace.ctxts.insert(ctxt, pos);
        }
        #[allow(rustc::potential_query_instability)]
        for (&pos, &expn) in decoded.expns.lock().iter() {
            inplace.expns.insert(expn, pos);
        }
        #[allow(rustc::potential_query_instability)]
        for (&pos, file) in decoded.files.lock().iter() {
            inplace.files.insert(Arc::as_ptr(file), pos);
        }
        #[allow(rustc::potential_query_instability)]
        for (key, &ty) in tcx.caches.ty_rcache.borrow().iter() {
            if key.cnum.is_none() {
                self.type_shorthands.insert(ty, key.pos + SHORTHAND_OFFSET);
            }
        }
    }

    /// Starts a pool item. Writes go to it until `end_pool_item`.
    fn begin_pool_item(&mut self) {
        let prev_span = mem::take(&mut self.prev_span);
        self.prev_span_saved.push(prev_span);
        self.inplace.as_mut().unwrap().stack.push(MemEncoder::new());
    }

    /// Writes the innermost pool item to the pool and returns its pool position.
    fn end_pool_item(&mut self) -> usize {
        self.prev_span = self.prev_span_saved.pop().unwrap();
        let inplace = self.inplace.as_mut().unwrap();
        let bytes = inplace.stack.pop().unwrap().finish();
        let pos = inplace.pool.position();
        inplace.pool.emit_raw_bytes(&bytes);
        pos
    }

    #[inline]
    fn top_item(&mut self) -> Option<&mut MemEncoder> {
        self.inplace.as_mut().and_then(|inplace| inplace.stack.last_mut())
    }

    /// Writes `value` as one entry: the value, then a back pointer to the entry start, then
    /// the pool positions of its allocations. Returns the position of the back pointer and
    /// the entry length.
    fn encode_entry<V: Encodable<Self>>(&mut self, value: &V) -> (AbsoluteBytePos, u32) {
        if std::mem::take(&mut self.inplace.as_mut().unwrap().reuse_decoded) {
            self.reuse_decoded_pool_items();
        }
        let start = self.encoder.position();
        self.interpret_allocs.clear();
        self.prev_span = PrevSpan::default();
        value.encode(self);
        let mut interpret_alloc_index = Vec::new();
        let mut idx = 0;
        // Encoding an allocation can reference further allocations.
        while idx < self.interpret_allocs.len() {
            let id = self.interpret_allocs[idx];
            self.begin_pool_item();
            let tcx = self.tcx;
            interpret::specialized_encode_alloc_id(self, tcx, id);
            interpret_alloc_index.push(self.end_pool_item() as u64);
            idx += 1;
        }
        let table_pos = self.encoder.position();
        self.encoder.emit_usize(table_pos - start);
        interpret_alloc_index.encode(&mut self.encoder);
        let len = self.encoder.position() - start;
        (AbsoluteBytePos::new(table_pos), len.try_into().expect("query value over 4 GiB"))
    }

    /// Encode something with additional information that allows to do some
    /// sanity checks when decoding the data again. This method will first
    /// encode the specified tag, then the given value, then the number of
    /// bytes taken up by tag and value. On decoding, we can then verify that
    /// we get the expected tag and read the expected number of bytes.
    /// Writes `footer` as `encode_tagged` does, and returns the bytes of each field.
    fn encode_footer(&mut self, footer: &Footer) -> [usize; 8] {
        let start_pos = self.position();
        self.prev_span = PrevSpan::default();
        TAG_FILE_FOOTER.encode(self);
        let fields: [&dyn Fn(&mut Self); 8] = [
            &|e| footer.file_index_to_stable_id.encode(e),
            &|e| footer.query_values_index.encode(e),
            &|e| footer.side_effects_index.encode(e),
            &|e| footer.interpret_alloc_index.encode(e),
            &|e| footer.syntax_contexts.encode(e),
            &|e| footer.expn_data.encode(e),
            &|e| footer.foreign_expn_data.encode(e),
            &|e| footer.def_path_hashes.encode(e),
        ];
        let sizes = fields.map(|field| {
            let at = self.position();
            field(self);
            self.position() - at
        });
        let end_pos = self.position();
        ((end_pos - start_pos) as u64).encode(self);
        sizes
    }

    fn encode_tagged<T: Encodable<Self>, V: Encodable<Self>>(&mut self, tag: T, value: &V) {
        let start_pos = self.position();
        self.prev_span = PrevSpan::default();

        tag.encode(self);
        value.encode(self);

        let end_pos = self.position();
        ((end_pos - start_pos) as u64).encode(self);
    }

    /// Writes `value` for `decode_value`, and returns its index entry.
    fn encode_value<V: Encodable<Self>>(
        &mut self,
        index: SerializedDepNodeIndex,
        value: &V,
    ) -> IndexEntry {
        let start_pos = self.position();
        self.prev_span = PrevSpan::default();
        value.encode(self);
        let len = (self.position() - start_pos).try_into().expect("query value over 4 GiB");
        IndexEntry { index, pos: AbsoluteBytePos::new(start_pos), len }
    }

    pub fn encode_query_value<V: Encodable<Self>>(&mut self, index: DepNodeIndex, value: &V) {
        if self.inplace.as_ref().is_some_and(|i| i.carried_values.contains(&index)) {
            return;
        }
        let index = SerializedDepNodeIndex::from_curr_for_serialization(index);
        if self.inplace.is_some() {
            let (pos, len) = self.encode_entry(value);
            self.inplace.as_mut().unwrap().query_values.push(IndexEntry { index, pos, len });
            return;
        }

        let entry = self.encode_value(index, value);
        self.query_values_index.push(entry);
    }

    fn encode_side_effect(&mut self, index: DepNodeIndex, side_effect: &QuerySideEffect) {
        if self.inplace.as_ref().is_some_and(|i| i.carried_side_effects.contains(&index)) {
            return;
        }
        let index = SerializedDepNodeIndex::from_curr_for_serialization(index);
        if self.inplace.is_some() {
            let (pos, len) = self.encode_entry(side_effect);
            self.inplace.as_mut().unwrap().side_effects.push(IndexEntry { index, pos, len });
            return;
        }

        let entry = self.encode_value(index, side_effect);
        self.side_effects_index.push(entry);
    }

    // copy&paste impl from rustc_metadata
    fn encode_symbol_or_byte_symbol(
        &mut self,
        index: u32,
        emit_str_or_byte_str: impl Fn(&mut Self),
    ) {
        // if symbol/byte symbol is predefined, emit tag and symbol index
        if Symbol::is_predefined(index) {
            self.emit_u8(SYMBOL_PREDEFINED);
            self.emit_u32(index);
        } else if let Some(inplace) = &self.inplace {
            let pos = match inplace.symbols.get(&index) {
                Some(&pos) => pos,
                None => {
                    self.begin_pool_item();
                    emit_str_or_byte_str(self);
                    let pos = self.end_pool_item();
                    self.inplace.as_mut().unwrap().symbols.insert(index, pos);
                    pos
                }
            };
            self.emit_u8(SYMBOL_OFFSET);
            self.emit_usize(pos);
        } else {
            // otherwise write it as string or as offset to it
            match self.symbol_index_table.entry(index) {
                Entry::Vacant(o) => {
                    self.encoder.emit_u8(SYMBOL_STR);
                    let pos = self.encoder.position();
                    o.insert(pos);
                    emit_str_or_byte_str(self);
                }
                Entry::Occupied(o) => {
                    let x = *o.get();
                    self.emit_u8(SYMBOL_OFFSET);
                    self.emit_usize(x);
                }
            }
        }
    }

    #[inline]
    fn finish(mut self) -> FileEncodeResult {
        self.encoder.finish()
    }
}

impl<'tcx> SpanEncoder for CacheEncoder<'tcx> {
    fn encode_syntax_context(&mut self, syntax_context: SyntaxContext) {
        if let Some(inplace) = &self.inplace {
            // Zero is the root context, and no pool item is at position zero.
            let pos = if syntax_context.is_root() {
                0
            } else if let Some(&pos) = inplace.ctxts.get(&syntax_context) {
                pos
            } else {
                let key = rustc_span::hygiene::syntax_context_key(syntax_context);
                self.begin_pool_item();
                key.encode(self);
                let pos = self.end_pool_item();
                self.inplace.as_mut().unwrap().ctxts.insert(syntax_context, pos);
                pos
            };
            return self.emit_usize(pos);
        }
        self.hygiene_context.borrow_mut().enqueue_syntax_context_for_encoding(syntax_context);
        self.emit_u32(syntax_context.as_u32());
    }

    fn encode_expn_id(&mut self, expn_id: ExpnId) {
        if let Some(inplace) = &self.inplace {
            // Zero is the root expansion, and no pool item is at position zero.
            let pos = if expn_id == ExpnId::root() {
                0
            } else if let Some(&pos) = inplace.expns.get(&expn_id) {
                pos
            } else {
                if !self.inplace.as_mut().unwrap().expns_in_progress.insert(expn_id) {
                    bug!("the data of expansion {expn_id:?} references the expansion itself");
                }
                self.begin_pool_item();
                expn_id.expn_hash().encode(self);
                if expn_id.krate == LOCAL_CRATE {
                    self.emit_u8(1);
                    expn_id.expn_data().encode(self);
                } else {
                    self.emit_u8(0);
                    self.emit_u32(expn_id.local_id.as_u32());
                }
                let pos = self.end_pool_item();
                let inplace = self.inplace.as_mut().unwrap();
                inplace.expns_in_progress.remove(&expn_id);
                inplace.expns.insert(expn_id, pos);
                pos
            };
            return self.emit_usize(pos);
        }
        self.hygiene_context.borrow_mut().schedule_expn_data_for_encoding(expn_id);
        expn_id.expn_hash().encode(self);
    }

    fn encode_span(&mut self, span: Span) {
        enum Pos {
            Partial,
            /// The start of the parent.
            Relative(BytePos),
            Full(SourceFileIndex, usize, RelativeBytePos),
        }

        let start = self.position();
        let span_data = span.data_untracked();
        let prev = self.prev_span;
        let parent = span_data
            .parent
            .map(|parent| self.source_span_cache.lookup(parent, &self.tcx.untracked().source_span));
        let pos = if span_data.is_dummy() {
            Pos::Partial
        } else if let Some(parent) = parent
            && parent.contains(span_data)
        {
            Pos::Relative(parent.lo)
        } else {
            match self.caching_source_map_view.byte_pos_to_line_and_col(span_data.lo) {
                None => Pos::Partial,
                Some((file_lo, ..)) if parent.is_some_and(|parent| file_lo.contains(parent.lo)) => {
                    Pos::Relative(parent.unwrap().lo)
                }
                Some((file_lo, line_lo, col_lo)) => {
                    Pos::Full(self.source_file_index(file_lo), line_lo, col_lo)
                }
            }
        };

        let same_ctxt = prev.ctxt == Some(span_data.ctxt);
        let same_parent = prev.parent == Some(span_data.parent);
        let (kind, base) = match pos {
            Pos::Partial => (TAG_PARTIAL_SPAN, None),
            Pos::Full(..) => (TAG_FULL_SPAN, None),
            Pos::Relative(parent_lo) => match prev.lo.filter(|_| same_parent) {
                Some(prev_lo)
                    if prev_lo.0.abs_diff(span_data.lo.0)
                        < parent_lo.0.abs_diff(span_data.lo.0) =>
                {
                    (TAG_PREVIOUS_SPAN, Some(prev_lo))
                }
                _ => (TAG_RELATIVE_SPAN, Some(parent_lo)),
            },
        };
        let mut head = kind;
        if same_ctxt {
            head |= SPAN_SAME_CTXT;
        }
        if same_parent {
            head |= SPAN_SAME_PARENT;
        }
        head.encode(self);
        if !same_ctxt {
            span_data.ctxt.encode(self);
        }
        if !same_parent {
            span_data.parent.encode(self);
        }
        match pos {
            Pos::Partial => {}
            // A span can start before its base, so the offset is zigzag. The end is a length.
            Pos::Relative(_) => {
                let dlo = span_data.lo.0.wrapping_sub(base.unwrap().0) as i32;
                (((dlo << 1) ^ (dlo >> 31)) as u32).encode(self);
                span_data.hi.0.wrapping_sub(span_data.lo.0).encode(self);
            }
            Pos::Full(source_file_index, line_lo, col_lo) => {
                source_file_index.encode(self);
                line_lo.encode(self);
                col_lo.encode(self);
                (span_data.hi - span_data.lo).encode(self);
            }
        }
        self.prev_span = PrevSpan {
            ctxt: Some(span_data.ctxt),
            parent: Some(span_data.parent),
            lo: base.map(|_| span_data.lo),
        };
        self.span_bytes += self.position().saturating_sub(start);
    }

    fn encode_symbol(&mut self, sym: Symbol) {
        self.encode_symbol_or_byte_symbol(sym.as_u32(), |this| this.emit_str(sym.as_str()));
    }

    fn encode_byte_symbol(&mut self, byte_sym: ByteSymbol) {
        self.encode_symbol_or_byte_symbol(byte_sym.as_u32(), |this| {
            this.emit_byte_str(byte_sym.as_byte_str())
        });
    }

    fn encode_crate_num(&mut self, crate_num: CrateNum) {
        self.tcx.stable_crate_id(crate_num).encode(self);
    }

    fn encode_def_id(&mut self, def_id: DefId) {
        let hash = self.tcx.def_path_hash(def_id);
        if self.inplace.is_some() {
            // An inplace entry outlives this session's footer, so it carries the hash itself.
            hash.encode(self);
            return;
        }
        let (index, _) = self.def_path_hashes.insert_full(hash);
        self.emit_usize(index);
    }

    fn encode_def_index(&mut self, _def_index: DefIndex) {
        bug!("encoding `DefIndex` without context");
    }
}

impl<'tcx> TyEncoder<'tcx> for CacheEncoder<'tcx> {
    const CLEAR_CROSS_CRATE: bool = false;

    #[inline]
    fn position(&self) -> usize {
        match self.inplace.as_ref().and_then(|inplace| inplace.stack.last()) {
            Some(item) => item.position(),
            None => self.encoder.position(),
        }
    }
    #[inline]
    fn type_shorthands(&mut self) -> &mut FxHashMap<Ty<'tcx>, usize> {
        &mut self.type_shorthands
    }
    #[inline]
    fn predicate_shorthands(&mut self) -> &mut FxHashMap<ty::PredicateKind<'tcx>, usize> {
        &mut self.predicate_shorthands
    }
    #[inline]
    fn note_shorthand_ref(&mut self, bytes: usize) {
        self.shorthand_refs += 1;
        self.shorthand_ref_bytes += bytes;
    }
    /// A shorthand of the inplace format points into the pool, where a distance from the
    /// value stream means nothing.
    #[inline]
    fn relative_shorthands(&self) -> bool {
        self.inplace.is_none()
    }
    #[inline]
    fn encode_alloc_id(&mut self, alloc_id: &interpret::AllocId) {
        let (index, _) = self.interpret_allocs.insert_full(*alloc_id);

        index.encode(self);
    }

    fn begin_full_encoding(&mut self) -> bool {
        if self.inplace.is_none() {
            return false;
        }
        self.begin_pool_item();
        true
    }

    fn end_full_encoding(&mut self) -> usize {
        self.end_pool_item()
    }

    fn instance_shorthands(&mut self) -> Option<&mut FxHashMap<ty::Instance<'tcx>, usize>> {
        if self.inplace.is_some() {
            return None;
        }
        Some(&mut self.instance_shorthands)
    }
}

macro_rules! encoder_methods {
    ($($name:ident($ty:ty);)*) => {
        $(
            #[inline]
            fn $name(&mut self, value: $ty) {
                match self.top_item() {
                    Some(item) => item.$name(value),
                    None => self.encoder.$name(value),
                }
            }
        )*
    }
}

impl<'tcx> Encoder for CacheEncoder<'tcx> {
    encoder_methods! {
        emit_usize(usize);
        emit_u128(u128);
        emit_u64(u64);
        emit_u32(u32);
        emit_u16(u16);
        emit_u8(u8);

        emit_isize(isize);
        emit_i128(i128);
        emit_i64(i64);
        emit_i32(i32);
        emit_i16(i16);

        emit_raw_bytes(&[u8]);
    }
}

// This ensures that the `Encodable<opaque::FileEncoder>::encode` specialization for byte slices
// is used when a `CacheEncoder` having an `opaque::FileEncoder` is passed to `Encodable::encode`.
// Unfortunately, we have to manually opt into specializations this way, given how `CacheEncoder`
// and the encoding traits currently work.
impl<'tcx> Encodable<CacheEncoder<'tcx>> for [u8] {
    fn encode(&self, e: &mut CacheEncoder<'tcx>) {
        match e.top_item() {
            Some(item) => self.encode(item),
            None => self.encode(&mut e.encoder),
        }
    }
}
