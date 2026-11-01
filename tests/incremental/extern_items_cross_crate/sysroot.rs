// With `-Z incremental-extern-sysroot`, a lookup of an item of a sysroot crate reads the hash of that crate.

//@ aux-build:a.rs
//@ revisions: rpass1 rpass2
//@ compile-flags: -Z query-dep-graph -Z incremental-extern-items -Z incremental-extern-sysroot
//@ ignore-backends: gcc

#![feature(rustc_attrs)]

extern crate a;

#[rustc_clean(cfg = "rpass2")]
pub fn use_stable() -> u16 {
    a::stable()
}

#[rustc_clean(cfg = "rpass2")]
pub fn use_std() -> usize {
    let v: Vec<u8> = Vec::with_capacity(3);
    v.capacity().min(3) + std::mem::size_of::<Option<u8>>()
}

pub fn changed_size() -> usize {
    std::mem::size_of::<a::Changed>()
}

fn main() {
    assert_eq!(use_stable(), 7);
    assert_eq!(use_std(), 5);
    #[cfg(rpass1)]
    assert_eq!(changed_size(), 1);
    #[cfg(rpass2)]
    assert_eq!(changed_size(), 4);
}
