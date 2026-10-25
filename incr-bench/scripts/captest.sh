#!/usr/bin/env bash
set -euo pipefail
rustc_bin=$1
dir=/home/user/bench-corpus/captest
/bin/rm -rf "$dir"
mkdir -p "$dir/src"
incr=$dir/incr

gen() {
	local out=$1
	: > "$out"
	for i in $(seq 1 3000); do
		printf 'pub fn f%d(x: u64) -> u64 { x.wrapping_mul(%d) ^ (x >> 3) }\n' "$i" "$i" >> "$out"
	done
}

build() {
	local cfg=$1 log=$2
	RUSTC_BOOTSTRAP=1 "$rustc_bin" --edition 2021 --crate-type lib --crate-name cap \
		-C metadata="$cfg" -C incremental="$incr" -Zincremental-size-cap=1 -Zincremental-verify-ich \
		-Zincremental-info --out-dir "$dir/out-$cfg" "$dir/src/$cfg.rs" > "$log" 2>&1
}

fail() {
	echo "FAIL: $*" >&2
	exit 1
}

for cfg in stale1 stale2 stale3 live_a live_b; do
	gen "$dir/src/$cfg.rs"
done

echo "== seeding stale configurations"
for cfg in stale1 stale2 stale3; do
	build "$cfg" "$dir/seed-$cfg.log" || fail "seed build $cfg"
done
ls "$incr" | sed 's/-.*//' | sort | uniq -c
echo "== waiting past the recent-use guard"
sleep 11

echo "== two concurrent build loops"
loop() {
	local cfg=$1
	for it in 1 2 3 4 5; do
		printf '// edit %s\n' "$it" >> "$dir/src/$cfg.rs"
		build "$cfg" "$dir/loop-$cfg-$it.log" || { echo "build $cfg iteration $it failed"; return 1; }
	done
}
loop live_a & pid_a=$!
loop live_b & pid_b=$!
wait "$pid_a" || fail "live_a loop"
wait "$pid_b" || fail "live_b loop"

if grep -v -h '^\[incremental\]' "$dir"/loop-*.log | grep -i -E "error|corrupt|could not|failed to|warning"; then
	fail "a concurrent build reported a problem"
fi

crate_dirs=$(ls "$incr" | wc -l)
echo "crate dirs after the loops: $crate_dirs (want 2: the stale ones evicted, the live ones kept)"
[[ $crate_dirs -eq 2 ]] || fail "expected 2 crate dirs, found $crate_dirs"

echo "== no-change rebuild of each live configuration must reuse its cache"
for cfg in live_a live_b; do
	build "$cfg" "$dir/final-$cfg.log" || fail "final build $cfg"
	if grep -q -i "ignoring\|incompatible\|could not" "$dir/final-$cfg.log"; then
		fail "final build $cfg did not reuse its cache"
	fi
	grep -E "Total Node Count" "$dir/final-$cfg.log" | head -1
done
echo "PASS"
