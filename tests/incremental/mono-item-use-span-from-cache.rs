//! The mono items of a caller that did not change come from the query cache,
//! which keeps no span for each use.

//@ revisions: bpass1 bfail2

trait Checked {
    const OK: ();
}

impl<T> Checked for T {
    #[cfg(bpass1)]
    const OK: () = ();
    #[cfg(bfail2)]
    const OK: () = [()][std::mem::size_of::<T>()];
    //[bfail2]~^ ERROR index out of bounds
    //[bfail2]~| NOTE evaluation of `<u32 as Checked>::OK` failed here
}

fn check<T>() {
    <T as Checked>::OK;
    //[bfail2]~^ NOTE erroneous constant encountered
}

fn caller() {
    check::<u32>();
    //[bfail2]~^ NOTE the above error was encountered while instantiating `fn check::<u32>`
}

fn main() {
    caller();
}
