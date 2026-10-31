use rustc_data_structures::fingerprint::{Fingerprint, PackedFingerprint};
use rustc_data_structures::unord::UnordMap;
#[expect(unused_imports, reason = "used by doc comments")]
use rustc_middle::dep_graph::DepKindVTable;
use rustc_middle::dep_graph::{
    DepGraphData, DepNode, DepNodeIndex, DepNodeKey, SerializedDepNodeIndex,
};
use rustc_middle::query::erase::{Erasable, Erased};
use rustc_middle::query::on_disk_cache::{CacheDecoder, CacheEncoder};
use rustc_middle::query::{QueryCache, QueryState, QueryVTable, erase};
use rustc_middle::ty::codec::TyEncoder;
use rustc_middle::ty::{InMetadata, TyCtxt};
use rustc_middle::verify_ich::incremental_verify_ich;
use rustc_serialize::{Decodable, Encodable};
use rustc_span::bug;

use crate::query_vtables::for_each_query_vtable;

fn all_inactive<'tcx, K>(state: &QueryState<'tcx, K>) -> bool {
    state.active.lock_shards().all(|shard| shard.is_empty())
}

pub(crate) fn encode_query_values<'tcx>(tcx: TyCtxt<'tcx>, encoder: &mut CacheEncoder<'tcx>) {
    if tcx.sess.opts.unstable_opts.incremental_info
        && let Some(Some(prev)) = tcx.prev_metadata.get()
    {
        prev.report();
    }
    let info = tcx.sess.opts.unstable_opts.incremental_info;
    let mut sizes = Vec::new();
    for_each_query_vtable!(CACHE_ON_DISK, tcx, |query| {
        let start = encoder.position();
        let span_start = encoder.span_bytes();
        let count = encode_query_values_inner(tcx, query, encoder);
        if info {
            let spans = encoder.span_bytes() - span_start;
            sizes.push((encoder.position() - start, count, query.name, spans));
        }
    });
    if info {
        // A type shorthand counts against the query that encodes the type first.
        sizes.sort_unstable_by(|a, b| b.cmp(a));
        for (bytes, count, name, spans) in sizes.into_iter().take(20).filter(|s| s.0 > 0) {
            eprintln!("[incremental] query cache bytes: {name} {count} {bytes} {spans}");
        }
        let (refs, ref_bytes) = encoder.shorthand_refs();
        eprintln!("[incremental] query cache shorthand refs: {refs} {ref_bytes}");
    }
}

fn encode_query_values_inner<'tcx, C, V>(
    tcx: TyCtxt<'tcx>,
    query: &'tcx QueryVTable<'tcx, C>,
    encoder: &mut CacheEncoder<'tcx>,
) -> usize
where
    C: QueryCache<Value = Erased<V>>,
    V: Erasable + Encodable<CacheEncoder<'tcx>>,
{
    let _timer = tcx.prof.generic_activity_with_arg("encode_query_results_for", query.name);

    assert!(all_inactive(&query.state));
    // The filter below hashes a dep node for each entry. A kind that writes nothing needs none.
    if !query.cache_on_disk_local {
        return 0;
    }
    let in_metadata = tcx.in_metadata.get().and_then(|sets| sets.for_kind(query.dep_kind));
    // A session that copies the metadata holds every value that it decoded
    // from it.
    let prev = match tcx.prev_metadata.get() {
        Some(&Some(prev)) if InMetadata::holds_kind(query.dep_kind) => Some(prev),
        _ => None,
    };
    let mut values = Vec::new();
    query.cache.for_each(&mut |key, value, dep_node| {
        if !query.will_cache_on_disk_for_key(*key) {
            return;
        }
        if (in_metadata.is_some() || prev.is_some())
            && let Some(def_id) = DepNode::construct(tcx, query.dep_kind, key).extract_def_id(tcx)
            && let Some(def_id) = def_id.as_local()
            && (in_metadata.is_some_and(|set| set.contains(&def_id))
                || prev.is_some_and(|prev| prev.decoded(query.dep_kind, def_id)))
        {
            return;
        }
        values.push((dep_node, *value));
    });
    // In node order, the index stores each node as a small step from the one before.
    values.sort_unstable_by_key(|&(dep_node, _)| dep_node.as_u32());
    for &(dep_node, value) in &values {
        encoder.encode_query_value::<V>(dep_node, &erase::restore_val::<V>(value));
    }
    values.len()
}

pub(crate) fn verify_query_key_hashes<'tcx>(tcx: TyCtxt<'tcx>) {
    if tcx.sess.opts.unstable_opts.incremental_verify_ich || cfg!(debug_assertions) {
        tcx.sess.time("verify_query_key_hashes", || {
            for_each_query_vtable!(ALL, tcx, |query| {
                verify_query_key_hashes_inner(query, tcx);
            });
        });
    }
}

