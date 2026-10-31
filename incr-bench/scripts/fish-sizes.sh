#!/bin/bash
# Print the size breakdown of a build tree. Usage: sizes.sh <cpp|rs>
# Apparent sizes come from find -printf '%s'. On-disk sizes come from du.
set -uo pipefail
B=/home/user/lang-bench

# sum <find args...>: apparent bytes of the matched regular files.
sum() { find "$@" -type f -printf '%s\n' 2>/dev/null | awk '{s+=$1} END {printf "%d", s}'; }
cnt() { find "$@" -type f 2>/dev/null | wc -l; }
mib() { awk -v b="$1" 'BEGIN {printf "%.1f MiB", b/1048576}'; }
row() { printf '%-55s %14s  %12s  files=%s\n' "$1" "$2" "$(mib "$2")" "$3"; }

if [ "$1" = cpp ]; then
	D=$B/fish-cpp/build
	tot=$(sum "$D"); row "build/ total (apparent)" "$tot" "$(cnt "$D")"
	echo "build/ total (du -s, KiB on disk): $(du -s "$D" | cut -f1)"
	o=$(sum "$D" -name '*.o'); row "  object files *.o" "$o" "$(cnt "$D" -name '*.o')"
	a=$(sum "$D" -name '*.a'); row "  archives *.a" "$a" "$(cnt "$D" -name '*.a')"
	x=$(sum "$D" -maxdepth 1 -perm -u+x); row "  executables (top level)" "$x" "$(cnt "$D" -maxdepth 1 -perm -u+x)"
	echo "  objects+archives+executables: $(mib $((o + a + x)))"
	bn=$(sum "$D" -maxdepth 1 \( -name build.ninja -o -name .ninja_deps -o -name .ninja_log -o -name CMakeCache.txt -o -name '*.cmake' \))
	row "  build.ninja+.ninja_deps+.ninja_log+CMakeCache+*.cmake" "$bn" "-"
	for f in build.ninja .ninja_deps .ninja_log CMakeCache.txt; do
		row "    $f" "$(sum "$D" -maxdepth 1 -name "$f")" 1
	done
	cf=$(sum "$D/CMakeFiles" ! -name '*.o' ! -name '*.d'); row "  CMakeFiles/ non-object, non-.d" "$cf" "$(cnt "$D/CMakeFiles" ! -name '*.o' ! -name '*.d')"
	dd=$(sum "$D" -name '*.d'); row "  *.d depfiles" "$dd" "$(cnt "$D" -name '*.d')"
	meta=$((bn + cf + dd)); echo "  metadata total: $(mib $meta)"
	rest=$((tot - o - a - x - meta)); row "  everything else" "$rest" "-"
	echo "  everything else, largest:"
	find "$D" -type f ! -name '*.o' ! -name '*.a' ! -name '*.d' ! -path '*/CMakeFiles/*' -printf '%s\t%P\n' | sort -rn | head -8 | sed 's/^/    /'
else
	T=$B/fish-rs/target
	tot=$(sum "$T"); row "target/ total (apparent; hardlinks counted twice)" "$tot" "$(cnt "$T")"
	echo "target/ total (du -s, KiB on disk, hardlinks once): $(du -s "$T" | cut -f1)"
	I=$T/debug/incremental
	it=$(sum "$I"); row "  debug/incremental total" "$it" "$(cnt "$I")"
	echo "    du -s KiB: $(du -s "$I" | cut -f1)"
	dg=$(sum "$I" -name dep-graph.bin); row "    dep-graph.bin" "$dg" "$(cnt "$I" -name dep-graph.bin)"
	qc=$(sum "$I" -name query-cache.bin); row "    query-cache.bin" "$qc" "$(cnt "$I" -name query-cache.bin)"
	io=$(sum "$I" -name '*.o'); row "    *.o (codegen objects)" "$io" "$(cnt "$I" -name '*.o')"
	row "    other" "$((it - dg - qc - io))" "-"
	echo "    crate incremental dirs: $(find "$I" -mindepth 1 -maxdepth 1 -type d | wc -l)"
	echo "    session dirs (s-*): $(find "$I" -mindepth 2 -maxdepth 2 -type d -name 's-*' | wc -l)"
	echo "    per crate dir (apparent bytes, sessions):"
	for c in "$I"/*/; do
		printf '      %-40s %12s  sessions=%s\n' "$(basename "$c")" "$(sum "$c")" "$(find "$c" -mindepth 1 -maxdepth 1 -type d -name 's-*' | wc -l)"
	done
	P=$T/debug/deps
	dt=$(sum "$P"); row "  debug/deps total" "$dt" "$(cnt "$P")"
	rl=$(sum "$P" -name '*.rlib'); row "    *.rlib" "$rl" "$(cnt "$P" -name '*.rlib')"
	rm_=$(sum "$P" -name '*.rmeta'); row "    *.rmeta" "$rm_" "$(cnt "$P" -name '*.rmeta')"
	bi=$(sum "$P" -perm -u+x ! -name '*.so'); row "    executables" "$bi" "$(cnt "$P" -perm -u+x ! -name '*.so')"
	so=$(sum "$P" -name '*.so'); row "    *.so (proc-macro)" "$so" "$(cnt "$P" -name '*.so')"
	dd=$(sum "$P" -name '*.d'); row "    *.d" "$dd" "$(cnt "$P" -name '*.d')"
	row "    other" "$((dt - rl - rm_ - bi - so - dd))" "-"
	bd=$(sum "$T/debug/build"); row "  debug/build (build scripts + OUT_DIRs)" "$bd" "$(cnt "$T/debug/build")"
	fp=$(sum "$T/debug/.fingerprint"); row "  debug/.fingerprint" "$fp" "$(cnt "$T/debug/.fingerprint")"
	tl=$(sum "$T/debug" -maxdepth 1); row "  debug/ top level (hardlinks of deps/ bins)" "$tl" "$(cnt "$T/debug" -maxdepth 1)"
	row "  everything else" "$((tot - it - dt - bd - fp - tl))" "-"
	echo "  everything else, largest:"
	find "$T" -type f ! -path '*/incremental/*' ! -path '*/debug/deps/*' ! -path '*/debug/build/*' ! -path '*/.fingerprint/*' -printf '%s\t%P\n' | sort -rn | head -8 | sed 's/^/    /'
fi
