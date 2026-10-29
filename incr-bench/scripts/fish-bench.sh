#!/bin/bash
# Phases: cold | noop | edits | cold2. Each timed build appends one row to timings.tsv.
set -uo pipefail
B=/home/user/lang-bench
CPP=$B/fish-cpp
RS=$B/fish-rs
CB=$CPP/build
TSV=$B/timings.tsv
mkdir -p "$B/logs"
export CARGO_TERM_COLOR=never

build_cpp() { ninja -C "$CB" -j3; }
build_rs() { (cd "$RS" && cargo build -j3); }

# timed <proj> <label> <cmd...>
timed() {
	local proj=$1 label=$2
	shift 2
	local load s e rc
	load=$(cat /proc/loadavg)
	echo "== $proj $label loadavg: $load"
	s=$(date +%s.%N)
	"$@" > "$B/logs/$proj-$label.log" 2>&1
	rc=$?
	e=$(date +%s.%N)
	printf '%s\t%s\t%.2f\t%s\t%s\n' "$proj" "$label" "$(echo "$e - $s" | bc)" "$rc" "$load" >> "$TSV"
	tail -n 1 "$TSV"
	if [ "$rc" != 0 ]; then
		echo "BUILD FAILED: $proj $label, see $B/logs/$proj-$label.log" >&2
		exit 1
	fi
}

phase=$1
case $phase in
cold)
	df -h /
	mkdir -p "$CB"
	timed cpp configure cmake -S "$CPP" -B "$CB" -G Ninja -DCMAKE_BUILD_TYPE=Debug
	timed cpp cold-1 build_cpp
	"$B/sizes.sh" cpp > "$B/sizes-cpp-after-cold.txt"
	df -h /
	timed rs cold-1 build_rs
	"$B/sizes.sh" rs > "$B/sizes-rs-after-cold.txt"
	df -h /
	;;
noop)
	for r in 1 2; do
		timed cpp noop-$r build_cpp
		timed rs noop-$r build_rs
	done
	;;
edits)
	for step in 3 4 5 6; do
		for r in 1 2; do
			for p in cpp rs; do
				"$B/edits.sh" apply "$p" "$step" "$r" || exit 1
				timed "$p" "step$step-run$r" "build_$p"
				"$B/edits.sh" revert "$p"
				timed "$p" "step$step-run$r-revert" "build_$p"
			done
		done
	done
	"$B/sizes.sh" cpp > "$B/sizes-cpp-after-edits.txt"
	"$B/sizes.sh" rs > "$B/sizes-rs-after-edits.txt"
	;;
redo)
	# Repeat the edit steps named as arguments, for runs that overlapped an outside build.
	shift
	for step in "$@"; do
		for r in 1 2; do
			for p in cpp rs; do
				"$B/edits.sh" apply "$p" "$step" "$r" || exit 1
				timed "$p" "step$step-run$r-redo" "build_$p"
				"$B/edits.sh" revert "$p"
				timed "$p" "step$step-run$r-revert-redo" "build_$p"
			done
		done
	done
	;;
cold2)
	df -h /
	ninja -C "$CB" -t clean > /dev/null
	(cd "$RS" && cargo clean)
	df -h /
	timed cpp "cold-${2:-2}" build_cpp
	timed rs "cold-${2:-2}" build_rs
	;;
*)
	echo "unknown phase $phase" >&2
	exit 2
	;;
esac
echo "PHASE $phase DONE"
