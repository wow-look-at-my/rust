// A fed query cannot leave the on-disk cache: its provider does not reproduce the fed value.
//@ incremental
//@ compile-flags: -Z incremental-uncache=type_of
//@ dont-check-compiler-stderr

fn main() {}

//~? ERROR not a cache_on_disk query, or a fed query: type_of
