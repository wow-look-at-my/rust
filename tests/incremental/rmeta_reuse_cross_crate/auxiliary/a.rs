//@ compile-flags: -Zincremental-rmeta-reuse

#![crate_name = "a"]
#![crate_type = "rlib"]

pub fn stable<T: Copy + Into<u64>>(x: T) -> u64 {
    let v = [x.into(); 2];
    v[0] + v[1]
}

#[inline]
pub fn inlined(x: u32) -> u32 {
    x + 1
}

#[cfg(rpass1)]
pub fn generic_body<T: Into<u64>>(x: T) -> u64 {
    x.into()
}

#[cfg(not(rpass1))]
pub fn generic_body<T: Into<u64>>(x: T) -> u64 {
    x.into() + 100
}

#[cfg(not(rpass3))]
pub fn plain_body(x: u32) -> u32 {
    x * 10
}

#[cfg(rpass3)]
pub fn plain_body(x: u32) -> u32 {
    x * 20
}

#[cfg(not(rpass3))]
pub fn signature<T: Into<u64>>(x: T) -> u64 {
    x.into()
}

#[cfg(rpass3)]
pub fn signature<T: Into<u64>>(x: T, y: T) -> u64 {
    x.into() + y.into()
}

pub fn promoted<T>() -> &'static u32 {
    &42
}

pub fn closure_user<T: Copy + Into<i64>>(x: T) -> i64 {
    let add = |y: i64| y + 1;
    add(x.into())
}

pub const fn const_fn(x: u32) -> u32 {
    x * 2
}

pub const CONST_ITEM: u32 = const_fn(7);
