//@ revisions: rpass1 bfail2 rpass3 rpass4
//@[bfail2] compile-flags: -Zassert-incr-state=not-loaded
//@[rpass4] compile-flags: -Zassert-incr-state=loaded
//@ ignore-backends: gcc

#![cfg_attr(bfail2, recursion_limit = "4")]

use std::ops::Deref;

macro_rules! chain {
    ($($a:ident => $b:ident),*) => {
        $(
            struct $a(Box<$b>);
            impl Deref for $a {
                type Target = $b;
                fn deref(&self) -> &$b {
                    &self.0
                }
            }
        )*
    };
}

struct J;

impl J {
    fn hello(&self) -> u32 {
        1
    }
}

chain!(A => B, B => C, C => D, D => E, E => F, F => G, G => H, H => I, I => J);

// A reuse of the `rpass1` graph in `bfail2` keeps the type check green and hides the error.
fn main() {
    let a = A(Box::new(B(Box::new(C(Box::new(D(Box::new(E(Box::new(F(Box::new(G(
        Box::new(H(Box::new(I(Box::new(J))))),
    )))))))))))));
    //[bfail2]~vv ERROR reached the recursion limit
    //[bfail2]~v ERROR no method named `hello`
    assert_eq!(a.hello(), 1);
}
