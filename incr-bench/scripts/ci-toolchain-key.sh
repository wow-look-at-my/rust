#!/usr/bin/env bash
# Resolves a compiler commit of this repository and prints "SHA KEY", KEY being the cache key of its stage1 toolchain.
# Usage: ci-toolchain-key.sh COMMIT GIT_DIR
# The key hashes every top-level tree except incr-bench, so a commit that only adds results reuses the toolchain.
set -euo pipefail
commit=$1
dir=$2
[[ -d $dir/.git ]] || git init -q "$dir"
git -C "$dir" fetch -q --depth 1 --filter=blob:none "https://github.com/${GITHUB_REPOSITORY}.git" "$commit"
sha=$(git -C "$dir" rev-parse FETCH_HEAD)
tree=$(git -C "$dir" ls-tree FETCH_HEAD | awk -F'\t' '$2 != "incr-bench"' | sha256sum | cut -c1-40)
echo "commit $commit resolves to $sha, compiler tree $tree" >&2
echo "$sha rust-stage1-x86_64-linux-$tree"
