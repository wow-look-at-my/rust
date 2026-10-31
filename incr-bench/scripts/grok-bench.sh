#!/usr/bin/env bash
# Runs ci-bench.sh on the local grok-build checkout. Usage: grok-bench.sh RUSTC LABEL [FLAGS]
# Rows go to /home/user/grok-target-LABEL.tsv.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
label=$2
BENCH_CONFIG=$label BENCH_COMPILER=$label BENCH_TOOLCHAIN=local BENCH_RUSTC_FLAGS=${3:-} \
	exec "$here/ci-bench.sh" grok /home/user/grok-build "$1" "/home/user/grok-target-$label" "/home/user/grok-target-$label.tsv"
