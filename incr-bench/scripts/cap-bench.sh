#!/usr/bin/env bash
# Size-cap benchmark: crates share one incremental dir. Each round edits
# crates and compiles them at the same time, with and without
# -Zincremental-size-cap.
set -euo pipefail
rustc=$(realpath "${1:?usage: cap-bench.sh RUSTC WORKDIR}")
root=$(realpath -m "${2:?usage: cap-bench.sh RUSTC WORKDIR}")
crates=(a b c d e f)

gen() {
	local name=$1 round=$2
	for i in $(seq 0 1999); do
		echo "pub fn f$i(x: u64) -> u64 { let v: Vec<u64> = (0..x % 8).map(|y| y.wrapping_mul($i)).collect(); v.iter().sum::<u64>() ^ $i }"
	done
	echo "pub fn edited() -> u64 { $round }"
	echo "pub fn id() -> &'static str { \"$name\" }"
}

compile() {
	local name=$1 cap=$2
	local args=(--crate-type rlib --crate-name "$name" -Copt-level=0 -Cincremental=incr -Zincremental-info
		-Zincremental-rmeta-reuse -Zincremental-uncache=optimized_mir,typeck_root)
	[[ $cap == none ]] || args+=("-Zincremental-size-cap=$cap")
	"$rustc" "${args[@]}" "$name.rs" 2>&1 | grep -oE 'node fates: previous [0-9]+ promoted [0-9]+ rerun-green [0-9]+ rerun-red [0-9]+' | sed "s/^/$name: /"
}

run() {
	local label=$1 cap=$2 dir=$root/$1
	rm -rf "$dir"
	mkdir -p "$dir"
	cd "$dir"
	for c in "${crates[@]}"; do gen "$c" 0 > "$c.rs"; done
	# Round a value of zero compiles all crates one at a time, so every crate has a cache.
	for c in "${crates[@]}"; do compile "$c" "$cap" > /dev/null; done
	echo "== $label (cap $cap): after round 0, $(du -sb incr | cut -f1) bytes, $(find incr -maxdepth 1 -mindepth 1 -type d | wc -l) crate dirs"
	for round in 1 2 3 4 5 6; do
		local x=${crates[$(( (round * 2) % 6 ))]} y=${crates[$(( (round * 2 + 1) % 6 ))]}
		gen "$x" "$round" > "$x.rs"
		gen "$y" "$round" > "$y.rs"
		local start end
		start=$(date +%s.%N)
		compile "$x" "$cap" > "fates-$round-$x" &
		local px=$!
		compile "$y" "$cap" > "fates-$round-$y" &
		wait "$px" $!
		end=$(date +%s.%N)
		printf 'round %d (%s %s): %.1fs, %d bytes, %d crate dirs | %s | %s\n' "$round" "$x" "$y" \
			"$(echo "$end - $start" | bc)" "$(du -sb incr | cut -f1)" \
			"$(find incr -maxdepth 1 -mindepth 1 -type d | wc -l)" "$(cat "fates-$round-$x")" "$(cat "fates-$round-$y")"
	done
}

run nocap none
size=$(du -sb "$root/nocap/incr" | cut -f1)
# A cap of half the uncapped size forces evictions while both compiles hold their dirs.
run cap "$((size / 2))"
echo "uncapped size $size, cap $((size / 2))"
