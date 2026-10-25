#!/usr/bin/env bash
# Prints the benchmark inputs to GITHUB_OUTPUT: the dispatch inputs on a dispatch, else incr-bench/ci-request.env.
# Usage: ci-bench-inputs.sh EVENT BASELINE PATCHED GROK_REF
set -euo pipefail
event=$1
baseline=$2
patched=$3
grok_ref=$4
if [[ $event != workflow_dispatch ]]; then
	request=incr-bench/ci-request.env
	baseline='' patched='' grok_ref=''
	while IFS='=' read -r name value; do
		case $name in
			'' | '#'*) ;;
			baseline) baseline=$value ;;
			patched) patched=$value ;;
			grok_ref) grok_ref=$value ;;
			*)
				echo "ci-bench-inputs: $request names an unknown key: $name" >&2
				exit 1
				;;
		esac
	done < "$request"
	[[ -n $baseline ]] || { echo "ci-bench-inputs: $request sets no baseline" >&2; exit 1; }
	[[ -n $grok_ref ]] || { echo "ci-bench-inputs: $request sets no grok_ref" >&2; exit 1; }
fi
patched=${patched:-$GITHUB_SHA}
echo "baseline=$baseline patched=$patched grok_ref=$grok_ref"
{
	echo "baseline=$baseline"
	echo "patched=$patched"
	echo "grok_ref=$grok_ref"
} >> "$GITHUB_OUTPUT"
