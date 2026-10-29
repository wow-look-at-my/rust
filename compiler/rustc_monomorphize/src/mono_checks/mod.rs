//! Monomorphization-time checks for each monomorphization of a function.

use rustc_middle::mir::Body;
use rustc_middle::ty::{Instance, TyCtxt};

mod abi_check;
mod move_check;

pub(crate) fn check_mono_item<'tcx>(
    tcx: TyCtxt<'tcx>,
    instance: Instance<'tcx>,
    body: &Body<'tcx>,
) {
    abi_check::check_feature_dependent_abi(tcx, instance, body);
    move_check::check_moves(tcx, instance, body);
}
