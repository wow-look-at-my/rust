//! Reads MIR back out of the `.rmeta`.

use std::cell::{Cell, RefCell};
use std::num::NonZero;

use rustc_data_structures::fx::{FxHashMap, FxHashSet};
use rustc_hashes::Hash64;
use rustc_hir::def_id::{DefId, DefIndex, DefPathHash, LocalDefId, StableCrateId};
use rustc_hir::{HirId, ItemLocalId};
use rustc_middle::mir;
use rustc_middle::ty::{self, Ty, TyCtxt};
use rustc_span::hygiene::ExpnId;
use rustc_span::{ExpnHash, SourceFile, SourceFileHash, Span, StableSourceFileId};

use super::*;

/// The tables of the session whose entries can be decoded again.
#[derive(Copy, Clone, Debug)]
pub(crate) enum PrevBody {
    OptimizedMir,
    MirForCtfe,
}

/// A local file of the session, matched to the same file in this session.
struct MatchedFile {
    current: Arc<SourceFile>,
    /// The source text is byte-identical, so every position in the file is still valid.
    unchanged: bool,
}

/// The item whose MIR is being decoded. Old local spans inside it move by `delta`.
#[derive(Copy, Clone)]
struct ItemScope {
    /// The current extent of the item.
    lo: BytePos,
    hi: BytePos,
    delta: i64,
}

pub(crate) struct PrevSession<'tcx> {
    tcx: TyCtxt<'tcx>,
    blob: MetadataBlob,
    root: CrateRoot,
    stable_crate_id: StableCrateId,
    def_path_hash_map: DefPathHashMapRef<'static>,
    alloc_decoding_state: AllocDecodingState,
    hygiene_context: RefCell<HygieneDecodeContext>,
    ty_cache: RefCell<FxHashMap<usize, Ty<'tcx>>>,
    /// Old file index to the matching file of this session, `None` when it has no match.
    files: RefCell<FxHashMap<u32, Option<Arc<MatchedFile>>>>,
    current_files: RefCell<Option<FxHashMap<StableSourceFileId, Arc<SourceFile>>>>,
    scope: Cell<Option<ItemScope>>,
    rejected: Cell<bool>,
    poisoned: Cell<bool>,
    /// Set when the item being decoded names an expansion that this session does not have.
    expn_missing: Cell<bool>,
    /// The HIR owner of the item being decoded, which HIR lowering gives as the parent of its spans.
    span_parent: Cell<Option<LocalDefId>>,
    /// Set when a dependency changed since the session.
    remap: Option<ForeignRemap>,
    pub(crate) reused: Cell<usize>,
    pub(crate) attempted: Cell<usize>,
    pub(crate) requested: Cell<usize>,
}

/// The stable ids of the positions the session wrote into the dependencies that changed.
struct ForeignRemap {
    changed: FxHashSet<CrateNum>,
    def_ids: FxHashMap<(u32, u32), DefPathHash>,
    expns: FxHashMap<(u32, u32), ExpnHash>,
    files: FxHashMap<(u32, u32), (StableSourceFileId, SourceFileHash)>,
    /// Per changed crate: file id to the index and source hash of that file today.
    current_files:
        RefCell<FxHashMap<CrateNum, FxHashMap<StableSourceFileId, (u32, SourceFileHash)>>>,
}

