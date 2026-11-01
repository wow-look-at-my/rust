#!/usr/bin/env bash
# One comment edit in xai-grok-shell on an already-built target, profiled per crate.
# Usage: run.sh RUSTC TARGET_DIR
set -euo pipefail
repo=/home/user/grok-build
src=$repo/crates/codegen/xai-grok-shell/src/lib.rs
trap 'git -C "$repo" checkout -q -- crates/codegen/xai-grok-shell/src/lib.rs' EXIT
: > /home/user/incr-prof/log
cat /proc/loadavg
printf '\n// incr-prof comment edit\n' >> "$src"
cd "$repo"
start=$(date +%s.%N)
RUSTC="$1" RUSTC_WRAPPER=/home/user/incr-prof/wrap.sh RUSTC_BOOTSTRAP=1 CARGO_INCREMENTAL=1 CARGO_TARGET_DIR="$2" \
	cargo test -q -p xai-grok-pager --no-run > /home/user/incr-prof/cargo.log 2>&1
end=$(date +%s.%N)
echo "total $(echo "$end - $start" | bc)s"
cat /proc/loadavg
