//@ revisions: rpass1 bfail2 rpass3 rpass4
//@[bfail2] compile-flags: -Zassert-incr-state=not-loaded
//@[rpass4] compile-flags: -Zassert-incr-state=loaded
//@ ignore-backends: gcc

#![cfg_attr(any(rpass1, rpass3, rpass4), feature(trivial_bounds))]
#![allow(trivial_bounds)]

// A reuse of the `rpass1` graph in `bfail2` keeps this bound green and hides the error.
pub fn f() -> String
where
    String: Copy, //[bfail2]~ ERROR the trait bound
{
    String::new()
}

fn main() {}
