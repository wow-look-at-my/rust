# Cache-size claims, checked against the source

Line numbers are for the branch at `e42db7d`.

| Claim | Verdict | Where |
|---|---|---|
| Each build configuration writes a full copy of the cache | True. The crate dir is `{crate_name}-{stable_crate_id}`. Cargo's `-C metadata` feeds the `StableCrateId`, so a new configuration gets a new dir with a full graph and query cache. | `compiler/rustc_incremental/src/persist/fs.rs:567-573` |
| Nothing evicts a superseded configuration dir | True upstream. Session GC looks only inside the current crate dir. The branch adds `-Zincremental-keep-configs=N`. | `fs.rs:580` (GC of one crate dir), `fs.rs:268`, `fs.rs:986` (`prune_crate_configs`) |
| Two sessions are kept per crate dir | False at rest. GC deletes every finalized session except the newest. A second session exists only while a compile runs, as the source of the new one. | `fs.rs:779-792` |
| A retention constant controls the session count | False. There is no constant. A working session dir younger than 10 s is never collected. | `fs.rs:575-577` |
| The query cache is reloaded and rewritten whole on every build | True upstream. Each save decodes every green value into memory and then writes the whole file again. The branch adds `-Zincremental-inplace-cache`, which skips both for entries that stay valid. | `persist/save.rs:79`, `rustc_middle/src/dep_graph/graph.rs:1078`, `rustc_middle/src/query/on_disk_cache.rs:380` (whole file), `:514` (in place) |
| The dep graph is about two thirds of the cache | True for grok at baseline: the graph was 3.17 GB and the query cache 2.37 GB. On the branch, the graph's edges are 62% of its bytes on syn. | `incr-bench/results/`, `-Zincremental-info` node bytes |
| The cache has no eviction by size | True upstream. The branch adds `-Zincremental-size-cap`, which skips a locked or recently used dir and never blocks. | `fs.rs:1007-1043` |
