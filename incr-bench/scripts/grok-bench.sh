#!/usr/bin/env bash
# Cold build of the grok pager tests, then edits in xai-grok-shell, with the incremental cache
# measured after each step. Usage: grok-bench.sh RUSTC LABEL
# GROK_REPO, GROK_TARGET and GROK_BACKUP move the checkout, the target dir and the lib.rs backup.
# GROK_RESULTS names a TSV file that gets one row per step. GROK_TOOLCHAIN_ID goes in its id column.
set -euo pipefail
rustc_bin=$1
label=$2
repo=${GROK_REPO:-/home/user/grok-build}
target=${GROK_TARGET:-/home/user/grok-target-$label}
backup=${GROK_BACKUP:-/mnt/cc-tmpfs/claude-0/-home-user/e6885dc8-6e9b-58ac-95cc-07530589c20c/scratchpad/grok-shell-lib.rs.bak}
results=${GROK_RESULTS:-}
toolchain_id=${GROK_TOOLCHAIN_ID:-$label}
src=$repo/crates/codegen/xai-grok-shell/src/lib.rs
cp "$src" "$backup"
trap 'cp "$backup" "$src"' EXIT
export RUSTC="$rustc_bin" RUSTC_BOOTSTRAP=1 CARGO_INCREMENTAL=1 CARGO_TARGET_DIR="$target"
cd "$repo"
if [[ -n $results ]]; then
	mkdir -p "$(dirname "$results")"
	printf 'label\ttoolchain\tstep\twall_s\tdep_graph_bytes\tquery_cache_bytes\tobj_bytes\tother_bytes\ttotal_bytes\tcrate_dirs\tsessions\n' > "$results"
fi

sum_sizes() {
	find "$@" -type f -printf '%s\n' | awk '{s+=$1} END{printf "%d", s}'
}

measure() {
	local step=$1 secs=$2 inc=$target/debug/incremental
	local graph qc objs total other dirs sessions
	graph=$(sum_sizes "$inc" -name dep-graph.bin)
	qc=$(sum_sizes "$inc" -name query-cache.bin)
	objs=$(sum_sizes "$inc" -name '*.o')
	total=$(sum_sizes "$inc")
	other=$((total - graph - qc - objs))
	dirs=$(find "$inc" -mindepth 1 -maxdepth 1 -type d | wc -l)
	sessions=$(find "$inc" -mindepth 2 -maxdepth 2 -type d -name 's-*' | wc -l)
	awk -v l="$label" -v s="$step" -v t="$secs" -v g="$graph" -v q="$qc" -v o="$objs" -v x="$other" -v a="$total" -v d="$dirs" -v n="$sessions" \
		'BEGIN{printf "%-10s %-14s %8.1fs graph=%.3fGB qcache=%.3fGB objs=%.3fGB other=%.3fGB total=%.3fGB dirs=%d sessions=%d\n", l, s, t, g/1e9, q/1e9, o/1e9, x/1e9, a/1e9, d, n}'
	if [[ -n $results ]]; then
		printf '%s\t%s\t%s\t%.1f\t%d\t%d\t%d\t%d\t%d\t%d\t%d\n' \
			"$label" "$toolchain_id" "$step" "$secs" "$graph" "$qc" "$objs" "$other" "$total" "$dirs" "$sessions" >> "$results"
	fi
}

build() {
	local step=$1 start end log=$target.${1//:/-}.log
	start=$(date +%s.%N)
	if ! cargo test -q -p xai-grok-pager --no-run > "$log" 2>&1; then
		tail -n 80 "$log" >&2
		echo "grok-bench: the $step build failed, full log in $log" >&2
		exit 1
	fi
	end=$(date +%s.%N)
	measure "$step" "$(echo "$end - $start" | bc)"
}

build cold
printf '\n// grok-bench comment edit\n' >> "$src"
build edit:comment
printf '\n#[allow(dead_code)]\nfn grok_bench_extra_item() -> u32 {\n    7\n}\n' >> "$src"
build edit:new-item
sed -i 's/^    7$/    8/' "$src"
grep -qx '    8' "$src" || { echo "grok-bench: the body edit did not apply to $src" >&2; exit 1; }
build edit:body
cp "$backup" "$src"
build revert
