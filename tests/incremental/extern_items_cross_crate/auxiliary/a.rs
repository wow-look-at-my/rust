//@ compile-flags: -Z query-dep-graph -Z incremental-extern-items

// Last in the file, so that its change moves the span of no other item.

#![crate_type = "rlib"]

pub fn stable() -> u16 {
    7
}

// Last in the file, so that its change moves the span of no other item.
#[cfg(rpass1)]
pub struct Changed {
    pub x: u8,
}

#[cfg(rpass2)]
pub struct Changed {
    pub x: u32,
}
