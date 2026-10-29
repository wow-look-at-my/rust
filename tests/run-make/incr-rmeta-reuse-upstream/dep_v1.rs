#![crate_type = "rlib"]
#![crate_name = "reuse_dep"]

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
