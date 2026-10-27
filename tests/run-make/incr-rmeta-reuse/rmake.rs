// `-Z incremental-rmeta-reuse` decodes unchanged MIR from the session's metadata.

//@ ignore-cross-compile

use run_make_support::{rfs, run, rustc};

fn build_lib(incr: &str, out: &str, reuse: bool) -> String {
    let mut cmd = rustc();
    cmd.input("src/lib.rs").incremental(incr).out_dir(out).emit("metadata,link");
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
    rfs::create_dir("src");
    rfs::copy("lib_v1.rs", "src/lib.rs");
    build_lib("incr", "reuse", true);

    rfs::copy("lib_v2.rs", "src/lib.rs");
    let stderr = build_lib("incr", "reuse", true);
    assert!(decoded_count(&stderr) > 0, "no MIR body was decoded:\n{stderr}");

    build_lib("fresh-incr", "fresh", false);
    assert!(
        rfs::read("reuse/libmeta_reuse.rmeta") == rfs::read("fresh/libmeta_reuse.rmeta"),
        "the metadata with decoded MIR differs from a fresh encode"
    );

    rustc().input("main.rs").extern_("meta_reuse", "reuse/libmeta_reuse.rlib").run();
    run("main");
}