impl<'tcx> PrevSession<'tcx> {
    /// Opens the metadata. Returns `None` when its list of crates differs
    /// from this, since encoded crate numbers then name other crates. Returns
    /// `None` when a dependency changed and the file has no table of foreign
    /// ids to read its positions through.
    pub(crate) fn load(tcx: TyCtxt<'tcx>, path: &Path) -> Option<PrevSession<'tcx>> {
        let file = std::fs::File::open(path).ok()?;
        // SAFETY: the incremental session directory owns this file. Nothing writes to it now.
        let mmap = unsafe { rustc_data_structures::memmap::Mmap::map(file) }.ok()?;
        let blob = MetadataBlob::new(rustc_data_structures::owned_slice::slice_owned(
            mmap,
            std::ops::Deref::deref,
        ))
        .ok()?;
        if blob.check_compatibility(tcx.sess.cfg_version).is_err() {
            return None;
        }
        let root = blob.get_root();
        let stable_crate_id = tcx.stable_crate_id(LOCAL_CRATE);
        if root.is_proc_macro_crate() || root.stable_crate_id != stable_crate_id {
            return None;
        }

        let old_deps: Vec<CrateDep> = root.crate_deps.decode(&blob).collect();
        let crates = tcx.crates(());
        if old_deps.len() != crates.len()
            || !old_deps
                .iter()
                .zip(crates.iter())
                .all(|(dep, &cnum)| dep.name == tcx.crate_name(cnum))
        {
            return None;
        }
        // A dependency that changed can move its definitions, expansions and files. Positions
        // into it go through the table of stable ids instead.
        let changed: FxHashSet<CrateNum> = old_deps
            .iter()
            .zip(crates.iter())
            .filter(|&(dep, &cnum)| dep.hash != tcx.crate_hash(cnum))
            .map(|(_, &cnum)| cnum)
            .collect();
        let alloc_decoding_state =
            AllocDecodingState::new(root.interpret_alloc_index.decode(&blob).collect());
        let def_path_hash_map = root.def_path_hash_map.decode(&blob);

        let mut session = PrevSession {
            tcx,
            blob,
            root,
            stable_crate_id,
            def_path_hash_map,
            alloc_decoding_state,
            hygiene_context: Default::default(),
            ty_cache: Default::default(),
            files: Default::default(),
            current_files: Default::default(),
            scope: Cell::new(None),
            rejected: Cell::new(false),
            poisoned: Cell::new(false),
            expn_missing: Cell::new(false),
            span_parent: Cell::new(None),
            remap: None,
            reused: Cell::new(0),
            attempted: Cell::new(0),
            requested: Cell::new(0),
        };
        if changed.is_empty() {
            return Some(session);
        }

        let refs: ForeignRefs = session.decode_at(session.root.foreign_refs.position);
        if refs.def_ids.is_empty() {
            return None;
        }
        let is_changed = |cnum: u32| changed.contains(&CrateNum::from_u32(cnum));
        session.remap = Some(ForeignRemap {
            def_ids: refs
                .def_ids
                .into_iter()
                .filter(|&(cnum, ..)| is_changed(cnum))
                .map(|(cnum, index, hash)| ((cnum, index), hash))
                .collect(),
            expns: refs
                .expns
                .into_iter()
                .filter(|&(cnum, ..)| is_changed(cnum))
                .map(|(cnum, index, hash)| ((cnum, index), hash))
                .collect(),
            files: refs
                .files
                .into_iter()
                .filter(|&(cnum, ..)| is_changed(cnum))
                .map(|(cnum, index, id, hash)| ((cnum, index), (id, hash)))
                .collect(),
            changed,
            current_files: Default::default(),
        });
        Some(session)
    }

    fn decoder(&self, pos: usize) -> PrevDecoder<'_, 'tcx> {
        PrevDecoder {
            opaque: MemDecoder::new(&self.blob, pos).unwrap(),
            lazy_state: LazyState::NoNode,
            prev: self,
            alloc_session: self.alloc_decoding_state.new_decoding_session(),
        }
    }

    fn decode_at<'a, R: Decodable<PrevDecoder<'a, 'tcx>>>(&'a self, pos: NonZero<usize>) -> R {
        let mut dcx = self.decoder(pos.get());
        dcx.lazy_state = LazyState::NodeStart(pos);
        R::decode(&mut dcx)
    }

    /// The index the session gave to `def_id`.
    fn old_index(&self, def_id: LocalDefId) -> Option<DefIndex> {
        self.def_path_hash_map.def_path_hash_to_def_index(&self.tcx.def_path_hash(def_id.into()))
    }

