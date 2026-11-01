//! The query cache writes a span that repeats an earlier span of the same value as a back reference.

//@ revisions: rpass1 rpass2
//@ compile-flags: -Ccodegen-units=1

use std::panic::Location;

#[track_caller]
fn here() -> u32 {
    Location::caller().line()
}

macro_rules! at {
    () => {
        (here(), line!())
    };
}

fn repeated() -> Vec<(u32, u32)> {
    let mut v = Vec::new();
    for _ in 0..2 {
        v.push((here(), line!()));
        v.push(at!());
        v.push(at!());
        v.push((here(), line!()));
        v.push(at!());
    }
    v
}

#[cfg(rpass1)]
fn changed() -> u32 {
    1
}

#[cfg(rpass2)]
fn changed() -> u32 {
    2
}

fn main() {
    for (got, want) in repeated() {
        assert_eq!(got, want);
    }
    assert!(changed() > 0);
}
