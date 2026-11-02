// With `-Z incremental-extern-items`, a lookup of an item of another crate reads one node per item.

//@ aux-build:a.rs
//@ revisions: rpass1 rpass2
//@ compile-flags: -Z query-dep-graph -Z incremental-extern-items
//@ ignore-backends: gcc

#![feature(rustc_attrs)]

extern crate a;

#[rustc_clean(cfg = "rpass2")]
pub fn use_stable() -> u16 {
    a::stable()
}

pub fn changed_size() -> usize {
    std::mem::size_of::<a::Changed>()
}

fn main() {
    assert_eq!(use_stable(), 7);
    #[cfg(rpass1)]
    assert_eq!(changed_size(), 1);
    #[cfg(rpass2)]
    assert_eq!(changed_size(), 4);
}
