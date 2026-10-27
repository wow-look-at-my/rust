#![crate_type = "rlib"]
// These items come first, so every definition below them gets a new index.
#![crate_name = "reuse_dep"]

// These items come first, so every definition below them gets a new index.
pub struct Added;

pub fn added() -> u32 {
    3
}

pub struct Wrap(pub u64);

pub fn helper(x: u64) -> u64 {
    x + 1
}

#[macro_export]
macro_rules! twice {
    ($e:expr) => {
        $e * 2
    };
}
