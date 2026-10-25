#!/usr/bin/env bash
# Builds abseil-cpp with its tests, then splits the build dir into compiled
# output and build-system metadata.
set -euo pipefail
src=/home/user/abseil-cpp
build=$src/build
cd "$build"

timed() {
	local start end
	start=$(date +%s.%N)
	nice -n 19 ninja -j2 > /dev/null
	end=$(date +%s.%N)
	echo "$end - $start" | bc
}

sizes() {
	local label=$1
	# Compiled output: object files, archives, and executables.
	local objs bins total meta
	objs=$(find "$build" -type f \( -name '*.o' -o -name '*.a' \) -printf '%s\n' | awk '{s+=$1} END{print s+0}')
	bins=$(find "$build" -type f -perm -u+x ! -name '*.sh' ! -name '*.o' ! -name '*.a' -printf '%s\n' | awk '{s+=$1} END{print s+0}')
	total=$(find "$build" -type f -printf '%s\n' | awk '{s+=$1} END{print s+0}')
	meta=$((total - objs - bins))
	printf '%-14s total=%.3fGB objs+archives=%.3fGB executables=%.3fGB build-system=%.1fMB (%.2f%% of total)\n' \
		"$label" "$(echo "$total/1e9" | bc -l)" "$(echo "$objs/1e9" | bc -l)" "$(echo "$bins/1e9" | bc -l)" \
		"$(echo "$meta/1e6" | bc -l)" "$(echo "100*$meta/$total" | bc -l)"
	echo "  largest build-system files:"
	find "$build" -type f ! -name '*.o' ! -name '*.a' ! -perm -u+x -printf '%s %P\n' | sort -nr | head -5 |
		awk '{printf "    %8.2f MB  %s\n", $1/1e6, $2}'
	echo "  by kind:"
	find "$build" -type f ! -name '*.o' ! -name '*.a' ! -perm -u+x -printf '%s %f\n' |
		awk '{n=$2; if (n ~ /\.d$/) k=".d depfiles"; else if (n==".ninja_deps") k=".ninja_deps"; else if (n==".ninja_log") k=".ninja_log"; else if (n ~ /\.ninja$/) k="*.ninja"; else if (n ~ /\.cmake$/) k="*.cmake"; else k="other"; s[k]+=$1; c[k]++} END{for (k in s) printf "    %8.2f MB  %6d files  %s\n", s[k]/1e6, c[k], k}' | sort -nr
}

echo "cold build: $(timed)s"
sizes cold

leaf=$src/absl/strings/str_cat.cc
header=$src/absl/base/config.h
cp "$leaf" /tmp/absl-leaf.bak
cp "$header" /tmp/absl-header.bak
trap 'cp /tmp/absl-leaf.bak "$leaf"; cp /tmp/absl-header.bak "$header"' EXIT

printf '\n// bench edit\n' >> "$leaf"
echo "edit one .cc (str_cat.cc): $(timed)s"
printf '\n// bench edit\n' >> "$header"
echo "edit a core header (base/config.h): $(timed)s"
sizes after-edits
