#![crate_type = "rlib"]
// These lines move every item below them.
#![crate_name = "meta_reuse"]

macro_rules! double {
    ($e:expr) => {
        $e + $e
    };
}

pub fn stable<T: Copy + Into<u64>>(x: T) -> u64 {
    double!(x.into())
}

#[inline]
pub fn inlined(x: u32) -> u32 {
    x + 1
}

pub fn changed<T: Into<u64>>(x: T) -> u64 {
    x.into() + 1
}

// These lines move every item below them. The edit leaves their HIR
// unchanged. Their MIR spans must follow them.
pub fn moved<T: Copy + Into<u64>>(x: T) -> (u64, u32) {
    (x.into() * x.into(), std::panic::Location::caller().line())
}

pub fn closure_user<T: Copy + Into<i64>>(x: T) -> i64 {
    let add = |y: i64| y + 1;
    add(x.into())
}

pub fn promoted<T>() -> &'static u32 {
    &42
}

pub const fn const_fn(x: u32) -> u32 {
    x * 2
}
