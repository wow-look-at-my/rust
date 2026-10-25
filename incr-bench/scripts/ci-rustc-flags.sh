#!/usr/bin/env bash
# RUSTC_WRAPPER that adds BENCH_RUSTC_FLAGS to each crate compile. Cargo does
# not see the flags, so its fingerprints stay the same. With BENCH_CRATE_TIMES
# set, it appends the wall time of each crate compile to that file.
rustc=$1
shift
crate=
kind=lib
prev=
for a in "$@"; do
	[[ $prev == --crate-name ]] && crate=$a
	[[ $a == --test ]] && kind=test
	[[ $prev == --crate-type && $a == bin ]] && kind=bin
	prev=$a
done
if [[ -z $crate ]]; then
	exec "$rustc" "$@"
fi
if [[ -z ${BENCH_CRATE_TIMES:-} ]]; then
	# shellcheck disable=SC2086
	exec "$rustc" "$@" ${BENCH_RUSTC_FLAGS:-}
fi
start=$(date +%s.%N)
# shellcheck disable=SC2086
"$rustc" "$@" ${BENCH_RUSTC_FLAGS:-}
rc=$?
end=$(date +%s.%N)
printf '%s:%s\t%s\n' "$crate" "$kind" "$(echo "$end - $start" | bc)" >> "$BENCH_CRATE_TIMES"
exit $rc
