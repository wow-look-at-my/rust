// A kind that `-Zincremental-transparent` names gets no node, so its reader
// depends on its reads.

// ignore-tidy-linelength
//@ revisions: rpass1 rpass2 rpass3
//@ compile-flags: -Zincremental-verify-ich
//@ compile-flags: -Zincremental-transparent=symbol_name,resolve_instance_raw,TraitSelect,fn_abi_of_instance_no_deduced_attrs,size_estimate
//@ ignore-backends: gcc

trait Answer {
    fn get(&self) -> u32;
}

struct S;

impl Answer for S {
    #[cfg(any(rpass1, rpass3))]
    fn get(&self) -> u32 {
        1
    }

    #[cfg(rpass2)]
    fn get(&self) -> u32 {
        2
    }
}

fn call<T: Answer>(t: &T) -> u32 {
    t.get()
}

#[cfg(any(rpass1, rpass3))]
mod place {
    pub fn tag<T>(x: T) -> T {
        x
    }
}

#[cfg(rpass2)]
mod place {
    pub fn tag<T: Copy>(x: T) -> T {
        let y = x;
        y
    }
}

fn main() {
    let dynamic: &dyn Answer = &S;
    let v = place::tag(call(&S)) + dynamic.get();
    #[cfg(any(rpass1, rpass3))]
    assert_eq!(v, 2);
    #[cfg(rpass2)]
    assert_eq!(v, 4);
}
