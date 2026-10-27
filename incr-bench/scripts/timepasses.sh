#!/usr/bin/env bash
# Builds the corpus after a comment edit with -Ztime-passes and prints the incremental phase times.
# Usage: timepasses.sh RUSTC TARGET_DIR
set -euo pipefail
rustc_bin=$1
target_dir=$2
corpus=/home/user/bench-corpus/ws
cargo=/home/user/rust/build/x86_64-unknown-linux-gnu/stage0/bin/cargo
trap 'git -C "$corpus" checkout -q -- syn' EXIT
cd "$corpus"
for i in 1 2 3; do
	printf '// timepasses %s\n' "$i" >> syn/src/lib.rs
	CARGO_HOME=/home/user/bench-corpus/targets/base/cargo-home CARGO_INCREMENTAL=1 RUSTC_BOOTSTRAP=1 RUSTC="$rustc_bin" \
		CARGO_TARGET_DIR="$target_dir" RUSTFLAGS="-Ztime-passes" \
		"$cargo" build -p syn 2>&1 | grep -E "time: .*(incr_comp|serialize_dep_graph|total)" | grep -E "load_dep_graph|persist_dep_graph|encode_dep_graph|serialize_result_cache|total" || true
	echo "--"
done
