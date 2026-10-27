// With more modules than codegen units, `-Z incremental-stable-cgu-merge`
// keeps the name of every merged codegen unit when one module grows.

//@ ignore-cross-compile

use std::collections::BTreeSet;
use std::path::PathBuf;

use run_make_support::{rfs, rustc};

const MODULES: &[&str] = &["a", "b", "c", "d", "e", "f", "g", "h"];

fn write_lib(extra_in_a: usize) {
    let mut src = String::from("#![crate_type = \"rlib\"]\n");
    for module in MODULES {
        src.push_str(&format!("pub mod {module} {{\n"));
        let count = if *module == "a" { 2 + extra_in_a } else { 2 };
        for i in 0..count {
            src.push_str(&format!(
                "    pub fn f{i}(x: u32) -> u32 {{ x.wrapping_mul({i}) + 1 }}\n"
            ));
        }
        src.push_str("}\n");
    }
    rfs::write("lib.rs", src);
}

/// The codegen units of the newest session, by the names of their object files.
fn cgu_names() -> BTreeSet<String> {
    let crate_dir = rfs::read_dir("incr").next().unwrap().unwrap().path();
    let sessions: Vec<PathBuf> = rfs::read_dir(&crate_dir)
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect();
    assert_eq!(sessions.len(), 1, "expected one session in {crate_dir:?}");
    rfs::read_dir(&sessions[0])
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".o"))
        .collect()
}

fn build() -> BTreeSet<String> {
    rustc()
        .input("lib.rs")
        .incremental("incr")
        .arg("-Ccodegen-units=3")
        .arg("-Zhuman-readable-cgu-names")
        .arg("-Zincremental-stable-cgu-merge")
        .run();
    cgu_names()
}

fn main() {
    write_lib(0);
    let before = build();
    assert!(!before.is_empty() && before.len() <= 3, "unexpected codegen units: {before:?}");

    write_lib(40);
    let after = build();
    assert_eq!(before, after, "a larger module renamed a merged codegen unit");
}
