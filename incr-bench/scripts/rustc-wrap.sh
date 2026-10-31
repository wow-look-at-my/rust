#!/usr/bin/env bash
# RUSTC_WRAPPER for bench.sh. Cargo calls it as: rustc-wrap.sh <rustc> <args...>.
# It moves the "[incremental]" lines of each rustc call into the file
# "$BENCH_INCR_LOG.<crate>.<pid>" and passes all other stderr through.
# With UNCACHE_KINDS set (empty included), an incremental call also gets -Zincremental-uncache, and with
# SELF_PROFILE_DIR it gets -Zself-profile. They go here and not in RUSTFLAGS so cargo fingerprints stay.
rustc=$1
shift
crate=""
prev=""
incremental=0
for a in "$@"; do
	[[ $prev == --crate-name ]] && crate=$a
	[[ $a == incremental=* ]] && incremental=1
	prev=$a
done
if ((incremental)); then
	[[ -n ${UNCACHE_KINDS+set} ]] && set -- "$@" "-Zincremental-uncache=$UNCACHE_KINDS"
	[[ -n ${SELF_PROFILE_DIR:-} ]] && set -- "$@" "-Zself-profile=$SELF_PROFILE_DIR"
fi
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
