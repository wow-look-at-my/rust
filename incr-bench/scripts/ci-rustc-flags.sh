#!/usr/bin/env bash
# RUSTC_WRAPPER that adds BENCH_RUSTC_FLAGS to each crate compile. Cargo does
# not see the flags, so its fingerprints stay the same.
rustc=$1
shift
for a in "$@"; do
	if [[ $a == --crate-name ]]; then
		# shellcheck disable=SC2086
		exec "$rustc" "$@" ${BENCH_RUSTC_FLAGS:-}
	fi
done
exec "$rustc" "$@"
