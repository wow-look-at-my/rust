#!/usr/bin/env bash
# Prints rustc's own save-phase times for syn after a comment edit, from -Ztime-passes.
# promotion = persist_result_cache - serialize_result_cache.
# Usage: rustc-save-times.sh RUSTC TARGET_DIR RUNS
set -euo pipefail
rustc_bin=$1
target_dir=$2
runs=$3
corpus=/home/user/bench-corpus/ws
cargo=/home/user/rust/build/host/stage0/bin/cargo
trap 'git -C "$corpus" checkout -q -- syn' EXIT
cd "$corpus"
build() {
	CARGO_HOME=/home/user/bench-corpus/targets/base/cargo-home CARGO_INCREMENTAL=1 RUSTC_BOOTSTRAP=1 RUSTC="$rustc_bin" \
		CARGO_TARGET_DIR="$target_dir" RUSTFLAGS="$1" "$cargo" build -j1 -p syn 2>&1
}
# Same RUSTFLAGS as the timed builds, so cargo keeps the incremental dir.
build "-Ztime-passes" >/dev/null
for i in $(seq 1 "$runs"); do
	printf '// save-times %s\n' "$i" >>syn/src/lib.rs
	out=$(build "-Ztime-passes")
	p=$(grep -E "time: .*incr_comp_persist_result_cache" <<<"$out" | awk '{print $2}' | tr -d ';')
	s=$(grep -E "time: .*incr_comp_serialize_result_cache" <<<"$out" | awk '{print $2}' | tr -d ';')
	qc=$(find "$target_dir/debug/incremental" -path '*syn-*' -name query-cache.bin -newer syn/src/lib.rs -printf '%s\n' | head -1)
	echo "run $i: persist_result_cache ${p}s serialize_result_cache ${s}s query-cache.bin ${qc} B"
done
