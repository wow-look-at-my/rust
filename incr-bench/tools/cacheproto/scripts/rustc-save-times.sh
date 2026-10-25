#!/usr/bin/env bash
# ignore-tidy-linelength Prints the wall time and rustc's own save-phase times
# for syn after a comment edit, from -Ztime-passes.
set -euo pipefail
rustc_bin=$1
target_dir=$2
runs=$3
flags="-Ztime-passes ${EXTRA_RUSTFLAGS:-}"
# CORPUS names a private copy, so a benchmark on the shared corpus keeps its edits.
corpus=${CORPUS:-/home/user/cache-ws}
cargo=/home/user/rust/build/host/stage0/bin/cargo
trap 'git -C "$corpus" checkout -q -- syn' EXIT
cd "$corpus"
build() {
	CARGO_HOME=/home/user/bench-corpus/targets/base/cargo-home CARGO_INCREMENTAL=1 RUSTC_BOOTSTRAP=1 RUSTC="$rustc_bin" \
		CARGO_TARGET_DIR="$target_dir" RUSTFLAGS="$flags" "$cargo" build -j1 -p syn 2>&1
}
if [[ ${FRESH:-0} == 1 ]]; then rm -rf "$target_dir"; fi
# Same RUSTFLAGS as the timed builds, so cargo keeps the incremental dir.
build >/dev/null
for i in $(seq 1 "$runs"); do
	printf '// save-times %s\n' "$i" >>syn/src/lib.rs
	start=$(date +%s%N)
	out=$(build)
	end=$(date +%s%N)
	grep -q "Finished" <<<"$out" || { tail -20 <<<"$out"; exit 1; }
	p=$(grep -E "time: .*incr_comp_persist_result_cache" <<<"$out" | awk '{print $2}' | tr -d ';')
	s=$(grep -E "time: .*incr_comp_serialize_result_cache" <<<"$out" | awk '{print $2}' | tr -d ';')
	crate=$(ls -d "$target_dir"/debug/incremental/syn-*/ | head -1)
	qc=$(find "$crate" -name query-cache.bin -newer syn/src/lib.rs -printf '%s\n' | head -1)
	dg=$(find "$crate" -name dep-graph.bin -newer syn/src/lib.rs -printf '%s\n' | head -1)
	qv=$(stat -c %s "$crate/query-values.bin" 2>/dev/null || echo 0)
	echo "run $i: wall $(((end - start) / 1000000)) ms persist_result_cache ${p}s serialize_result_cache ${s}s query-cache.bin ${qc} B query-values.bin ${qv} B dep-graph.bin ${dg} B"
done
