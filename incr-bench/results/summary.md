# CI benchmark summary

One row per run, workload and config, at the `edit:body` step. Numbers from different runs come from different runners and patched commits. Compare a ratio only with a row of the same run.

`all but *.o` is everything in the incremental dirs except the object files. The `-Zincremental-inplace-cache` rows keep their query cache in a file that `graph+cache` does not count, so read those rows by `all but *.o`.

| run | workload | config | compiler commit | rustc flags | graph+cache MB | all but *.o MB | *.o MB | graph+cache % of *.o | all but *.o % of *.o | body edit incr / non-incr |
|---|---|---|---|---|---:|---:|---:|---:|---:|---:|
| 40206337966 | grok | baseline | `dc44e7f2c436` | none | 5420 | 6040 | 1772 | 306% | 341% | 0.30x |
| 40206337966 | grok | patched-inplace-rmeta | `206845ccebc4` | `-Zincremental-inplace-cache -Zincremental-rmeta-reuse -Zincremental-info` | 1363 | 3806 | 1772 | 77% | 215% | 0.25x |
| 40206337966 | grok | patched-inplace | `206845ccebc4` | `-Zincremental-inplace-cache` | 1363 | 3806 | 1772 | 77% | 215% | 0.25x |
| 40206337966 | grok | patched-rmeta-uncache-all | `206845ccebc4` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root,items_of_instance -Zincremental-info` | 1737 | 2358 | 1772 | 98% | 133% | 0.74x |
| 40206337966 | grok | patched-rmeta-uncache-mir | `206845ccebc4` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir -Zincremental-info` | 2413 | 3034 | 1772 | 136% | 171% | 0.32x |
| 40206337966 | grok | patched-rmeta | `206845ccebc4` | `-Zincremental-rmeta-reuse -Zincremental-info` | 3143 | 3764 | 1772 | 177% | 212% | 0.28x |
| 40206337966 | grok | patched-uncache-all | `206845ccebc4` | `-Zincremental-uncache=optimized_mir,typeck_root,items_of_instance` | 1737 | 2358 | 1772 | 98% | 133% | 0.86x |
| 40206337966 | grok | patched-uncache-mir | `206845ccebc4` | `-Zincremental-uncache=optimized_mir` | 2414 | 3035 | 1772 | 136% | 171% | 0.37x |
| 40206337966 | grok | patched | `206845ccebc4` | none | 3143 | 3764 | 1772 | 177% | 212% | 0.24x |
| 40206337966 | syn | baseline | `dc44e7f2c436` | none | 69 | 74 | 38 | 179% | 193% | 0.24x |
| 40206337966 | syn | patched-inplace-rmeta | `206845ccebc4` | `-Zincremental-inplace-cache -Zincremental-rmeta-reuse -Zincremental-info` | 22 | 47 | 38 | 57% | 122% | 0.28x |
| 40206337966 | syn | patched-inplace | `206845ccebc4` | `-Zincremental-inplace-cache` | 22 | 47 | 38 | 57% | 122% | 0.24x |
| 40206337966 | syn | patched-rmeta-uncache-all | `206845ccebc4` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root,items_of_instance -Zincremental-info` | 27 | 32 | 38 | 70% | 84% | 0.82x |
| 40206337966 | syn | patched-rmeta-uncache-mir | `206845ccebc4` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir -Zincremental-info` | 34 | 39 | 38 | 88% | 102% | 0.24x |
| 40206337966 | syn | patched-rmeta | `206845ccebc4` | `-Zincremental-rmeta-reuse -Zincremental-info` | 42 | 47 | 38 | 109% | 123% | 0.25x |
| 40206337966 | syn | patched-uncache-all | `206845ccebc4` | `-Zincremental-uncache=optimized_mir,typeck_root,items_of_instance` | 27 | 32 | 38 | 70% | 84% | 0.88x |
| 40206337966 | syn | patched-uncache-mir | `206845ccebc4` | `-Zincremental-uncache=optimized_mir` | 34 | 39 | 38 | 88% | 102% | 0.37x |
| 40206337966 | syn | patched | `206845ccebc4` | none | 42 | 47 | 38 | 109% | 123% | 0.24x |
| 40210665335 | grok | patched-inplace-rmeta-uncache-mir | `e42db7dfbc3a` | `-Zincremental-inplace-cache -Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir -Zincremental-info` | 1143 | 2833 | 1772 | 64% | 160% | 0.32x |
| 40210665335 | grok | patched-rmeta-uncache-all | `e42db7dfbc3a` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root,items_of_instance -Zincremental-info` | 1506 | 2127 | 1772 | 85% | 120% | 0.81x |
| 40210665335 | grok | patched-rmeta-uncache-mir | `e42db7dfbc3a` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir -Zincremental-info` | 2192 | 2812 | 1772 | 124% | 159% | 0.36x |
| 40210665335 | grok | patched | `e42db7dfbc3a` | `-Zincremental-info` | 2924 | 3545 | 1772 | 165% | 200% | 0.26x |
| 40210665335 | syn | patched-inplace-rmeta-uncache-mir | `e42db7dfbc3a` | `-Zincremental-inplace-cache -Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir -Zincremental-info` | 19 | 35 | 38 | 49% | 92% | 0.32x |
| 40210665335 | syn | patched-rmeta-uncache-all | `e42db7dfbc3a` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root,items_of_instance -Zincremental-info` | 23 | 29 | 38 | 61% | 75% | 0.80x |
| 40210665335 | syn | patched-rmeta-uncache-mir | `e42db7dfbc3a` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir -Zincremental-info` | 30 | 36 | 38 | 79% | 93% | 0.25x |
| 40210665335 | syn | patched | `e42db7dfbc3a` | `-Zincremental-info` | 39 | 44 | 38 | 101% | 115% | 0.26x |
| 40210911807 | grok | patched-inplace-rmeta-uncache-mir-typeck | `e42db7dfbc3a` | `-Zincremental-inplace-cache -Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root -Zincremental-info` | 1143 | 2248 | 1772 | 64% | 127% | 0.46x |
| 40210911807 | grok | patched-rmeta-uncache-mir-typeck | `e42db7dfbc3a` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root -Zincremental-info` | 1606 | 2227 | 1772 | 91% | 126% | 0.47x |
| 40210911807 | syn | patched-inplace-rmeta-uncache-mir-typeck | `e42db7dfbc3a` | `-Zincremental-inplace-cache -Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root -Zincremental-info` | 19 | 29 | 38 | 49% | 76% | 0.48x |
| 40210911807 | syn | patched-rmeta-uncache-mir-typeck | `e42db7dfbc3a` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root -Zincremental-info` | 24 | 29 | 38 | 63% | 77% | 0.46x |
| 40211210156 | grok | patched-inplace-rmeta-uncache-allmir-typeck | `e42db7dfbc3a` | `-Zincremental-inplace-cache -Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,mir_for_ctfe,promoted_mir,typeck_root -Zincremental-info` | 1141 | 1506 | 1772 | 64% | 85% | 0.44x |
| 40211210156 | grok | patched-rmeta-uncache-allmir-typeck | `e42db7dfbc3a` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,mir_for_ctfe,promoted_mir,typeck_root -Zincremental-info` | 1491 | 1492 | 1772 | 84% | 84% | 0.43x |
| 40211210156 | syn | patched-inplace-rmeta-uncache-allmir-typeck | `e42db7dfbc3a` | `-Zincremental-inplace-cache -Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,mir_for_ctfe,promoted_mir,typeck_root -Zincremental-info` | 19 | 24 | 38 | 49% | 62% | 0.50x |
| 40211210156 | syn | patched-rmeta-uncache-allmir-typeck | `e42db7dfbc3a` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,mir_for_ctfe,promoted_mir,typeck_root -Zincremental-info` | 24 | 24 | 38 | 63% | 63% | 0.47x |
| 40211481430 | grok | baseline | `dc44e7f2c436` | none | 5420 | 5421 | 1772 | 306% | 306% | 0.28x |
| 40211481430 | grok | best-keep1 | `e42db7dfbc3a` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,mir_for_ctfe,promoted_mir,typeck_root -Zincremental-keep-configs=1 -Zincremental-info` | 1496 | 1497 | 1769 | 85% | 85% | 0.84x |
| 40211481430 | grok | best | `e42db7dfbc3a` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,mir_for_ctfe,promoted_mir,typeck_root -Zincremental-info` | 1491 | 1492 | 1772 | 84% | 84% | 0.47x |
| 40211481430 | syn | baseline | `dc44e7f2c436` | none | 69 | 69 | 38 | 179% | 179% | 0.23x |
| 40211481430 | syn | best-keep1 | `e42db7dfbc3a` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,mir_for_ctfe,promoted_mir,typeck_root -Zincremental-keep-configs=1 -Zincremental-info` | 24 | 24 | 38 | 63% | 63% | 0.47x |
| 40211481430 | syn | best | `e42db7dfbc3a` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,mir_for_ctfe,promoted_mir,typeck_root -Zincremental-info` | 24 | 24 | 38 | 63% | 63% | 0.49x |
| 40220872313 | grok | best-size | `e42db7dfbc3a` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root` | 1606 | 1607 | 1772 | 91% | 91% | 0.51x |
| 40220872313 | grok | patched | `e42db7dfbc3a` | none | 2924 | 2925 | 1772 | 165% | 165% | 0.27x |
| 40226212072 | grok | baseline | `dc44e7f2c436` | none | 5420 | 5421 | 1772 | 306% | 306% | 0.25x |
| 40226212072 | grok | best-allmir | `6e2cc9f1a693` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,mir_for_ctfe,promoted_mir,typeck_root` | 1566 | 1568 | 1772 | 88% | 88% | 0.39x |
| 40226212072 | grok | best-size | `6e2cc9f1a693` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root` | 1682 | 1683 | 1772 | 95% | 95% | 0.41x |
| 40226212072 | grok | patched | `6e2cc9f1a693` | none | 3001 | 3002 | 1772 | 169% | 169% | 0.28x |
| 40226212072 | syn | baseline | `dc44e7f2c436` | none | 69 | 69 | 38 | 179% | 179% | 0.25x |
| 40226212072 | syn | best-allmir | `6e2cc9f1a693` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,mir_for_ctfe,promoted_mir,typeck_root` | 25 | 25 | 38 | 65% | 65% | 0.25x |
| 40226212072 | syn | best-size | `6e2cc9f1a693` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root` | 25 | 25 | 38 | 65% | 65% | 0.27x |
| 40226212072 | syn | patched | `6e2cc9f1a693` | none | 40 | 40 | 38 | 103% | 103% | 0.28x |
| 40237726975 | grok | best-size | `6e2cc9f1a693` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root` | 1682 | 1683 | 1772 | 95% | 95% | 0.41x |
| 40237726975 | grok | patched | `6e2cc9f1a693` | none | 3001 | 3002 | 1772 | 169% | 169% | 0.27x |
| 40247181375 | grok | baseline | `dc44e7f2c436` | none | 5421 | 5422 | 1772 | 306% | 306% | 0.28x |
| 40247181375 | grok | best-size | `849fcf126655` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root` | 1685 | 1686 | 1772 | 95% | 95% | 0.32x |
| 40247181375 | grok | patched | `849fcf126655` | none | 3005 | 3006 | 1772 | 170% | 170% | 0.25x |
| 40258879754 | grok | best-size | `849fcf126655` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root` | 1685 | 1686 | 1772 | 95% | 95% | 0.32x |
| 40258879754 | grok | uncache-mir | `849fcf126655` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir` | 2270 | 2272 | 1772 | 128% | 128% | 0.26x |
| 40271464506 | grok | baseline | `dc44e7f2c436` | none | 5420 | 5422 | 1772 | 306% | 306% | 0.25x |
| 40271464506 | grok | best-size | `9675a57f37ab` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root` | 1684 | 1685 | 1772 | 95% | 95% | 0.26x |
| 40289423253 | grok | baseline | `dc44e7f2c436` | none | 5420 | 5421 | 1772 | 306% | 306% | 0.31x |
| 40289423253 | grok | best-keep1 | `f7303973e7a9` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root -Zincremental-keep-configs=1` | 1682 | 1683 | 1770 | 95% | 95% | 0.26x |
| 40289423253 | grok | best-size | `f7303973e7a9` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root` | 1684 | 1685 | 1772 | 95% | 95% | 0.27x |
| 40289423253 | rustc | baseline | `dc44e7f2c436` | none | 3172 | 3173 | 482 | 658% | 659% | 0.29x |
| 40289423253 | rustc | best-keep1 | `f7303973e7a9` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root -Zincremental-keep-configs=1` | 1089 | 1090 | 482 | 226% | 226% | 0.32x |
| 40289423253 | rustc | best-size | `f7303973e7a9` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root` | 1089 | 1090 | 482 | 226% | 226% | 0.32x |
| 40305199699 | rustc | best-size-info | `f7303973e7a9` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root -Zincremental-info` | 1089 | 1090 | 482 | 226% | 226% | 0.36x |
| 40309260707 | grok | best-keep1-fp64 | `0c1f9e4b8459` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root -Zincremental-keep-configs=1` | 1592 | 1594 | 1771 | 90% | 90% | 0.28x |
| 40309260707 | grok | best-size-fp64 | `0c1f9e4b8459` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root` | 1595 | 1596 | 1773 | 90% | 90% | 0.26x |
| 40309260707 | rustc | best-keep1-fp64 | `0c1f9e4b8459` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root -Zincremental-keep-configs=1` | 1026 | 1027 | 482 | 213% | 213% | 0.31x |
| 40309260707 | rustc | best-size-fp64 | `0c1f9e4b8459` | `-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root` | 1026 | 1027 | 482 | 213% | 213% | 0.32x |
