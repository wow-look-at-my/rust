#!/usr/bin/env bash
# Checks out one commit of this repository with its history and no blobs until
# checkout. Usage: ci-checkout-rustc.sh DIR SHA. GH_TOKEN authenticates the
# fetch.
set -euo pipefail
dir=$1
sha=$2
: "${GH_TOKEN:?GH_TOKEN must hold a token that can read ${GITHUB_REPOSITORY:?}}"
auth="AUTHORIZATION: basic $(printf 'x-access-token:%s' "$GH_TOKEN" | base64 -w0)"

git init -q "$dir"
cd "$dir"
git remote add origin "https://github.com/$GITHUB_REPOSITORY"
git config remote.origin.promisor true
git config remote.origin.partialclonefilter blob:none
git config http.https://github.com/.extraheader "$auth"

until git fetch --no-tags --filter=blob:none origin "$sha"; do
	echo "ci-checkout-rustc: fetch of $sha failed, trying again in 30 seconds" >&2
	sleep 30
done
until git checkout -q --detach "$sha"; do
	echo "ci-checkout-rustc: checkout of $sha failed while it fetched blobs, trying again in 30 seconds" >&2
	sleep 30
done
git config --unset-all http.https://github.com/.extraheader
git log -1 --format='ci-checkout-rustc: checked out %H'
