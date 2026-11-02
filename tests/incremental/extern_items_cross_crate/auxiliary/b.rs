//@ compile-flags: -Z incremental-extern-items -Z incremental-extern-sysroot

// b.rs holds the b code.

#![crate_type = "rlib"]

// `#[inline]` makes the function a local copy in each caller, so only the first and the
// last revision export its symbol.
#[cfg_attr(rpass2, inline)]
pub fn value() -> u32 {
    5
}