    /// Maps a definition of another crate, named by its index at the time of the session.
    fn map_foreign_index(&self, krate: CrateNum, old: DefIndex) -> DefIndex {
        let Some(remap) = &self.remap else { return old };
        if !remap.changed.contains(&krate) {
            return old;
        }
        let found = remap
            .def_ids
            .get(&(krate.as_u32(), old.as_u32()))
            .and_then(|&hash| self.tcx.def_path_hash_to_def_id(hash))
            .filter(|def_id| def_id.krate == krate);
        match found {
            Some(def_id) => def_id.index,
            // A cached type can hold this definition, so no later item can be trusted.
            None => {
                self.poisoned.set(true);
                old
            }
        }
    }

    /// Maps an expansion of another crate, named by its index at the time of the session.
    fn map_foreign_expn(&self, krate: CrateNum, old: u32) -> Option<u32> {
        let Some(remap) = &self.remap else { return Some(old) };
        if !remap.changed.contains(&krate) {
            return Some(old);
        }
        let &hash = remap.expns.get(&(krate.as_u32(), old))?;
        let cstore = CStore::from_tcx(self.tcx);
        cstore.get_crate_data(krate).find_expn_index(self.tcx, hash).map(|index| index.as_u32())
    }

    /// Maps a file of another crate, named by its index in that crate's source map at the time
    /// of the session. `None` when the file changed, since a position in it can then be wrong.
    fn map_foreign_file(&self, krate: CrateNum, old: u32) -> Option<u32> {
        let Some(remap) = &self.remap else { return Some(old) };
        if !remap.changed.contains(&krate) {
            return Some(old);
        }
        let &(stable_id, src_hash) = remap.files.get(&(krate.as_u32(), old))?;
        let mut current_files = remap.current_files.borrow_mut();
        let files = current_files.entry(krate).or_insert_with(|| {
            let tcx = self.tcx;
            let cstore = CStore::from_tcx(tcx);
            let cdata = cstore.get_crate_data(krate);
            (0..cdata.root.source_map.size() as u32)
                .map(|index| {
                    let file = cdata.imported_source_file(tcx, index).translated_source_file;
                    (file.stable_id, (index, file.src_hash))
                })
                .collect()
        });
        let &(index, current_hash) = files.get(&stable_id)?;
        (current_hash == src_hash).then_some(index)
    }

    fn map_local_index(&self, old: DefIndex) -> DefIndex {
        let local_hash = self.root.tables.def_path_hashes.get(&self.blob, old);
        let hash = DefPathHash::new(self.stable_crate_id, Hash64::new(local_hash));
        match self.tcx.def_path_hash_to_def_id(hash) {
            Some(def_id) => def_id.index,
            None => bug!("definition {old:?} of the previous metadata no longer exists"),
        }
    }

