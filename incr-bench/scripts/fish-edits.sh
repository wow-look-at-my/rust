#!/bin/bash
# Apply or revert one benchmark edit.
set -euo pipefail
B=/home/user/lang-bench
CPP=$B/fish-cpp
RS=$B/fish-rs

act=$1
proj=$2

if [ "$act" = revert ]; then
	if [ "$proj" = cpp ]; then
		git -C "$CPP" checkout -- src/builtins/random.cpp src/common.h src/common.cpp
	else
		git -C "$RS" checkout -- src/builtins/random.rs src/common.rs
	fi
	exit 0
fi

step=$3
r=$4

if [ "$proj" = cpp ]; then
	case $step in
	3) printf '// bench leaf comment run %s\n' "$r" >> "$CPP/src/builtins/random.cpp" ;;
	4) sed -i "s|^    int argc = builtin_count_args(argv);\$|&\n    int bench_extra_$r = argc + $r; if (bench_extra_$r == 0) return STATUS_INVALID_ARGS;|" "$CPP/src/builtins/random.cpp" ;;
	5) printf '// bench core comment run %s\n' "$r" >> "$CPP/src/common.h" ;;
	6) sed -i "s|^#endif  // FISH_COMMON_H\$|int bench_new_fn_$r(int x);\n&|" "$CPP/src/common.h"
	   printf 'int bench_new_fn_%s(int x) { return x + %s; }\n' "$r" "$r" >> "$CPP/src/common.cpp" ;;
	*) echo "bad step $step" >&2; exit 2 ;;
	esac
	git -C "$CPP" diff --stat >&2
else
	case $step in
	3) printf '// bench leaf comment run %s\n' "$r" >> "$RS/src/builtins/random.rs" ;;
	4) sed -i "s|^    let argc = argv.len();\$|&\n    let bench_extra_$r = argc + $r; if bench_extra_$r == 0 { return Err(STATUS_INVALID_ARGS); }|" "$RS/src/builtins/random.rs" ;;
	5) printf '// bench core comment run %s\n' "$r" >> "$RS/src/common.rs" ;;
	6) printf 'pub fn bench_new_fn_%s(x: i32) -> i32 { x + %s }\n' "$r" "$r" >> "$RS/src/common.rs" ;;
	*) echo "bad step $step" >&2; exit 2 ;;
	esac
	git -C "$RS" diff --stat >&2
fi
# A sed that matched nothing leaves the tree clean: fail loud.
if [ "$proj" = cpp ]; then d=$CPP; else d=$RS; fi
if git -C "$d" diff --quiet; then
	echo "edit $proj step $step run $r changed nothing" >&2
	exit 3
fi
