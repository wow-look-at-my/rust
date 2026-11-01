# incr-bench

Tools and results for measuring rustc's incremental cache. The compiler changes are on the `claude/funny-pascal-zdac1j` branch of `wow-look-at-my/rust`.

## Tools

Build each with `go-toolchain`.

- `tools/dgstat`: byte breakdown of `dep-graph.bin` and `query-cache.bin`, the `collapse` simulation, `kinds` and `kindedges`.
- `tools/dgdump`: prints sample nodes with their key, value and edge target kinds.

## Scripts

- `scripts/bench.sh`: cache size and rebuild time of one rustc on the syn corpus.
- `scripts/rustc-wrap.sh`: the `RUSTC_WRAPPER` that `bench.sh` uses.
- `scripts/timepasses.sh`: incremental phase times after a comment edit.
- `scripts/incr-vs-full.sh`: incremental against non-incremental rebuilds, alternated.
- `scripts/captest.sh`: the cache size cap under concurrent builds.
- `scripts/grok-bench.sh`: `ci-bench.sh` on the local grok-build checkout.
- `scripts/ci-bench.sh`: one CI leg. A workload (grok or syn) with one compiler config, incremental and non-incremental, cold and after each edit.
- `scripts/ci-*.sh`: the other steps of `.github/workflows/incr-bench.yml`. `ci-syn-corpus.sh` builds the syn workspace from `corpus/syn-ws` and the crates.io source.
- `scripts/absl-bench.sh`: abseil-cpp build, output against CMake metadata.
- `scripts/fish-bench.sh`, `fish-edits.sh`, `fish-sizes.sh`: fish-shell C++ against Rust builds.
- `scripts/incr-prof-run.sh`, `incr-prof-wrap.sh`: one grok-build edit, profiled per crate.

## CI benchmark

A push that changes `ci-request.env` runs `.github/workflows/incr-bench.yml`. The file names `baseline`, `patched`, `grok_ref`, `workloads`, and one `config=NAME baseline|patched [RUSTC FLAGS]` line per config. Each config and workload is one leg. A `paired=WORKLOAD:ROUNDS ...` line adds one leg per workload that runs the incremental steps of every config on one runner, round after round. And the report divides each wall time by the baseline compiler's in the same round. The report lands in the job summary and in `results/ci/<run id>.md`.

## Results

- `source-claims.md`: each claim of the original brief, checked against the unpatched source with file and line.

- `results/results.csv`, `results2.csv`, `results-stage0.csv`: `bench.sh` rows per compiler.
- `results/run-*.log`: `bench.sh` logs per compiler.
- `results/syn-incr-vs-full.txt`: `incr-vs-full.sh` on syn.
- `results/grok-base.txt`, `grok-patched.txt`: the earlier `grok-bench.sh` with the base and patched compiler.
- `results/ci/`: one report per CI benchmark run.
- `results/summary.md`: every config of every CI run at the body edit, from `scripts/ci-summary.sh results/ci`.
- `results/cap-concurrent.txt`: `scripts/cap-bench.sh`, crates in one incremental dir, compiles at a time, with and without `-Zincremental-size-cap`.
- `results/absl.txt`: abseil-cpp build split.
- `results/stat-*.txt`: `dgstat` on the grok sample graphs.
- `results/collapse-*.txt`: `dgstat collapse` on the grok sample graphs.
- `results/kinds-baseline-check.txt`: the kind list checked against the baseline compiler.
- `results/fish-cpp-vs-rust.md`, `fish-timings.tsv`: fish-shell build benchmark write-up and raw rows.