    /// Decodes the body that the session stored for `def_id`. Returns `None`
    /// when the file has no such body, or when one of its spans cannot be
    /// placed in this session.
    pub(crate) fn body(&self, kind: PrevBody, def_id: LocalDefId) -> Option<mir::Body<'tcx>> {
        let old = self.old_index(def_id)?;
        let lazy = match kind {
            PrevBody::OptimizedMir => self.root.tables.optimized_mir.get(&self.blob, old),
            PrevBody::MirForCtfe => self.root.tables.mir_for_ctfe.get(&self.blob, old),
        }?;
        let mut body: mir::Body<'tcx> =
            self.with_item_scope(def_id, old, || self.decode_at(lazy.position))?;
        if let PrevBody::OptimizedMir = kind {
            self.set_lint_roots(def_id, old, &mut body);
        }
        Some(body)
    }

    /// Metadata clears the lint root of each source scope. The crate wrote them to a side
    /// table, and this puts them back, so that the body is the one the crate built.
    fn set_lint_roots(&self, def_id: LocalDefId, old: DefIndex, body: &mut mir::Body<'tcx>) {
        let Some(lazy) = self.root.tables.optimized_mir_lint_roots.get(&self.blob, old) else {
            return;
        };
        let mut left = lazy.num_elems;
        if left == 0 {
            return;
        }
        let mut dcx = self.decoder(lazy.position.get());
        let owner = self.tcx.local_def_id_to_hir_id(def_id).owner;
        mir_lint_root_bodies_mut(body, &mut |body| {
            for scope in body.source_scopes.iter_mut() {
                if left == 0 {
                    return;
                }
                left -= 1;
                let local_id = u32::decode(&mut dcx);
                if local_id != u32::MAX {
                    scope.local_data = mir::ClearCrossCrate::Set(mir::SourceScopeLocalData {
                        lint_root: HirId { owner, local_id: ItemLocalId::from_u32(local_id) },
                    });
                }
            }
        });
    }

    pub(crate) fn promoted(
        &self,
        def_id: LocalDefId,
    ) -> Option<IndexVec<mir::Promoted, mir::Body<'tcx>>> {
        let old = self.old_index(def_id)?;
        let lazy = self.root.tables.promoted_mir.get(&self.blob, old)?;
        self.with_item_scope(def_id, old, || self.decode_at(lazy.position))
    }

    fn with_item_scope<R>(
        &self,
        def_id: LocalDefId,
        old: DefIndex,
        f: impl FnOnce() -> R,
    ) -> Option<R> {
        self.attempted.set(self.attempted.get() + 1);
        if self.poisoned.get() {
            return None;
        }
        self.scope.set(self.item_scope(def_id, old));
        // The stable hash of a span with a parent is relative to the parent.
        let parent = self
            .tcx
            .sess
            .opts
            .incremental
            .is_some()
            .then(|| self.tcx.local_def_id_to_hir_id(def_id).owner.def_id);
        self.span_parent.set(parent);
        self.rejected.set(false);
        let r = f();
        self.scope.set(None);
        self.span_parent.set(None);
        if self.expn_missing.replace(false) {
            // The hygiene cache can hold contexts built on the root fallback.
            *self.hygiene_context.borrow_mut() = Default::default();
            self.rejected.set(false);
            return None;
        }
        if self.rejected.replace(false) || self.poisoned.get() {
            return None;
        }
        self.reused.set(self.reused.get() + 1);
        Some(r)
    }

    /// Finds how far the item moved: its `def_span` in the earlier file against the current one.
    fn item_scope(&self, def_id: LocalDefId, old: DefIndex) -> Option<ItemScope> {
        let lazy = self.root.tables.def_span.get(&self.blob, old)?;
        let mut dcx = self.decoder(lazy.position.get());
        dcx.lazy_state = LazyState::NodeStart(lazy.position);
        let RawSpan::Local { file, lo: old_lo, hi: old_hi } = dcx.decode_raw_span() else {
            return None;
        };
        let file = self.matched_file(file)?;

        let tcx = self.tcx;
        let new_head = tcx.def_span(def_id).data();
        let extent = tcx.source_span(def_id).data();
        if !file.current.contains(new_head.lo) || !file.current.contains(extent.lo) {
            return None;
        }
        let new_lo = (new_head.lo - file.current.start_pos).0;
        let new_hi = (new_head.hi - file.current.start_pos).0;
        if new_hi - new_lo != old_hi - old_lo {
            return None;
        }
        Some(ItemScope {
            lo: extent.lo,
            hi: extent.hi,
            delta: i64::from(new_lo) - i64::from(old_lo),
        })
    }

    fn matched_file(&self, index: u32) -> Option<Arc<MatchedFile>> {
        if let Some(found) = self.files.borrow().get(&index) {
            return found.clone();
        }
        let matched = self.match_file(index).map(Arc::new);
        self.files.borrow_mut().insert(index, matched.clone());
        matched
    }

    fn match_file(&self, index: u32) -> Option<MatchedFile> {
        let lazy = self.root.source_map.get(&self.blob, index)?;
        let old: SourceFile = self.decode_at(lazy.position);
        let mut current_files = self.current_files.borrow_mut();
        let current_files = current_files.get_or_insert_with(|| {
            let source_map = self.tcx.sess.source_map();
            source_map
                .files()
                .iter()
                .filter(|file| !file.is_imported())
                .map(|file| (exported_stable_id(file, self.stable_crate_id), Arc::clone(file)))
                .collect()
        });
        let current = Arc::clone(current_files.get(&old.stable_id)?);
        let unchanged = current.src_hash == old.src_hash
            && current.normalized_source_len == old.normalized_source_len;
        Some(MatchedFile { current, unchanged })
    }

    /// Places an old local span in this session, or rejects the item being decoded.
    fn place_local_span(&self, file: u32, lo: u32, hi: u32) -> (BytePos, BytePos) {
        let Some(file) = self.matched_file(file) else {
            self.rejected.set(true);
            return (BytePos(0), BytePos(0));
        };
        let start = file.current.start_pos;
        if file.unchanged {
            return (start + BytePos(lo), start + BytePos(hi));
        }
        if let Some(scope) = self.scope.get() {
            let lo = i64::from(start.0) + i64::from(lo) + scope.delta;
            let hi = i64::from(start.0) + i64::from(hi) + scope.delta;
            if let (Ok(lo), Ok(hi)) = (u32::try_from(lo), u32::try_from(hi))
                && scope.lo.0 <= lo
                && hi <= scope.hi.0
            {
                return (BytePos(lo), BytePos(hi));
            }
        }
        self.rejected.set(true);
        (start, start)
    }
}