fn verify_query_key_hashes_inner<'tcx, C: QueryCache>(
    query: &'tcx QueryVTable<'tcx, C>,
    tcx: TyCtxt<'tcx>,
) {
    let _timer = tcx.prof.generic_activity_with_arg("query_key_hash_verify_for", query.name);

    let cache = &query.cache;
    let mut map = UnordMap::with_capacity(cache.len());
    cache.for_each(&mut |key, _, _| {
        let node = DepNode::construct(tcx, query.dep_kind, key);
        if let Some(other_key) = map.insert(node, *key) {
            bug!(
                "query key:\n\
                `{:?}`\n\
                and key:\n\
                `{:?}`\n\
                mapped to the same dep node:\n\
                {:?}",
                key,
                other_key,
                node
            );
        }
    });
}

/// Whether a value loaded from the on-disk cache should have its fingerprint
/// verified with `incremental_verify_ich`. If `-Zincremental-verify-ich` is
/// specified, re-hash results from the cache and make sure that they have the
/// expected fingerprint.
///
/// If not, we still verify a subset: re-hashing is too expensive to do for
/// every value. The subset rotates with the session count, covering the whole
/// cache every 32 sessions, and is deterministic so that a verification
/// failure reproduces on retry.
///
/// `to_smaller_hash` mixes both fingerprint halves because neither half is
/// evenly distributed on its own (`DefPathHash` keys share the
/// `StableCrateId`, `HirId` keys contain a sequential id).
pub(crate) fn should_verify_loaded_value(
    tcx: TyCtxt<'_>,
    dep_graph_data: &DepGraphData,
    key_fingerprint: PackedFingerprint,
) -> bool {
    let hash = Fingerprint::from(key_fingerprint).to_smaller_hash().as_u64();
    hash % 32 == dep_graph_data.session_count() % 32
        || tcx.sess.opts.unstable_opts.incremental_verify_ich
}

/// Inner implementation of [`DepKindVTable::promote_from_disk_fn`] for queries.
pub(crate) fn promote_from_disk_inner<'tcx, C: QueryCache>(
    tcx: TyCtxt<'tcx>,
    query: &'tcx QueryVTable<'tcx, C>,
    dep_node: DepNode,
    prev_index: SerializedDepNodeIndex,
    dep_node_index: DepNodeIndex,
) {
    debug_assert!(tcx.dep_graph.is_green(&dep_node));

    let key = C::Key::try_recover_key(tcx, &dep_node).unwrap_or_else(|| {
        panic!(
            "Failed to recover key for {dep_node:?} with key fingerprint {}",
            dep_node.key_fingerprint
        )
    });

    // If the recovered key isn't eligible for cache-on-disk, then there's no
    // value on disk to promote.
    if !query.will_cache_on_disk_for_key(key) {
        return;
    }

    // If the value is already in memory, then promotion isn't needed.
    if query.cache.lookup(&key).is_some() {
        return;
    }

    // Load the disk-cached value into memory.
    let dep_graph_data =
        tcx.dep_graph.data().expect("should always be present in incremental mode");

    let prof_timer = tcx.prof.incr_cache_loading();
    let value = (query.try_load_from_disk_fn)(tcx, prev_index);
    prof_timer.finish_with_query_invocation_id(dep_node_index.into());

    let Some(value) = value else {
        // A previous session under `-Z incremental-uncache` did not write
        // this kind.
        if tcx.sess.opts.unstable_opts.incremental_uncache.is_some() {
            return;
        }
        // Under `-Z incremental-rmeta-reuse` the metadata holds some MIR, types and signatures
        // in place of the query cache.
        if tcx.sess.opts.unstable_opts.incremental_rmeta_reuse
            && InMetadata::holds_kind(query.dep_kind)
        {
            return;
        }
        // The key is cache-on-disk and its node is green, so a value must be on disk.
        bug!("failed to load disk-cached value for green node {dep_node:?}");
    };

    // Verify the fingerprints of the same subset of loaded values as
    // `load_from_disk_or_invoke_provider_green` does.
    if should_verify_loaded_value(tcx, dep_graph_data, dep_node.key_fingerprint) {
        incremental_verify_ich(
            tcx,
            dep_graph_data,
            &value,
            prev_index,
            query.hash_value_fn,
            query.format_value,
        );
    }

    query.cache.complete(key, value, dep_node_index);
}

pub(crate) fn try_load_from_disk<'tcx, V>(
    tcx: TyCtxt<'tcx>,
    prev_index: SerializedDepNodeIndex,
) -> Option<V>
where
    V: for<'a> Decodable<CacheDecoder<'a, 'tcx>>,
{
    let on_disk_cache = tcx.query_system.on_disk_cache.as_ref()?;

    // The call to `with_query_deserialization` enforces that no new `DepNodes`
    // are created during deserialization. See the docs of that method for more
    // details.
    tcx.dep_graph.with_query_deserialization(|| on_disk_cache.try_load_query_value(tcx, prev_index))
}
