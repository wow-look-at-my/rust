// `-Z incremental-rmeta-reuse` still decodes unchanged MIR after a dependency changed.

//@ ignore-cross-compile

use run_make_support::{rfs, run, rustc};

fn build_dep(src: &str) {
    rustc().input(src).out_dir("dep").run();
}

fn build_lib(incr: &str, out: &str, reuse: bool) -> String {
    let mut cmd = rustc();
    cmd.input("lib.rs").incremental(incr).out_dir(out).emit("metadata,link");
    cmd.extern_("reuse_dep", "dep/libreuse_dep.rlib");
    cmd.arg("-Zincremental-info");
    if reuse {
        cmd.arg("-Zincremental-rmeta-reuse");
    }
    cmd.run().stderr_utf8()
}

fn decoded_count(stderr: &str) -> usize {
    let line = stderr
        .lines()
        .find(|l| l.contains("[incremental] metadata: decoded"))
        .unwrap_or_else(|| panic!("no MIR reuse report in:\n{stderr}"));
    let rest = line.split("decoded ").nth(1).unwrap();
    rest.split(' ').next().unwrap().parse().unwrap()
}

fn main() {
    build_dep("dep_v1.rs");
    build_lib("incr", "reuse", true);

    build_dep("dep_v2.rs");
    let stderr = build_lib("incr", "reuse", true);
    assert!(decoded_count(&stderr) > 0, "no MIR body was decoded after the change:\n{stderr}");

    build_lib("fresh-incr", "fresh", false);
    assert!(
        rfs::read("reuse/libreuse_user.rmeta") == rfs::read("fresh/libreuse_user.rmeta"),
        "the metadata with decoded MIR differs from a fresh encode"
    );

    rustc()
        .input("main.rs")
        .extern_("reuse_user", "reuse/libreuse_user.rlib")
        .arg("-Ldependency=dep")
        .run();
    run("main");
}
