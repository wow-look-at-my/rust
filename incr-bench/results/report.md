# Report: unpatched compiler against this branch

The unpatched compiler is `dc44e7f2c436`. The numbers are from CI run 41094926524 (run 49), which ran the unpatched compiler and the branch at `19951ad` in one run. Config `props` uses the flags below. `props-ctfe` adds `eval_to_const_value_raw,eval_to_allocation_raw` to `-Zincremental-uncache`. Each ratio is an incremental build divided by a non-incremental build of the same step in the same job. Each job gets its own runner, and the runners differ in speed.

```
-Zincremental-rmeta-reuse -Zincremental-keep-configs=1 -Zincremental-extern-items
-Zincremental-transparent=symbol_name,callee_facts,is_freeze_raw,is_unpin_raw,is_unsafe_unpin_raw,adt_sizedness_constraint
-Zincremental-uncache=typeck_root,optimized_mir,mir_for_ctfe,promoted_mir,lookup_stability,lookup_deprecation_entry,lookup_const_stability,adt_def,impl_trait_header,codegen_select_candidate
```

## Size: graph and query cache against the object files

| workload | compiler | dep-graph.bin MB | query-cache.bin MB | *.o MB | graph+cache % of *.o |
|---|---|---:|---:|---:|---:|
| grok | unpatched | 3178.8 | 2253.7 | 1776.1 | 306% |
| grok | props | 573.6 | 145.3 | 1773.5 | 41% |
| grok | props-ctfe | 573.6 | 136.8 | 1773.5 | 40% |
| rustc | unpatched | 2098.0 | 1073.7 | 481.7 | 658% |
| rustc | props | 376.2 | 104.8 | 481.7 | 100% |
| rustc | props-ctfe | 376.2 | 93.5 | 481.7 | 98% |

The rows are the body edit. The graph is 82% smaller on both workloads. With `props-ctfe`, graph plus cache is 87% smaller on grok and 87% smaller on rustc.

## Time: an edit, incremental against non-incremental

| workload | step | unpatched | props | props-ctfe |
|---|---|---:|---:|---:|
| grok | edit:comment | 0.24x | 0.27x | 0.26x |
| grok | edit:body | 0.24x | 0.26x | 0.26x |
| grok | edit:new-item | 0.24x | 0.29x | 0.26x |
| grok | edit:signature | 0.29x | 0.32x | 0.33x |
| grok | revert | 0.29x | 0.34x | 0.30x |
| rustc | edit:comment | 0.31x | 0.35x | 0.30x |
| rustc | edit:body | 0.31x | 0.36x | 0.32x |
| rustc | edit:new-item | 0.31x | 0.36x | 0.32x |
| rustc | edit:signature | 0.32x | 0.35x | 0.32x |
| rustc | revert | 0.32x | 0.35x | 0.32x |

These ratios do not settle the 10% bound. `props` and `props-ctfe` run the same compiler, and `props-ctfe` caches less. Even so, their ratios differ by up to 12% in the incorrect direction: on rustc `props` is at 0.36x and `props-ctfe` at 0.32x. The non-incremental build of `props` took 27.3 s and that of `props-ctfe` 34.7 s, so the jobs ran on runners of different speed. The ratio did not cancel the difference. On these numbers `props-ctfe` is within 4% of unpatched on rustc, and between 3% and 14% over it on grok.

The paired legs of run 50 (41119563423) time every config on one runner, round after round, so a runner's speed falls on each config. They measure `b152c50`, which holds the span change. The grok leg ran one round:

| grok step | unpatched s | props s | props / unpatched | props-ctfe s | props-ctfe / unpatched |
|---|---:|---:|---:|---:|---:|
| cold | 868.4 | 828.1 | 0.95x | 801.6 | 0.92x |
| edit:comment | 88.9 | 98.1 | 1.10x | 91.9 | 1.03x |
| edit:body | 92.2 | 95.8 | 1.04x | 87.9 | 0.95x |
| edit:new-item | 92.9 | 99.7 | 1.07x | 88.8 | 0.96x |
| edit:signature | 97.1 | 109.4 | 1.13x | 102.0 | 1.05x |
| revert | 96.6 | 106.6 | 1.10x | 100.4 | 1.04x |
| config-change | 829.1 | 785.6 | 0.95x | 825.9 | 1.00x |

On grok `props-ctfe` is within 10% of unpatched at every step, and `props` is not. Writing the CTFE results costs more time than computing them again. One round gives no spread, which is why the rustc leg repeats its rounds.

The rustc leg repeated its rounds. Each cell is the median of the rounds' ratios to unpatched, with the range of the rounds after it:

| rustc step | props | props-ctfe |
|---|---:|---:|
| cold | 1.00x (0.99–1.01) | 1.01x (1.01–1.02) |
| edit:comment | 1.04x (1.04–1.06) | 1.07x (1.06–1.07) |
| edit:body | 1.06x (1.04–1.08) | 1.07x (1.07–1.09) |
| edit:new-item | 1.05x (1.05–1.08) | 1.08x (1.07–1.09) |
| edit:signature | 1.04x (1.04–1.05) | 1.05x (1.05–1.05) |
| revert | 1.05x (1.04–1.10) | 1.07x (1.04–1.09) |

