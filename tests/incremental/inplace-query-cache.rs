// `-Z incremental-inplace-cache` keeps query values in a values file that the
// sessions of a crate share. Green nodes keep their entries from an earlier
// session, so those entries must decode in later sessions. `-Z
// incremental-verify-ich` checks each value that a session loads against its
// fingerprint.

//@ revisions: rpass1 rpass2 rpass3 rpass4 rpass5
//@ compile-flags: -Z query-dep-graph -Z incremental-verify-ich
//@ [rpass1] compile-flags: -Z incremental-inplace-cache
//@ [rpass2] compile-flags: -Z incremental-inplace-cache
//@ [rpass3] compile-flags: -Z incremental-inplace-cache
//@ [rpass5] compile-flags: -Z incremental-inplace-cache
//@ ignore-backends: gcc

#![feature(rustc_attrs)]
#![rustc_partition_reused(module = "inplace_query_cache-x", cfg = "rpass2")]
#![rustc_partition_reused(module = "inplace_query_cache-x", cfg = "rpass3")]
#![rustc_partition_reused(module = "inplace_query_cache-x", cfg = "rpass4")]
#![rustc_partition_reused(module = "inplace_query_cache-x", cfg = "rpass5")]

fn main() {
    assert_eq!(x::sum(&[1, 2, 3]), 6);
    assert_eq!(x::generic(7u8), 7);
    assert_eq!(x::TABLE[2], "two");
    assert_eq!(x::doubled(), 42);
    assert!(x::here().ends_with("inplace-query-cache.rs"));

    #[cfg(any(rpass2, rpass4))]
    assert_eq!(y::f(), 2);
    #[cfg(any(rpass1, rpass3, rpass5))]
    assert_eq!(y::f(), 1);
}

mod x {
    macro_rules! twice {
        ($e:expr) => {{
            let v = $e;
            v + v
        }};
    }

    pub static TABLE: [&str; 3] = ["zero", "one", "two"];

    pub fn sum(v: &[i32]) -> i32 {
        v.iter().sum()
    }

    #[inline]
    pub fn generic<T: Into<u32>>(t: T) -> u32 {
        t.into()
    }

    pub fn doubled() -> i32 {
        twice!(21)
    }

    pub fn here() -> &'static str {
        file!()
    }
}

mod y {
    #[cfg(any(rpass2, rpass4))]
    pub fn f() -> i32 {
        2
    }

    #[cfg(any(rpass1, rpass3, rpass5))]
    pub fn f() -> i32 {
        1
    }
}
