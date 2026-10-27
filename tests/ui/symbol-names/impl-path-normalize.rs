//@ build-fail

#![feature(rustc_attrs)]
#![allow(dead_code)]

pub trait Id {
    type Out;
}
impl Id for u8 {
    type Out = u8;
}
impl Id for u32 {
    type Out = u16;
}

pub trait Tr<A> {
    fn m();
}

pub struct Foo;
pub struct Wrap<T>(T);
pub struct Wrap2<T>(T);

type Alias = <Foo as Id>::Out;
impl Id for Foo {
    type Out = Wrap<u8>;
}

impl Tr<<u8 as Id>::Out> for Foo {
    #[rustc_dump_symbol_name] //~ ERROR symbol-name(
    fn m() {}
}

impl Tr<<u32 as Id>::Out> for Alias {
    #[rustc_dump_symbol_name] //~ ERROR symbol-name(
    fn m() {}
}

impl<T: Id> Tr<T::Out> for Wrap2<T> {
    fn m() {
        #[rustc_dump_symbol_name] //~ ERROR symbol-name(
        fn inner() {}
    }
}

impl<T> Tr<<T as Id>::Out> for (T,)
where
    T: Id<Out = u16>,
{
    fn m() {
        #[rustc_dump_symbol_name] //~ ERROR symbol-name(
        fn inner() {}
    }
}

impl Wrap<<u32 as Id>::Out> {
    #[rustc_dump_symbol_name] //~ ERROR symbol-name(
    fn inherent() {}
}

fn main() {}
