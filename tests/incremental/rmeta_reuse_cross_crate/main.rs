// Edits to an upstream crate whose metadata decodes unchanged MIR from its previous session.

//@ revisions: rpass1 rpass2 rpass3
//@ aux-build: a.rs
//@ ignore-backends: gcc

extern crate a;

const C: u32 = a::const_fn(3);

fn main() {
    assert_eq!(a::stable(4u8), 8);
    assert_eq!(a::inlined(4), 5);
    assert_eq!(*a::promoted::<u8>(), 42);
    assert_eq!(a::closure_user(2i32), 3);
    assert_eq!(C, 6);
    assert_eq!(a::CONST_ITEM, 14);

    #[cfg(rpass1)]
    assert_eq!(a::generic_body(1u8), 1);
    #[cfg(not(rpass1))]
    assert_eq!(a::generic_body(1u8), 101);

    #[cfg(not(rpass3))]
    {
        assert_eq!(a::plain_body(1), 10);
        assert_eq!(a::signature(2u8), 2);
    }
    #[cfg(rpass3)]
    {
        assert_eq!(a::plain_body(1), 20);
        assert_eq!(a::signature(2u8, 3u8), 5);
    }
}
