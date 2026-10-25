
rustc_queries! {
    query a(_: ()) -> u32 {
        desc { "a" }
        // eval_always in a comment does not count
    }
    query b(key: DefId) -> u32 {
        cache_on_disk
    }
    query c(key: DefId) {
        eval_always
        desc { "c {}", x }
    }
    non_query Null
    non_query Red
    non_query TraitSelect
}
