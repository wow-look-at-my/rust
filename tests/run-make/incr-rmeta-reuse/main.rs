// `Location::caller` reads the span of the reused MIR, which moved down a few lines.

extern crate meta_reuse;

const C: u32 = meta_reuse::const_fn(3);

fn main() {
    assert_eq!(meta_reuse::stable(4u8), 8);
    assert_eq!(meta_reuse::inlined(4), 5);
    assert_eq!(meta_reuse::changed(1u8), 2);
    // `Location::caller` reads the span of the reused MIR, which moved down a few lines.
    assert_eq!(meta_reuse::moved(3u16), (9, 27));
    assert_eq!(meta_reuse::closure_user(2i32), 3);
    assert_eq!(*meta_reuse::promoted::<u8>(), 42);
    assert_eq!(C, 6);
}
