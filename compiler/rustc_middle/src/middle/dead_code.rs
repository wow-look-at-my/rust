use rustc_data_structures::fx::FxIndexSet;
use rustc_hir::def_id::{DefId, LocalDefId, LocalDefIdMap, LocalDefIdSet};
use rustc_macros::{StableHash, TyDecodable, TyEncodable};

/// One effect that a body has on the dead-code worklist.
#[derive(Copy, Clone, Debug, StableHash, TyEncodable, TyDecodable)]
pub enum DeadCodeBodyUse {
    /// The def is live, and the pass explores it if it can.
    Check(DefId),
    /// The def is live, and the pass does not explore it.
    Insert(DefId),
    /// The def is live in every allow or expect context.
    Live(LocalDefId),
    /// The pass explores the def with no allow or expect context.
    Seed(LocalDefId),
}

/// A single snapshot of dead-code liveness analysis state.
#[derive(Clone, Debug, StableHash)]
pub struct DeadCodeLivenessSnapshot {
    pub live_symbols: LocalDefIdSet,
    /// Maps each ADT to derived traits (for example `Debug` and `Clone`) that should be ignored
    /// when checking for dead code diagnostics.
    pub ignored_derived_traits: LocalDefIdMap<FxIndexSet<DefId>>,
}

/// Dead-code liveness data for both analysis phases.
///
/// `pre_deferred_seeding` is computed before reachable-public and `#[allow(dead_code)]` seeding,
/// and is used for lint `dead_code_pub_in_binary`.
/// `final_result` is the final liveness snapshot used for lint `dead_code`.
#[derive(Clone, Debug, StableHash)]
pub struct DeadCodeLivenessSummary {
    pub pre_deferred_seeding: DeadCodeLivenessSnapshot,
    pub final_result: DeadCodeLivenessSnapshot,
}
