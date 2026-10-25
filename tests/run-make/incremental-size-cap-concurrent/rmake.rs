//! `-Zincremental-size-cap` evicts idle crate dirs while other compiles use
//! the same incremental dir.

//@ ignore-cross-compile

use std::path::Path;
use std::thread;
use std::time::Duration;

use run_make_support::{rfs, run, rustc, shallow_find_directories};

const OLD: [&str; 4] = ["a", "b", "c", "d"];

fn lib_source(name: &str) -> String {
    // Enough items that compiles overlap in time.
    let mut src = String::new();
    for i in 0..3000 {
        src.push_str(&format!("pub fn f{i}(x: u64) -> u64 {{ x.wrapping_mul({i}) ^ {i} }}\n"));
    }
    src.push_str(&format!("pub fn id() -> &'static str {{ \"{name}\" }}\n"));
    src
}

fn build(name: &str, cap: bool) {
    let mut cmd = rustc();
    cmd.input(format!("{name}.rs")).crate_name(name).crate_type("rlib").incremental("incr");
    if cap {
        cmd.arg("-Zincremental-size-cap=1");
    }
    cmd.run();
}

fn crate_dir(name: &str) -> Option<std::path::PathBuf> {
    let prefix = format!("{name}-");
    let mut dirs = shallow_find_directories("incr", |p| {
        p.file_name().unwrap().to_str().unwrap().starts_with(&prefix)
    });
    assert!(dirs.len() <= 1, "more than one crate dir for `{name}`: {dirs:?}");
    dirs.pop()
}

fn assert_one_finalized_session(name: &str) {
    let dir = crate_dir(name).unwrap_or_else(|| panic!("crate dir of `{name}` was evicted"));
    let sessions = shallow_find_directories(&dir, |_| true);
    assert_eq!(sessions.len(), 1, "`{name}` should hold one session: {sessions:?}");
    let session = sessions[0].file_name().unwrap().to_str().unwrap().to_owned();
    assert!(!session.ends_with("-working"), "`{name}` kept an unfinished session: {session}");
}

fn main() {
    for name in OLD.iter().chain(&["g", "h"]) {
        rfs::write(format!("{name}.rs"), lib_source(name));
    }
    for name in OLD {
        build(name, false);
    }
    // Eviction ignores a crate dir used in the last several seconds.
    thread::sleep(Duration::from_secs(11));

    // The cap must not be able to delete anything outside a crate dir.
    rfs::create_dir_all("incr/keepme");
    rfs::write("incr/keepme/file", "x");
    rfs::write("incr/not-a-crate-dir", "x");

    // `a` and `b` rebuild while `g` and `h` evict every idle crate dir.
    thread::scope(|s| {
        s.spawn(|| build("a", false));
        s.spawn(|| build("b", false));
        s.spawn(|| build("g", true));
        s.spawn(|| build("h", true));
    });

    for name in ["a", "b", "g", "h"] {
        assert_one_finalized_session(name);
    }
    for name in ["c", "d"] {
        assert!(crate_dir(name).is_none(), "idle crate dir of `{name}` was not evicted");
    }
    assert!(Path::new("incr/keepme/file").exists(), "the cap deleted a non-crate dir");
    assert!(Path::new("incr/not-a-crate-dir").exists(), "the cap deleted a non-crate file");

    // Rebuild an evicted crate and the rebuilt ones, then link and run all of them.
    for name in ["a", "b", "c", "g"] {
        build(name, false);
        assert_one_finalized_session(name);
    }
    rfs::write(
        "main.rs",
        "fn main() {\n\
         assert_eq!(a::id(), \"a\");\n\
         assert_eq!(b::id(), \"b\");\n\
         assert_eq!(c::id(), \"c\");\n\
         assert_eq!(g::id(), \"g\");\n\
         assert_eq!(a::f7(3), 3u64.wrapping_mul(7) ^ 7);\n\
         }\n",
    );
    rustc()
        .input("main.rs")
        .extern_("a", "liba.rlib")
        .extern_("b", "libb.rlib")
        .extern_("c", "libc.rlib")
        .extern_("g", "libg.rlib")
        .run();
    run("main");
}
