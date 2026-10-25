#!/usr/bin/env bash
# Rebuild time after each edit, incremental against non-incremental, alternated so both see the same load.
# Usage: incr-vs-full.sh RUSTC ROUNDS
set -euo pipefail
rustc_bin=$1
rounds=$2
corpus=/home/user/bench-corpus/ws
cargo=/home/user/rust/build/x86_64-unknown-linux-gnu/stage0/bin/cargo
root=/home/user/bench-corpus/targets/ivf
export CARGO_HOME=/home/user/bench-corpus/targets/base-b/cargo-home RUSTC_BOOTSTRAP=1 RUSTC="$rustc_bin"
trap 'git -C "$corpus" checkout -q -- syn' EXIT
cd "$corpus"

build() {
	local mode=$1
	local start end
	start=$(date +%s.%N)
	CARGO_INCREMENTAL=$mode CARGO_TARGET_DIR=$root/incr$mode "$cargo" build -q --workspace
	end=$(date +%s.%N)
	echo "$end - $start" | bc
}

echo "warming both target dirs"
build 0 > /dev/null
build 1 > /dev/null

edit() {
	case $1 in
	comment) sed -i '0,/^pub fn parse_file/s//\/\/ bench comment\npub fn parse_file/' syn/src/lib.rs ;;
	body) sed -i 's/file.shebang = shebang;/file.shebang = shebang.filter(|s| !s.is_empty());/' syn/src/lib.rs ;;
	sig) sed -i 's/pub fn parse_file(mut content: \&str)/pub fn parse_file(mut content: \&str, _bench_extra: u32)/' syn/src/lib.rs ;;
	esac
	if git -C "$corpus" diff --quiet -- syn; then
		echo "edit $1 changed nothing" >&2
		exit 1
	fi
}

printf '%-8s %-5s %10s %10s\n' edit round full_s incr_s
for r in $(seq 1 "$rounds"); do
	for e in comment body sig; do
		edit "$e"
		full=$(build 0)
		incr=$(build 1)
		printf '%-8s %-5s %10.3f %10.3f\n' "$e" "$r" "$full" "$incr"
		git -C "$corpus" checkout -q -- syn
		build 0 > /dev/null
		build 1 > /dev/null
	done
done
