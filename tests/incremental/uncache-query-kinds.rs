// `-Z incremental-uncache` drops query kinds from the on-disk cache. A green
// node of a dropped kind has no value on disk, so it must recompute.

//@ revisions: rpass1 rpass2 rpass3 rpass4
//@ compile-flags: -Z query-dep-graph -Z incremental-verify-ich
//@ [rpass1] compile-flags: -Z incremental-uncache=optimized_mir,typeck_root,items_of_instance
//@ [rpass2] compile-flags: -Z incremental-uncache=optimized_mir,typeck_root,items_of_instance
//@ [rpass3] compile-flags: -Z incremental-uncache=
//@ [rpass4] compile-flags: -Z incremental-uncache=optimized_mir
//@ ignore-backends: gcc

#![feature(rustc_attrs)]
#![rustc_partition_reused(module = "uncache_query_kinds-x", cfg = "rpass2")]
#![rustc_partition_reused(module = "uncache_query_kinds-x", cfg = "rpass3")]
#![rustc_partition_reused(module = "uncache_query_kinds-x", cfg = "rpass4")]

fn main() {
    assert_eq!(x::sum(&[1, 2, 3]), 6);
    assert_eq!(x::generic(7u8), 7);

    #[cfg(rpass2)]
    assert_eq!(y::f(), 2);
    #[cfg(any(rpass1, rpass3, rpass4))]
    assert_eq!(y::f(), 1);
}

mod x {
    pub fn sum(v: &[i32]) -> i32 {
        v.iter().sum()
    }

    #[inline]
    pub fn generic<T: Into<u32>>(t: T) -> u32 {
        t.into()
    }
}

mod y {
    #[cfg(rpass2)]
    pub fn f() -> i32 {
        2
    }
    #[cfg(not(rpass2))]
    pub fn f() -> i32 {
        1
    }
}