/// Visits a body and the coroutine drop bodies inside it, in the order of the
/// `optimized_mir_lint_roots` table.
pub(crate) fn mir_lint_root_bodies<'a, 'tcx>(
    body: &'a mir::Body<'tcx>,
    f: &mut impl FnMut(&'a mir::Body<'tcx>),
) {
    f(body);
    if let Some(info) = &body.coroutine {
        for inner in
            [&info.coroutine_drop, &info.coroutine_drop_async, &info.coroutine_drop_proxy_async]
                .into_iter()
                .flatten()
        {
            mir_lint_root_bodies(inner, f);
        }
    }
}

/// The same walk as [`mir_lint_root_bodies`], for a body to change.
fn mir_lint_root_bodies_mut<'tcx>(
    body: &mut mir::Body<'tcx>,
    f: &mut impl FnMut(&mut mir::Body<'tcx>),
) {
    f(body);
    if let Some(info) = &mut body.coroutine {
        for inner in [
            &mut info.coroutine_drop,
            &mut info.coroutine_drop_async,
            &mut info.coroutine_drop_proxy_async,
        ]
        .into_iter()
        .flatten()
        {
            mir_lint_root_bodies_mut(inner, f);
        }
    }
}

/// The id under which a local file appears in this crate's exported source map.
pub(crate) fn exported_stable_id(file: &SourceFile, crate_id: StableCrateId) -> StableSourceFileId {
    let mut name = file.name.clone();
    if let rustc_span::FileName::Real(ref mut real) = name {
        real.update_for_crate_metadata();
    }
    StableSourceFileId::from_filename_for_export(&name, crate_id)
}

enum RawSpan {
    Dummy,
    Local { file: u32, lo: u32, hi: u32 },
    Foreign(SpanData),
}

pub(crate) struct PrevDecoder<'a, 'tcx> {
    opaque: MemDecoder<'a>,
    lazy_state: LazyState,
    prev: &'a PrevSession<'tcx>,
    alloc_session: AllocDecodingSession<'a>,
}

impl<'a, 'tcx> PrevDecoder<'a, 'tcx> {
    /// Reads a span with the file numbering. Follows span shorthands.
    fn decode_raw_span(&mut self) -> RawSpan {
        self.decode_raw_span_with_ctxt().0
    }

    fn decode_raw_span_with_ctxt(&mut self) -> (RawSpan, SyntaxContext) {
        let start = self.position();
        let tag = SpanTag(self.peek_byte());
        if tag.kind() == SpanKind::Indirect {
            self.read_u8();
            let bytes_needed = tag.length().unwrap().0 as usize;
            let mut total = [0u8; usize::BITS as usize / 8];
            total[..bytes_needed].copy_from_slice(self.read_raw_bytes(bytes_needed));
            let offset_or_position = usize::from_le_bytes(total);
            let position = if tag.is_relative_offset() {
                start - offset_or_position
            } else {
                offset_or_position
            };
            self.with_position(position, |d| d.decode_direct_span())
        } else {
            self.decode_direct_span()
        }
    }

