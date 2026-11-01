#!/usr/bin/env bash
# Commits one results file to the branch the run is on, and pushes it. A push
# that loses a race fetches and tries again. Usage: ci-commit-results.sh FILE
# MESSAGE (from inside the checkout)
set -euo pipefail
file=$1
message=$2
branch=${GITHUB_REF_NAME:?}
git config user.name 'github-actions[bot]'
git config user.email '41898282+github-actions[bot]@users.noreply.github.com'
git add --sparse -- "$file"
git commit -q -m "$message"
until git push -q origin "HEAD:refs/heads/$branch"; do
	echo "ci-commit-results: the push to $branch was refused, fetching and trying again in 10s" >&2
	sleep 10
	git fetch -q origin "$branch"
	git rebase -q FETCH_HEAD
done
git log -1 --format='pushed %H to '"$branch"
