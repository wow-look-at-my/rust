# The brief's claims, checked against the unpatched source

Every line reference is to the baseline compiler, `dc44e7f2c436`, which is upstream before this branch.

| Claim in the brief | Verdict | Where |
|---|---|---|
| A full copy of the cache is written per configuration hash. | True. The crate dir name is the crate name plus the stable crate id. Cargo's `-C metadata` feeds that id, so each profile, feature set or flag set gets its own dir with its own graph and query cache. | `compiler/rustc_incremental/src/persist/fs.rs:542-546` (`crate_path`) |
| Nothing evicts a superseded directory. | True. Garbage collection only walks the session dirs inside one crate dir. No code looks at sibling crate dirs. | `fs.rs:555` (`garbage_collect_session_directories`), `fs.rs:753` |
| Two sessions are kept per directory. | False in steady state. Garbage collection deletes every finalized session but the newest. A second session dir exists only while a compile runs. Every CI row on this branch shows as many sessions as crate dirs. | `fs.rs:51` (module doc), `fs.rs:753` ("Delete all but the most recent of the candidates") |
| Each session writes the whole dep graph and query cache again. | True. A new session dir starts empty and reads the previous session dir. `save_dep_graph` then serializes the full query cache into a new file, and the encoder streams every node of the new graph. | `fs.rs:256-271` (`prepare_session_directory`), `compiler/rustc_incremental/src/persist/save.rs:81-83` |
| The format version must change when the encoding changes, and a mismatch must rebuild. | True. A mismatch is reported and the cache is not loaded. This branch moves it from 0 to 17. Each bump costs one cold build of every crate. | `compiler/rustc_incremental/src/persist/file_format.rs:29`, the check at `file_format.rs:145` |
| The graph lives in `compiler/rustc_query_system/src/dep_graph/serialized.rs`. | Moved. `rustc_query_system` is gone. The graph encoder is `compiler/rustc_middle/src/dep_graph/serialized.rs`. | `git ls-tree dc44e7f2c436 compiler/rustc_query_system` prints nothing |
| The graph is bigger than the data it indexes. | True. Every node has a fixed header of 38 bytes, which holds a 16-byte key fingerprint and a 16-byte result fingerprint, whatever the key kind. Edges use a fixed byte width per node. On grok-build the unpatched graph plus query cache is 313% of the object files (CI run 34). | `serialized.rs:442-448` (`SerializedNodeHeader`), `serialized.rs:25-35` (module doc), `serialized.rs:373` |
| `load.rs` and `data.rs` hold more of the cache. | `load.rs` reads the graph and the work products of the previous session. `data.rs` holds only `SerializedWorkProduct`, the list of object files. Neither stores per-config copies. | `compiler/rustc_incremental/src/persist/load.rs:35` (`load_dep_graph`), `compiler/rustc_incremental/src/persist/data.rs:6-13` |
| `save.rs` writes the graph and the query cache each session. | True. `save_dep_graph` writes both into the new session dir, through a staging file for the graph. | `save.rs:25`, `save.rs:38-51` |
| The `-Zincremental-*` options. | Upstream has `incremental-ignore-spans`, `incremental-info` and `incremental-verify-ich`, and nothing that prunes, caps or drops a query from the cache. | `compiler/rustc_session/src/options.rs:2583-2588` |

## What changed from the brief's plan

- **Session retention:** nothing to build. The source already keeps one finalized session.
- **Pruning superseded configs:** `-Zincremental-keep-configs`. It prunes only dirs with the same crate types, `--test` switch and target, because a lib and its test build share the crate name.
- **Size cap:** `-Zincremental-size-cap`. It is a safety net, not the fix: `results/cap-concurrent.txt` shows every capped compile going cold once the cap is under the working set.
- **Compression:** left out at the owner's instruction. The size came down by storing less instead: result fingerprints in 64 bits, keys and values as back-references, edges as varint deltas, and the MIR. It typeck results that the previous `.rmeta` or a cheap recompute can give no longer cached.
