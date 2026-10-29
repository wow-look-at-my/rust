#!/usr/bin/env bash
# Reads the benchmark request and prints the job matrices to GITHUB_OUTPUT. On
# a dispatch the request is the inputs. On a push it is
# incr-bench/ci-request.env.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
event=$1
baseline=$2
patched=$3
grok_ref=$4
workloads=$5
configs=()
IFS=';' read -ra configs <<< "$6"

die() {
	echo "ci-bench-inputs: $*" >&2
	exit 1
}

if [[ $event != workflow_dispatch ]]; then
	request=incr-bench/ci-request.env
	baseline='' patched='' grok_ref='' workloads='' configs=()
	# read fails on a last line with no newline. The -n test keeps that line.
	while IFS='=' read -r name value || [[ -n $name ]]; do
		case $name in
		'' | '#'*) ;;
		baseline) baseline=$value ;;
		patched) patched=$value ;;
		grok_ref) grok_ref=$value ;;
		workloads) workloads=$value ;;
		config) configs+=("$value") ;;
		*) die "$request names an unknown key: $name" ;;
		esac
	done < "$request"
fi
[[ -n $baseline ]] || die "no baseline commit"
[[ -n $grok_ref ]] || die "no grok_ref"
patched=${patched:-$GITHUB_SHA}
workloads=${workloads:-grok syn}
[[ ${#configs[@]} -gt 0 ]] || configs=("baseline baseline" "patched patched")

declare -A sha key used seen
for c in baseline patched; do
	commit=$baseline
	[[ $c == patched ]] && commit=$patched
	read -r "sha[$c]" "key[$c]" < <("$here/ci-toolchain-key.sh" "$commit" "${RUNNER_TEMP:-/tmp}/keyrepo")
done

legs='[]'
for entry in "${configs[@]}"; do
	cname='' compiler='' flags=''
	read -r cname compiler flags <<< "$entry"
	[[ -n $cname ]] || continue
	[[ $cname =~ ^[A-Za-z0-9._-]+$ ]] || die "config name '$cname' is not [A-Za-z0-9._-]+"
	[[ -z ${seen[$cname]:-} ]] || die "config '$cname' appears twice"
	seen[$cname]=1
	[[ $compiler == baseline || $compiler == patched ]] || die "config '$cname' names compiler '$compiler', not baseline or patched"
	[[ $flags =~ ^[-A-Za-z0-9=,_.:+\ ]*$ ]] || die "config '$cname' has flags outside [-A-Za-z0-9=,_.:+ ]: $flags"
	used[$compiler]=1
	for w in $workloads; do
		[[ $w == grok || $w == syn || $w == rustc ]] || die "unknown workload '$w'"
		legs=$(jq -c --arg leg "$w-$cname" --arg w "$w" --arg c "$cname" --arg comp "$compiler" \
			--arg sha "${sha[$compiler]}" --arg key "${key[$compiler]}" --arg f "$flags" \
			'. + [{leg: $leg, workload: $w, config: $c, compiler: $comp, sha: $sha, key: $key, flags: $f}]' <<< "$legs")
	done
done
[[ $legs != '[]' ]] || die "the request names no config"
compilers='[]'
for c in baseline patched; do
	[[ -n ${used[$c]:-} ]] || continue
	compilers=$(jq -c --arg c "$c" --arg sha "${sha[$c]}" --arg key "${key[$c]}" '. + [{compiler: $c, sha: $sha, key: $key}]' <<< "$compilers")
done
jq . <<< "$legs" >&2
{
	echo "grok_ref=$grok_ref"
	# The rustc workload compiles the baseline source with each config's compiler.
	echo "rustc_src=${sha[baseline]}"
	echo "compilers=$compilers"
	echo "legs=$legs"
	echo "names=$(jq -c 'map(.leg)' <<< "$legs")"
} >> "$GITHUB_OUTPUT"
