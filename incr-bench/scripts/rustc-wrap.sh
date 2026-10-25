#!/usr/bin/env bash
# RUSTC_WRAPPER for bench.sh. Cargo calls it as: rustc-wrap.sh <rustc> <args...>.
# It moves the "[incremental]" lines of each rustc call into the file
# "$BENCH_INCR_LOG.<crate>.<pid>" and passes all other stderr through.
rustc=$1
shift
crate=""
prev=""
for a in "$@"; do
	[[ $prev == --crate-name ]] && crate=$a
	prev=$a
done
if [[ -z ${BENCH_INCR_LOG:-} || -z $crate ]]; then
	exec "$rustc" "$@"
fi
err=$(mktemp)
"$rustc" "$@" 2>"$err"
rc=$?
if grep -q '^\[incremental\]' "$err"; then
	grep '^\[incremental\]' "$err" >"$BENCH_INCR_LOG.$crate.$$"
fi
grep -v '^\[incremental\]' "$err" >&2
rm -f "$err"
exit $rc
