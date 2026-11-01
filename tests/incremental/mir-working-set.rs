//! A body in a red codegen unit finds no cached MIR when the last session did not use it.

//@ revisions: rpass1 rpass2 rpass3
//@ compile-flags: -Zincremental-mir-working-set -Zincremental-verify-ich -Copt-level=0

mod a {
    #[cfg(rpass1)]
    pub fn value() -> u32 {
        1
    }

    #[cfg(not(rpass1))]
    pub fn value() -> u32 {
        2
    }
}

mod b {
    pub fn stable() -> u32 {
        40
    }

    #[cfg(not(rpass3))]
    pub fn changed() -> u32 {
        1
    }

    #[cfg(rpass3)]
    pub fn changed() -> u32 {
        2
    }
}

fn main() {
    assert_eq!(a::value(), if cfg!(rpass1) { 1 } else { 2 });
    assert_eq!(b::stable(), 40);
    assert_eq!(b::changed(), if cfg!(rpass3) { 2 } else { 1 });
}
