# fish-shell build benchmark: C++ (3.7.1) vs Rust (4.1.2)

Machine: Linux 6.18.44 (see the facts table). Both builds use `-j3`, Debug / dev profile. Raw rows: `timings.tsv` (proj, label, seconds, exit code, `/proc/loadavg` before the step). Per-step build logs: `logs/`. Harness: `bench.sh` (phases `cold`, `noop`, `edits`, `redo <step>`, `cold2 [label]`), `edits.sh` (apply and revert each edit), `sizes.sh` (size breakdown).

## Toolchains

- C++: g++ (Ubuntu 13.3.0-6ubuntu2~24.04.1) 13.3.0 (`/usr/bin/c++`), CMake 3.28.3, Ninja 1.11.1. Flags: `-g -std=gnu++11 -Wall -Wextra -fno-exceptions`, no `-O`.
- Rust: rustc 1.97.0 (2d8144b78 2026-07-07), cargo 1.97.0 (c980f4866 2026-06-30), rustup `stable` toolchain in `/root/.rustup`. Dev profile defaults (opt-level 0, debuginfo, incremental on, codegen-units 256).

## What the default build builds

- C++ (`ninja`): `libfishlib.a` (static), `fish`, `fish_indent`, `fish_key_reader`, `fish_test_helper`, and the `.gmo` translation catalogs. `fish_tests` is `EXCLUDE_FROM_ALL`. Docs are off (no sphinx-build).
- Rust (`cargo build`, root package only): lib `fish` + bins `fish`, `fish_indent`, `fish_key_reader`. Default features `embed-data` and `localize-messages` (msgfmt runs in build.rs. Man pages skipped, no sphinx). 65 `Compiling` lines (third-party crates, workspace crates, build scripts).
- PCRE2: both link the system `libpcre2-32.so` (ldd). The Rust binary also links `libpcre2-8.so`.

## Edits

- Leaf: `src/builtins/random.cpp` / `src/builtins/random.rs`.
	- step 3: append a `//` comment line.
	- step 4: after `argc` is set, add `bench_extra_N = argc + N; if (bench_extra_N == 0) return STATUS_INVALID_ARGS;`.
- Core: `src/common.h` (+ `src/common.cpp` for step 6) / `src/common.rs`.
	- step 5: append a `//` comment line.
	- step 6: C++ adds `int bench_new_fn_N(int x);` in common.h and its definition in common.cpp. Rust adds `pub fn bench_new_fn_N(x: i32) -> i32 { x + N }` to common.rs.
- The facts table gives how many files name the core file.

## Facts

| fact | C++ | Rust |
|---|---|---|
| CPU cores on the box | 4 | 4 |
| default build graph | 117 ninja edges, 100 TUs in libfishlib | 65 `Compiling` lines |
| `.gmo` catalogs built | 8 | (maps in build OUT_DIR) |
| files that name the core file | 141 (`#include "common.h"`) | 93 (`common::`) |
| codegen units (dev/Debug) | one per TU | 256 per crate |
| all tracked source lines (`git ls-files` + `wc -l`) | 78727 (`*.cpp`, `*.h`) | 97861 (`*.rs`) |
| main sources without test files | 71085 (`src/`, minus fish_tests.cpp and fish_test_helper.cpp) | 87483 (`src/` + build.rs, minus `src/tests/`; inline `#[cfg(test)]` still counted) |
| test-file lines removed above | 7338 (fish_tests.cpp) | 6557 (`src/tests/`) |
| workspace helper crates | - | 3821 (`crates/`) |
- Each run uses a different N, so no run repeats the content of an earlier one. After each timed build, `git checkout` reverts the file and the build after that revert is timed too ("revert" column).

## C++ (CMake + Ninja, Debug)

| step | run1 s | run2 s | revert after run1 / run2 s | loadavg (1-min) before run1 / run2 |
|---|---|---|---|---|
| configure (cmake, once) | 5.16 | - | - | 0.18 |
| 1 cold build (empty dir) | 45.35 | 43.86 | 3rd run: 45.87 | 0.25 / 3.24 (3rd: 1.76) |
| 2 no-op | 0.06 | 0.05 | - | 1.64 / 1.64 |
| 3 leaf comment | 3.11 | 2.52 | 2.97 / 2.78 | 1.64 / 1.60 |
| 4 leaf body change | 3.45 | 3.31 | 3.11 / 2.98 | 1.46 / 1.41 |
| 5 core comment (common.h) | 49.04 | 47.38 | 46.43 / 45.77 | 2.43 / 3.55 |
| 6 core new function (common.h + common.cpp) | 43.55 | 45.93 | 42.58 / 48.52 | 2.81 / 2.81 |

## Rust (cargo, dev profile, incremental)

| step | run1 s | run2 s | revert after run1 / run2 s | loadavg (1-min) before run1 / run2 |
|---|---|---|---|---|
| 1 cold build (empty target/) | 44.78 | 32.90 | 3rd run: 37.27 | 2.10 / 3.30 (3rd: 2.57) |
| 2 no-op | 0.13 | 0.11 | - | 1.64 / 1.64 |
| 3 leaf comment | 5.63 | 5.19 | 5.40 / 5.46 | 1.71 / 1.55 |
| 4 leaf body change | 6.71 | 7.55 | 6.67 / 7.68 | 1.43 / 1.37 |
| 5 core comment (common.rs) | 5.81 | 5.50 | 5.50 / 5.56 | 3.87 / 3.88 |
| 6 core new pub fn (common.rs) | 6.79 | 6.81 | 6.28 / 6.99 | 3.23 / 2.80 |

