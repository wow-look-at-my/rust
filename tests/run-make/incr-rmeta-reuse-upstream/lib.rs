#![crate_type = "rlib"]
#![crate_name = "reuse_user"]

#[macro_use]
extern crate reuse_dep;

use reuse_dep::{Wrap, helper};

pub fn call<T: Into<u64>>(x: T) -> u64 {
    helper(x.into())
}

pub fn wrap<T: Into<u64>>(x: T) -> Wrap {
    Wrap(twice!(x.into()))
}