    fn decode_direct_span(&mut self) -> (RawSpan, SyntaxContext) {
        let tag = SpanTag::decode(self);
        let ctxt = tag.context().unwrap_or_else(|| SyntaxContext::decode(self));
        if tag.kind() == SpanKind::Partial {
            return (RawSpan::Dummy, ctxt);
        }
        let lo = BytePos::decode(self);
        let len = tag.length().unwrap_or_else(|| BytePos::decode(self));
        let metadata_index = u32::decode(self);
        if tag.kind() == SpanKind::Local {
            return (RawSpan::Local { file: metadata_index, lo: lo.0, hi: (lo + len).0 }, ctxt);
        }
        let cnum = CrateNum::decode(self);
        let Some(metadata_index) = self.prev.map_foreign_file(cnum, metadata_index) else {
            self.prev.rejected.set(true);
            return (RawSpan::Dummy, ctxt);
        };
        let tcx = self.prev.tcx;
        let file =
            CStore::from_tcx(tcx).get_crate_data(cnum).imported_source_file(tcx, metadata_index);
        let start = file.translated_source_file.start_pos;
        (
            RawSpan::Foreign(SpanData { lo: lo + start, hi: lo + len + start, ctxt, parent: None }),
            ctxt,
        )
    }
}

impl<'a, 'tcx> Decoder for PrevDecoder<'a, 'tcx> {
    forward_all_decoder_methods_to!(|self| self.opaque);
}

impl<'a, 'tcx> LazyDecoder for PrevDecoder<'a, 'tcx> {
    fn set_lazy_state(&mut self, state: LazyState) {
        self.lazy_state = state;
    }

    fn get_lazy_state(&self) -> LazyState {
        self.lazy_state
    }
}

impl<'a, 'tcx> BlobDecoder for PrevDecoder<'a, 'tcx> {
    fn decode_def_index(&mut self) -> DefIndex {
        self.prev.map_local_index(DefIndex::from_u32(self.read_u32()))
    }

    fn decode_symbol(&mut self) -> Symbol {
        match self.read_u8() {
            SYMBOL_STR => Symbol::intern(self.read_str()),
            SYMBOL_OFFSET => {
                let pos = self.read_usize();
                self.opaque.with_position(pos, |d| Symbol::intern(d.read_str()))
            }
            SYMBOL_PREDEFINED => Symbol::new(self.read_u32()),
            _ => unreachable!(),
        }
    }

    fn decode_byte_symbol(&mut self) -> ByteSymbol {
        match self.read_u8() {
            SYMBOL_STR => ByteSymbol::intern(self.read_byte_str()),
            SYMBOL_OFFSET => {
                let pos = self.read_usize();
                self.opaque.with_position(pos, |d| ByteSymbol::intern(d.read_byte_str()))
            }
            SYMBOL_PREDEFINED => ByteSymbol::new(self.read_u32()),
            _ => unreachable!(),
        }
    }

    fn decode_local_def_id(&mut self) -> LocalDefId {
        LocalDefId { local_def_index: self.decode_def_index() }
    }
}

impl<'a, 'tcx> SpanDecoder for PrevDecoder<'a, 'tcx> {
    fn decode_attr_id(&mut self) -> rustc_span::AttrId {
        self.prev.tcx.sess.psess.attr_id_generator.mk_attr_id()
    }

    // The crate graph matches the one (see `PrevSession::load`), so crate
    // numbers keep their meaning.
    fn decode_crate_num(&mut self) -> CrateNum {
        CrateNum::from_u32(self.read_u32())
    }

    fn decode_def_id(&mut self) -> DefId {
        let krate = CrateNum::decode(self);
        let raw = DefIndex::from_u32(self.read_u32());
        let index = if krate == LOCAL_CRATE {
            self.prev.map_local_index(raw)
        } else {
            self.prev.map_foreign_index(krate, raw)
        };
        DefId { krate, index }
    }

    fn decode_syntax_context(&mut self) -> SyntaxContext {
        let prev = self.prev;
        rustc_span::hygiene::decode_syntax_context(
            self,
            &prev.hygiene_context.borrow(),
            |_, ctxt| match prev.root.syntax_contexts.get(&prev.blob, ctxt) {
                Some(lazy) => prev.decode_at(lazy.position),
                None => bug!("missing syntax context {ctxt:?} in the previous metadata"),
            },
        )
    }