The core-comment rows are from the `redo` phase. The original core-comment runs (C++ 48.75 / 44.41, Rust 5.50 / 5.21, and their reverts) overlapped an outside cargo build of about 25 s. They are discarded. They remain in `timings.tsv` without the `-redo` suffix.

Cold runs 2 and 3 started from `ninja -t clean` (CMake configuration kept) and `cargo clean`. Crate sources were already in `~/.cargo` for every Rust cold run (`cargo fetch` ran first, and no build log shows a download). Run 1 of each was the first build after checkout. The Rust cold spread (32.9 to 44.8 s) is larger than the C++ spread (43.9 to 45.9 s). This harness does not explain it.

Load: the 1-min loadavg column shows the value before each step. The previous `-j3` step raised it. No other process was seen to use CPU during the runs, except the one reported cargo build.

## Sizes

Apparent bytes are sums of `find -printf '%s'`. On-disk is `du -s`. In Rust `target/debug/`, the top-level binaries and `libfish.rlib` are hard links of files in `deps/`. The apparent total counts them twice. `du` counts them once.

### C++ `build/`

| category | after cold | after edit sequence |
|---|---|---|
| total, apparent | 338.3 MiB (151 files) | 339.4 MiB |
| total, du -s | 346772 KiB | 347912 KiB |
| object files `*.o` (103) | 114.5 MiB | 114.5 MiB |
| archive `libfishlib.a` | 113.0 MiB | 113.0 MiB |
| executables (fish 36.0, fish_indent 36.2, fish_key_reader 35.9 MiB, fish_test_helper, littlecheck.py) | 108.3 MiB | 108.3 MiB |
| objects + archives + executables | 335.8 MiB | 335.8 MiB |
| build.ninja | 0.30 MiB | 0.30 MiB |
| .ninja_deps | 0.17 MiB | 1.25 MiB |
| .ninja_log | 11 KiB | 41 KiB |
| CMakeCache.txt + top-level *.cmake | 0.2 MiB | 0.2 MiB |
| CMakeFiles/ non-object content (21 files) | 0.3 MiB | 0.3 MiB |
| `*.d` | 0 (Ninja folds depfiles into .ninja_deps) | 0 |
| metadata total | 0.9 MiB | 2.1 MiB |
| everything else (.gmo catalogs, compile_commands.json, config.h, test helpers) | 1.5 MiB | 1.5 MiB |

### Rust `target/`

| category | after cold | after edit sequence |
|---|---|---|
| total, apparent (hard links twice) | 1077.9 MiB (1379 files) | 1352.7 MiB (1711 files) |
| total, du -s (hard links once) | 774652 KiB | 972316 KiB |
| debug/incremental total | 313.9 MiB (du 322944 KiB) | 588.7 MiB (du 520552 KiB) |
| - dep-graph.bin | 123.2 MiB (13 files) | 238.4 MiB (17) |
| - query-cache.bin | 81.9 MiB (13) | 141.4 MiB (17) |
| - `*.o` codegen objects | 88.9 MiB (607) | 171.0 MiB (922) |
| - other (work products, lock files, etc.) | 20.0 MiB | 37.8 MiB |
| - crate incremental dirs | 13 | 13 |
| - session dirs `s-*` | 13 | 17 (2 each for fish lib, fish bin, fish_indent, fish_key_reader) |
| - the `fish` lib crate's dir alone | 276.0 MiB | 544.1 MiB |
| debug/deps total | 394.2 MiB (218 files) | 394.2 MiB |
| - `*.rlib` (70) | 185.6 MiB | 185.6 MiB |
| - `*.rmeta` (70) | 59.0 MiB | 59.0 MiB |
| - executables (3: fish 50.3, fish_indent 49.8, fish_key_reader 41.6 MiB) | 141.7 MiB | 141.7 MiB |
| - proc-macro `.so` (1) | 7.8 MiB | 7.8 MiB |
| - `*.d` (74) | 0.2 MiB | 0.2 MiB |
| debug/build (build-script binaries + OUT_DIRs) | 124.5 MiB | 124.5 MiB |
| debug/.fingerprint | 0.1 MiB | 0.1 MiB |
| debug/ top level (hard links: 3 bins + libfish.rlib 101.9 MiB) | 243.7 MiB | 243.7 MiB |
| everything else (fish-localization-map-cache, etc.) | 1.5 MiB | 1.5 MiB |

## Line counts

The facts table holds the counts. The C++ total includes the vendored `src/widecharwidth` and `cmake/checks`.

## Things that make the comparison unfair

- Different versions: 4.1.2 has features that 3.7.1 does not have. The ports are not line-for-line.
- Dependencies: C++ compiles only first-party code, and its libraries come from the system. Rust compiles third-party crates from source in each cold build (packages in Cargo.lock, including Windows-only ones that are not built), and runs build scripts (msgfmt, rsconf probes, phf codegen).
- Linking: C++ links each of its binaries against a 113 MiB `libfishlib.a`, so every edit re-archives the library and relinks each binary. Rust links each binary against `libfish.rlib`. Neither uses LTO in Debug/dev (release profile: `lto = true`, not measured).
- Codegen units: Rust dev splits each crate into codegen units with incremental reuse (see the facts table). C++ has one TU per file and no ccache.
- C++ cold time excludes the 5.16 s configure. Rust cold time includes dependency compilation and build scripts.
- C++ cold runs 2 and 3 kept the CMake configuration (`ninja -t clean`).
