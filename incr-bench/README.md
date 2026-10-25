# incr-bench

Tools and results for measuring rustc's incremental cache. The compiler changes are on the `claude/funny-pascal-zdac1j` branch of `wow-look-at-my/rust`.

- `tools/dgstat` reads `dep-graph.bin` and `query-cache.bin`. It bytes per field, per dep kind, per owning crate, and a simulation of a graph that keeps only some node kinds. Build it with `go-toolchain`.
- `tools/dgdump` prints sample nodes with their key, value and the kind of each edge target.
- `scripts/` holds the benchmark harnesses: `bench.sh` (syn corpus edits), `grok-bench.sh` (grok-build pager tests), `incr-vs-full.sh`, `captest.sh` (size cap under concurrent builds), `absl-bench.sh` (CMake comparison).
- `results/` holds the raw output of each run.
