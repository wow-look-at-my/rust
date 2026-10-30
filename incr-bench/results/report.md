# Report: unpatched compiler against this branch

The unpatched compiler is `dc44e7f2c436`. The patched numbers are from CI run 41048892988 (run 48), config `props`. That config uses the flags below. Each ratio is an incremental build divided by a non-incremental build of the same step on the same runner. This is because the runners of different runs differ in speed.

```
-Zincremental-rmeta-reuse -Zincremental-keep-configs=1 -Zincremental-extern-items
-Zincremental-transparent=symbol_name,callee_facts,is_freeze_raw,is_unpin_raw,is_unsafe_unpin_raw,adt_sizedness_constraint
-Zincremental-uncache=typeck_root,optimized_mir,mir_for_ctfe,promoted_mir,lookup_stability,lookup_deprecation_entry,lookup_const_stability,adt_def,impl_trait_header,codegen_select_candidate
```

## Size: graph and query cache against the object files

| workload | compiler | dep-graph.bin MB | query-cache.bin MB | *.o MB | graph+cache % of *.o |
|---|---|---:|---:|---:|---:|
| grok | unpatched (run 40289423253) | 3171.4 | 2248.4 | 1772.1 | 306% |
| grok | this branch (run 48) | 573.6 | 145.3 | 1773.5 | 40.5% |
| rustc | unpatched (run 40289423253) | 2098.0 | 1073.7 | 481.7 | 658% |
| rustc | this branch (run 48) | 379.4 | 106.0 | 488.0 | 99.5% |

The graph is 82% smaller on both workloads. Graph plus cache is 87% smaller on grok and 85% smaller on rustc.

## Time: an edit, incremental against non-incremental

| workload | step | unpatched | this branch |
|---|---|---:|---:|
| grok | edit:comment | 0.31x | 0.26x |
| grok | edit:body | 0.31x | 0.26x |
| grok | edit:new-item | 0.28x | 0.27x |
| grok | edit:signature | not run | 0.32x |
| rustc | edit:comment | 0.29x | 0.30x |
| rustc | edit:body | 0.29x | 0.32x |
| rustc | edit:new-item | 0.30x | 0.32x |

On grok each edit is faster than unpatched. On rustc each edit is within 10% of unpatched: the worst is the body edit at 0.32x against 0.29x.

`results/ci/41048892988.md` holds the node fates of each step: promoted green, re-run green, re-run red, unreached and new.

## Each change

`results/summary.md` holds one row per run and config at the body edit. Read each change there against the row of the same run. Run 48 adds changes to the compiler of run 46:

| rustc config | query cache MB | graph+cache % of *.o | body edit |
|---|---:|---:|---:|
| run 46 compiler | 126.1 | 104% | 0.33x |
| run 48 compiler | 106.0 | 99.5% | 0.32x |
| run 48, `eval_to_const_value_raw` and `eval_to_allocation_raw` uncached | 94.6 | 97% | 0.32x |
| run 48, `is_reachable_non_generic` transparent | 106.0 | 99% | 0.32x (grok signature edit 0.48x) |
| run 48, `items_of_instance` transparent and uncached | 52.2 | 83% | 0.66x |

The last two cost more time than they save in bytes, so the branch does not use them.

## The brief's plan against the source

`source-claims.md` checks each claim against the unpatched source, with file and line. A summary:

- **Old config directories:** `-Zincremental-keep-configs` prunes them, for directories of the same kind of compile only.
- **Sessions per directory:** the source already keeps one finalized session, so nothing was built for that step.
- **Size cap:** `-Zincremental-size-cap` is a safety net, not the fix. It never deletes a locked directory, a directory that ran in the last few seconds, or anything outside a crate directory.

## The format version

`HEADER_FORMAT_VERSION` in `compiler/rustc_incremental/src/persist/file_format.rs` moved from 0 to 20. A cache with another version is ignored and built again, not read. Each bump costs one cold build of each crate, which is the `cold` step in the tables of `results/ci/`.

## Gates

CI ran these on `6c5f8b2` (run 41065632534) and on `4f8c38c` (run 41048902253). Every job passed:

- `x.py check`, in `test-pr-check-1` and `test-pr-check-2`.
- The tests of every compiler crate, `rustc_incremental` and `rustc_middle` among them, in `test-aarch64-gnu-llvm-22-2`.
- The `ui`, `incremental` and `run-make` compiletest suites, in `test-x86_64-gnu-gcc`.

`tests/run-make/incremental-size-cap-concurrent` rebuilds some crates while concurrent compiles under the size cap evict every idle crate directory. It then rebuilds under `-Zincremental-verify-ich`, which hashes each loaded value again. Last, it links and runs the result.

## What this report does not show

- The gates ran in CI only. The local checkout is sparse and holds too little of the tree to build.
- The unpatched rows come from run 40289423253 and the patched rows from run 48, on different runners. Only the ratios compare across the runs.
