//! The symbol check compares the hash of a green item's name with the name of a new item.

//@ revisions: bpass1 bfail2

#[unsafe(export_name = "clash")]
pub fn first() {}

#[cfg(bfail2)]
#[unsafe(export_name = "clash")]
pub fn second() {}
//[bfail2]~^ ERROR symbol `clash` is already defined

fn main() {
    first();
    #[cfg(bfail2)]
    second();
}
