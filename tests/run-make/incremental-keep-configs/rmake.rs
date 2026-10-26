//! `-Zincremental-keep-configs=1` replaces an old configuration of a crate, but never the `--test` build of the same crate.

//@ ignore-cross-compile

use run_make_support::{rfs, rustc, shallow_find_directories};

fn build(metadata: &str, test: bool) {
    let mut cmd = rustc();
    cmd.input("k.rs").crate_name("k").incremental("incr").arg("-Zincremental-keep-configs=1");
    cmd.arg(format!("-Cmetadata={metadata}"));
    if test {
        cmd.arg("--test");
    } else {
        cmd.crate_type("rlib");
    }
    cmd.run();
}

/// The crate dirs of `k`, each with the slot that its compile recorded.
fn crate_dirs() -> Vec<(std::path::PathBuf, String)> {
    shallow_find_directories("incr", |p| p.file_name().unwrap().to_str().unwrap().starts_with("k-"))
        .into_iter()
        .map(|dir| {
            let slot = rfs::read_to_string(dir.join("config-slot"));
            (dir, slot)
        })
        .collect()
}

fn lib_dir(dirs: &[(std::path::PathBuf, String)]) -> std::path::PathBuf {
    let libs: Vec<_> = dirs.iter().filter(|(_, slot)| slot.contains("test=false")).collect();
    assert_eq!(libs.len(), 1, "expected one lib config: {dirs:?}");
    libs[0].0.clone()
}

fn main() {
    rfs::write("k.rs", "pub fn f() -> u32 { 7 }\n#[test]\nfn t() { assert_eq!(f(), 7); }\n");

    build("one", false);
    build("one", true);
    let dirs = crate_dirs();
    assert_eq!(dirs.len(), 2, "the lib and its test build must both keep a dir: {dirs:?}");
    let old_lib = lib_dir(&dirs);
    // Pruning ignores a crate dir used in the last several seconds.
    std::thread::sleep(std::time::Duration::from_secs(11));

    // A new configuration of the lib replaces the lib dir and leaves the test dir.
    build("two", false);
    let dirs = crate_dirs();
    assert_eq!(dirs.len(), 2, "keep-configs=1 must leave one lib and one test dir: {dirs:?}");
    let new_lib = lib_dir(&dirs);
    assert_ne!(new_lib, old_lib, "the new lib configuration did not get its own dir");
    assert!(!old_lib.exists(), "the old lib configuration was not pruned");
    assert_eq!(
        dirs.iter().filter(|(_, slot)| slot.contains("test=true")).count(),
        1,
        "the test build's dir was pruned: {dirs:?}"
    );
}
