// One local item that a body refers to, for `reachable_set`.

use rustc_hir::def::DefKind;
use rustc_hir::def_id::LocalDefId;
use rustc_macros::{StableHash, TyDecodable, TyEncodable};

/// One local item that a body refers to, for `reachable_set`.
#[derive(Copy, Clone, Debug, StableHash, TyEncodable, TyDecodable)]
pub enum ReachableBodyRef {
    /// The body names this item. `reachable_set` decides from its kind what to do.
    Item(DefKind, LocalDefId),
    /// The item is reachable, and `reachable_set` does not scan it.
    Symbol(LocalDefId),
}
