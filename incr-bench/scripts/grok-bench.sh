#!/usr/bin/env bash
# Cold build of the grok pager tests, then edits in xai-grok-shell, with the incremental cache
# measured after each step. Usage: grok-bench.sh RUSTC LABEL
set -euo pipefail
rustc_bin=$1
label=$2
repo=/home/user/grok-build
target=/home/user/grok-target-$label
src=$repo/crates/codegen/xai-grok-shell/src/lib.rs
backup=/mnt/cc-tmpfs/claude-0/-home-user/e6885dc8-6e9b-58ac-95cc-07530589c20c/scratchpad/grok-shell-lib.rs.bak
cp "$src" "$backup"
trap 'cp "$backup" "$src"' EXIT
export RUSTC="$rustc_bin" RUSTC_BOOTSTRAP=1 CARGO_INCREMENTAL=1 CARGO_TARGET_DIR="$target"
cd "$repo"

measure() {
	local step=$1 secs=$2 inc=$target/debug/incremental
	local graph qc objs dirs sessions
	graph=$(find "$inc" -name dep-graph.bin -printf '%s\n' | awk '{s+=$1} END{printf "%.3f", s/1e9}')
	qc=$(find "$inc" -name query-cache.bin -printf '%s\n' | awk '{s+=$1} END{printf "%.3f", s/1e9}')
	objs=$(find "$inc" -name '*.o' -printf '%s\n' | awk '{s+=$1} END{printf "%.3f", s/1e9}')
	dirs=$(find "$inc" -mindepth 1 -maxdepth 1 -type d | wc -l)
	sessions=$(find "$inc" -mindepth 2 -maxdepth 2 -type d -name 's-*' | wc -l)
	printf '%-10s %-14s %8.1fs graph=%sGB qcache=%sGB objs=%sGB total=%s dirs=%d sessions=%d\n' \
		"$label" "$step" "$secs" "$graph" "$qc" "$objs" "$(du -sh "$inc" | cut -f1)" "$dirs" "$sessions"
}

build() {
	local step=$1 start end
	start=$(date +%s.%N)
	cargo test -q -p xai-grok-pager --no-run > "$target.$step.log" 2>&1
	end=$(date +%s.%N)
	measure "$step" "$(echo "$end - $start" | bc)"
}

build cold
printf '\n// grok-bench comment edit\n' >> "$src"
build edit:comment
printf '\n#[allow(dead_code)]\nfn grok_bench_extra_item() -> u32 {\n    7\n}\n' >> "$src"
build edit:new-item
cp "$backup" "$src"
build revert
