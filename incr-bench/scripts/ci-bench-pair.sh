#!/usr/bin/env bash
# Runs the incremental steps of every config of a paired leg on this runner,
# so their wall times compare directly. Usage: ci-bench-pair.sh WORKLOAD SRC
# TOOLCHAINS TARGET_ROOT RESULTS_DIR LEG. TOOLCHAINS holds baseline/ and
# patched/. BENCH_PAIR_CONFIGS is a JSON list of {config, compiler, sha,
# flags}. BENCH_PAIR_ROUNDS is the round count. Each round starts one config
# later, so a drift in runner speed falls on every config.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
workload=$1
src=$2
toolchains=$(realpath "$3")
target_root=$(realpath -m "$4")
results_dir=$(realpath -m "$5")
leg=$6
configs=${BENCH_PAIR_CONFIGS:?}
rounds=${BENCH_PAIR_ROUNDS:?}
[[ $rounds =~ ^[1-9][0-9]*$ ]] || { echo "ci-bench-pair: BENCH_PAIR_ROUNDS is not a positive count: $rounds" >&2; exit 2; }
n=$(jq length <<< "$configs")
[[ $n -ge 2 ]] || { echo "ci-bench-pair: a paired leg needs two configs, got $n" >&2; exit 2; }

for ((r = 1; r <= rounds; r++)); do
	for ((i = 0; i < n; i++)); do
		c=$(jq -c ".[$(((i + r - 1) % n))]" <<< "$configs")
		name=$(jq -r .config <<< "$c")
		compiler=$(jq -r .compiler <<< "$c")
		echo "::group::round $r, $name ($compiler)"
		BENCH_CONFIG="$name@r$r" BENCH_COMPILER="$compiler" BENCH_TOOLCHAIN=$(jq -r .sha <<< "$c") \
			BENCH_RUSTC_FLAGS=$(jq -r .flags <<< "$c") BENCH_MODES=incr BENCH_PROFILE=0 \
			"$here/ci-bench.sh" "$workload" "$src" "$toolchains/$compiler/bin/rustc" \
			"$target_root/r$r-$name" "$results_dir/$leg.$name.r$r.tsv"
		echo "::endgroup::"
	done
done
