#!/usr/bin/env bash
# RUSTC_WRAPPER: adds -Ztime-passes and logs each crate's wall time and incremental phase times.
# A wrapper is not part of cargo's fingerprint, so this does not rebuild anything by itself.
rustc=$1
shift
crate=
prev=
kind=lib
for a in "$@"; do
	[ "$prev" = --crate-name ] && crate=$a
	[ "$a" = --test ] && kind=test
	[ "$prev" = --crate-type ] && [ "$a" = bin ] && kind=bin
	prev=$a
done
log=${INCR_PROF_LOG:-/home/user/incr-prof/log}
if [ -z "$crate" ]; then
	exec "$rustc" "$@"
fi
tag="$crate:$kind"
start=$(date +%s.%N)
if [ -n "${INCR_PROF_ARGS:-}" ]; then
	printf '%s\t%s\t%s\n' "$start" "$tag" "$(printf '%q ' "$rustc" "$@")" >> "$INCR_PROF_ARGS"
	mkdir -p "$INCR_PROF_ARGS.d"
	{
		export -p | grep -vE '^declare -x (INCR_PROF_|RUSTC_WRAPPER=)'
		printf 'cd %q\n' "$PWD"
		printf 'exec %q' /home/user/incr-prof/wrap.sh
		printf ' %q' "$rustc" "$@"
		printf '\n'
	} > "$INCR_PROF_ARGS.d/$start-$tag.sh"
fi
"$rustc" "$@" -Ztime-passes 2> >(tee >(grep --line-buffered '^time:' | sed "s|^|$tag |" >> "$log") | grep -v '^time:' >&2)
rc=$?
end=$(date +%s.%N)
echo "$tag WALL $(echo "$end - $start" | bc) rc=$rc" >> "$log"
exit $rc
