// The fingerprint of an item of another crate covers whether that crate exports the item's symbol.

//@ aux-build:b.rs
//@ revisions: rpass1 rpass2 rpass3
//@ compile-flags: -Z incremental-extern-items -Z incremental-extern-sysroot
//@ ignore-backends: gcc

extern crate b;

fn main() {
    assert_eq!(b::value(), 5);
}