    fn decode_expn_id(&mut self) -> ExpnId {
        let cnum = CrateNum::decode(self);
        let index = u32::decode(self);
        let prev = self.prev;
        if cnum == LOCAL_CRATE {
            if index == 0 {
                return ExpnId::root();
            }
            let hash = prev
                .root
                .expn_hashes
                .get(&prev.blob, ExpnIndex::from_u32(index))
                .map(|lazy| prev.decode_at::<ExpnHash>(lazy.position));
            return match hash.and_then(ExpnId::from_hash) {
                Some(expn_id) => expn_id,
                // An expansion hash holds its call site, so an edit above a macro call gives
                // the expansion a new hash. Only this item is rejected.
                None => {
                    prev.expn_missing.set(true);
                    ExpnId::root()
                }
            };
        }
        let Some(index) = prev.map_foreign_expn(cnum, index) else {
            prev.expn_missing.set(true);
            return ExpnId::root();
        };
        let tcx = prev.tcx;
        rustc_span::hygiene::decode_expn_id(cnum, index, |expn_id| {
            let ExpnId { krate, local_id } = expn_id;
            let cstore = CStore::from_tcx(tcx);
            let cdata = cstore.get_crate_data(krate);
            let expn_data = cdata.root.expn_data.get(cdata, local_id).unwrap().decode((cdata, tcx));
            let expn_hash =
                cdata.root.expn_hashes.get(cdata, local_id).unwrap().decode((cdata, tcx));
            (expn_data, expn_hash)
        })
    }

    fn decode_span(&mut self) -> Span {
        let (raw, ctxt) = self.decode_raw_span_with_ctxt();
        match raw {
            RawSpan::Dummy => DUMMY_SP.with_ctxt(ctxt),
            // HIR lowering gives a span from a foreign file, such as a macro of a dependency, the same parent.
            RawSpan::Foreign(data) => {
                SpanData { parent: self.prev.span_parent.get(), ..data }.span()
            }
            RawSpan::Local { file, lo, hi } => {
                let (lo, hi) = self.prev.place_local_span(file, lo, hi);
                SpanData { lo, hi, ctxt, parent: self.prev.span_parent.get() }.span()
            }
        }
    }
}

impl<'a, 'tcx> TyDecoder<'tcx> for PrevDecoder<'a, 'tcx> {
    const CLEAR_CROSS_CRATE: bool = true;

    fn cached_ty_for_shorthand<F>(&mut self, shorthand: usize, or_insert_with: F) -> Ty<'tcx>
    where
        F: FnOnce(&mut Self) -> Ty<'tcx>,
    {
        if let Some(&ty) = self.prev.ty_cache.borrow().get(&shorthand) {
            return ty;
        }
        let ty = or_insert_with(self);
        self.prev.ty_cache.borrow_mut().insert(shorthand, ty);
        ty
    }

    fn with_position<F, R>(&mut self, pos: usize, f: F) -> R
    where
        F: FnOnce(&mut Self) -> R,
    {
        let new_opaque = self.opaque.split_at(pos);
        let old_opaque = mem::replace(&mut self.opaque, new_opaque);
        let old_state = mem::replace(&mut self.lazy_state, LazyState::NoNode);
        let r = f(self);
        self.opaque = old_opaque;
        self.lazy_state = old_state;
        r
    }

    fn decode_alloc_id(&mut self) -> rustc_middle::mir::interpret::AllocId {
        let ads = self.alloc_session;
        ads.decode_alloc_id(self)
    }
}

impl<'a, 'tcx> ty::InternerDecoder for PrevDecoder<'a, 'tcx> {
    type Interner = TyCtxt<'tcx>;

    #[inline]
    fn interner(&self) -> TyCtxt<'tcx> {
        self.prev.tcx
    }
}

impl<'a, 'tcx> Decodable<PrevDecoder<'a, 'tcx>> for ExpnIndex {
    #[inline]
    fn decode(d: &mut PrevDecoder<'a, 'tcx>) -> ExpnIndex {
        ExpnIndex::from_u32(d.read_u32())
    }
}
