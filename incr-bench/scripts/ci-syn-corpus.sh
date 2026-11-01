#!/usr/bin/env bash
# DEST becomes a git repository, so a bench can revert its edits with git checkout. Usage: ci-syn-corpus.sh DEST
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
dest=$1
version=2.0.119
sha256=872831b642d1a07999a962a351ed35b955ea2cfc8f3862091e2a240a84f17297
mkdir -p "$dest"
cp -r "$here/../corpus/syn-ws/." "$dest/"
crate=$(mktemp -d)/syn.crate
curl -fsSL -o "$crate" "https://static.crates.io/crates/syn/syn-$version.crate"
echo "$sha256  $crate" | sha256sum -c -
tar -xzf "$crate" -C "$dest/syn" --strip-components=1 \
	"syn-$version/src" "syn-$version/LICENSE-APACHE" "syn-$version/LICENSE-MIT" "syn-$version/README.md"
git -C "$dest" init -q
git -C "$dest" add -A
git -C "$dest" -c user.name=incr-bench -c user.email=incr-bench@localhost commit -q -m "syn $version corpus"
echo "syn corpus at $dest"
