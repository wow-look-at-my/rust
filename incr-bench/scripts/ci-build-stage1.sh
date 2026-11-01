#!/usr/bin/env bash
# Builds the stage1 compiler and std of a rust checkout and copies the sysroot out of the build dir.
# Usage: ci-build-stage1.sh SRC OUT
set -euo pipefail
src=$(realpath "$1")
out=$(realpath -m "$2")
cd "$src"
cat > bootstrap.toml <<EOF
profile = "compiler"
change-id = "ignore"
[llvm]
download-ci-llvm = true
[rust]
debuginfo-level = 0
debug-assertions = false
incremental = false
[build]
jobs = $(nproc)
EOF
# Under GITHUB_ACTIONS, bootstrap expects a merge commit at the checkout tip. This checkout has none.
env -u GITHUB_ACTIONS ./x build --stage 1 compiler library
sysroot=build/host/stage1
mkdir -p "$out"
tar -C "$sysroot" --exclude=./lib/rustlib/src --exclude=./lib/rustlib/rustc-src -cf - . | tar -C "$out" -xf -
if ldd "$out/bin/rustc" | grep 'not found'; then
	echo "ci-build-stage1: $out/bin/rustc has unresolved libraries" >&2
	exit 1
fi
"$out/bin/rustc" -vV
du -sh "$out"