CI run 41150372379 ran the grok paired leg again, with multiple rounds of unpatched and `props-ctfe`:

| grok step | round 1 | round 2 | median over this run and the round above |
|---|---:|---:|---:|
| cold | 1.08x | 1.09x | 1.08x |
| edit:comment | 1.13x | 1.12x | 1.12x |
| edit:body | 1.18x | 1.00x | 1.00x |
| edit:new-item | 1.10x | 0.99x | 0.99x |
| edit:signature | 1.10x | 1.04x | 1.05x |
| revert | 1.22x | 1.10x | 1.10x |

So the grok comment edit is over the 10% bound with `props-ctfe`, and the single round above did not show it. The self-profile of the comment edit names the costs in the `xai-grok-shell` crate. `symbol_name` ran for 338,132 instances in 3.73 s, where unpatched loads the names from its cache in 0.62 s. This is because the symbol check of the partitioner asks for the name of every mono item. The dep graph encode took 4.39 s for 6.0M nodes against 1.75 s for 10.6M nodes unpatched. `typeck_root` and `mir_borrowck` took 3.1 s more, for the bodies that a span change makes red and that the cache no longer holds.

The encode cost is a hash map lookup per edge, which `e0822ef` replaces with a direct-mapped table. The next bench run measures that change, and a config that caches `symbol_name` again.

CI run 41163473453 measured `e0822ef`, the direct-mapped edge table. `props-ctfe-sym` there is `props-ctfe` with `symbol_name` out of `-Zincremental-transparent`. The query keeps its node. The branch then held no value for it on disk. Medians over the rounds, against unpatched:

| step | grok props-ctfe | grok props-ctfe-sym | rustc props-ctfe | rustc props-ctfe-sym |
|---|---:|---:|---:|---:|
| cold | 1.00x | 0.96x | 0.99x | 1.01x |
| edit:comment | 1.14x | 1.01x | 1.11x | 1.04x |
| edit:body | 1.11x | 1.03x | 1.14x | 1.09x |
| edit:new-item | 1.19x | 1.11x | 1.13x | 1.08x |
| edit:signature | 1.10x | 1.01x | 1.10x | 1.07x |
| revert | 1.14x | 1.00x | 1.10x | 1.10x |

A transparent query has no node, so each task that calls it records the reads of the call. The codegen tasks then carry many more edges to check. The node costs 64.9 MB of graph on grok, where graph plus cache becomes 43% of the object files. On rustc it costs 28.4 MB, and the share becomes 99%. The edge table cut the dep graph encode of the grok comment edit from 4.39 s to 3.02 s and left the graph size the same. The paired legs of CI run 41163473453 hold `props-ctfe` slower on rustc than the rounds of CI run 41119563423 did, on the same graph bytes.

Both configs are within 10% of unpatched on every rustc step, and so is every round of `props-ctfe`. The rounds agree to within 5% of each other, against up to 12% between the per-config legs of run 49. `props-ctfe` meets the size bound on both workloads, with graph plus cache at 39% of the object files on grok and 94% on rustc. It meets the time bound on rustc, and not on the grok comment edit. The per-config legs of run 50 rank the two configs the other way on the grok body edit: 0.31x for `props-ctfe`, 0.26x for `props`.

A ratio across runners does not hold that much precision.

The faster of the two. A ratio across runners does not hold that much precision.

On the same runner, unpatched wrote a 3178.8 MB graph and a 2253.7 MB query cache at the body edit. `props-ctfe` wrote 575.6 MB and 115.6 MB, 39% of the 1773.5 MB of object files. On rustc, the `props` leg of run 50 wrote a 376.3 MB graph and an 85.4 MB query cache, 96% of the 481.7 MB of object files. In run 49 that cache was 104.8 MB: the span change took 19.4 MB out of it. `props-ctfe` wrote a 74.2 MB query cache, 94% of the object files, against 93.5 MB in run 49. `results/ci/` holds the node fates of each step: promoted green, re-run green, re-run red, unreached and new.

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

CI ran these on `b152c50` (run 41104963378), `6c5f8b2` (run 41065632534) and `4f8c38c` (run 41048902253). Every job passed:

- `x.py check`, in `test-pr-check-1` and `test-pr-check-2`.
- The tests of every compiler crate, `rustc_incremental` and `rustc_middle` among them, in `test-aarch64-gnu-llvm-22-2`.
- The `ui`, `incremental` and `run-make` compiletest suites, in `test-x86_64-gnu-gcc`.

`tests/run-make/incremental-size-cap-concurrent` rebuilds some crates while concurrent compiles under the size cap evict every idle crate directory. It then rebuilds under `-Zincremental-verify-ich`, which hashes each loaded value again. Last, it links and runs the result.

## What this report does not show

- The gates ran in CI only. The local checkout is sparse and holds too little of the tree to build.
- Every leg of run 49 ran on its own runner. A ratio cancels part of the speed difference between runners, not all of it, as the `props` and `props-ctfe` rows show.
- The time and size tables above the paired legs measured `19951ad`, before the span change of `8b34cfd`. The paired legs, and the sizes CI run 41119563423 gives, measured `b152c50`.
- The grok paired leg ran one round, so its ratios carry no spread.
