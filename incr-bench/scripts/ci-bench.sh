#!/usr/bin/env bash
# Steps: cold, edit:comment, edit:body, edit:new-item, edit:signature (the edits add up),
# revert. Usage: ci-bench.sh WORKLOAD SRC RUSTC TARGET_ROOT RESULTS WORKLOAD
# is grok, syn or rustc. SRC is a git checkout of it.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
workload=$1
src=$(realpath "$2")
rustc_bin=$(realpath "$3")
target_root=$(realpath -m "$4")
results=$(realpath -m "$5")
config=${BENCH_CONFIG:?}
compiler=${BENCH_COMPILER:?}
toolchain=${BENCH_TOOLCHAIN:?}
flags=${BENCH_RUSTC_FLAGS:-}

case $workload in
grok)
	cmd=(cargo test -q --locked -p xai-grok-pager --no-run)
	item_file=crates/codegen/xai-grok-shell/src/lib.rs
	body_file=crates/codegen/xai-grok-shell/src/tier.rs
	body_from='    let t = tier.trim().to_ascii_lowercase();'
	body_to='    let t = tier.trim().to_lowercase();'
	comment_after=''
	profile_crates='xai_grok_shell:lib xai_grok_pager:lib xai_grok_pager:test'
	;;
syn)
	cmd=(cargo build -q --locked --workspace)
	item_file=syn/src/lib.rs
	body_file=syn/src/lib.rs
	body_from='    file.shebang = shebang;'
	body_to='    file.shebang = shebang.filter(|s| !s.is_empty());'
	comment_after='    // Strip the BOM if it is present'
	profile_crates='syn:lib'
	;;
rustc)
	# The compiler builds itself. RUSTC is the stage0 compiler under build.local-rebuild.
	cmd=(env -u GITHUB_ACTIONS ./x build --stage 1 compiler)
	item_file=compiler/rustc_passes/src/lib.rs
	body_file=compiler/rustc_passes/src/lib.rs
	body_from='    canonical_symbols::provide(providers);'
	body_to='    canonical_symbols::provide(providers); let _ = 0u8;'
	comment_after=''
	profile_crates=''
	;;
*)
	echo "ci-bench: unknown workload $workload" >&2
	exit 2
	;;
esac

if [[ $workload == rustc ]]; then
	# Bootstrap runs its own rustc shim and passes RUSTFLAGS_NOT_BOOTSTRAP to the compiles of a local rebuild.
	export RUSTFLAGS_NOT_BOOTSTRAP="$flags"
	# The stage1 toolchain has no cargo, and bootstrap looks for one next to build.rustc. Use the cargo that src/stage0 pins.
	host=$("$rustc_bin" -vV | sed -n 's/^host: //p')
	date=$(sed -n 's/^compiler_date=//p' "$src/src/stage0")
	tarball=cargo-beta-$host.tar.xz
	sum=$(sed -n "s|^dist/$date/$tarball=||p" "$src/src/stage0")
	[[ -n $sum ]] || { echo "ci-bench: src/stage0 pins no $tarball" >&2; exit 1; }
	cargo_dir=$target_root/stage0-cargo
	mkdir -p "$cargo_dir"
	curl -fsSL --retry 5 -o "$cargo_dir/$tarball" "https://static.rust-lang.org/dist/$date/$tarball"
	echo "$sum  $cargo_dir/$tarball" | sha256sum -c -
	tar -xJf "$cargo_dir/$tarball" -C "$cargo_dir" --strip-components=1
	printf '%s\n' 'profile = "compiler"' 'change-id = "ignore"' '[llvm]' 'download-ci-llvm = true' \
		'[rust]' 'debuginfo-level = 0' 'debug-assertions = false' '[build]' \
		"rustc = \"$rustc_bin\"" "cargo = \"$cargo_dir/cargo/bin/cargo\"" 'local-rebuild = true' \
		"jobs = ${CARGO_BUILD_JOBS:-$(nproc)}" > "$src/bootstrap.toml"
else
	export RUSTC="$rustc_bin" RUSTC_WRAPPER="$here/ci-rustc-flags.sh" RUSTC_BOOTSTRAP=1 BENCH_RUSTC_FLAGS="$flags"
fi
cd "$src"
git diff --quiet || { echo "ci-bench: $src has uncommitted changes" >&2; exit 1; }
trap 'git -C "$src" checkout -q -- .' EXIT
mkdir -p "$target_root" "$(dirname "$results")"
printf 'workload\tconfig\tcompiler\ttoolchain\tflags\tmode\tstep\twall_s\tdep_graph_bytes\tquery_cache_bytes\tobj_bytes\tother_bytes\ttotal_bytes\tcrate_dirs\tsessions\n' > "$results"

sum_sizes() {
	find "$@" -type f -printf '%s\n' | awk '{s+=$1} END{printf "%d", s}'
}

# Appends one row for the incremental dirs under target dir $1. A cargo target dir has one. A bootstrap build dir has one per stage and mode.
measure() {
	local mode=$2 step=$3 secs=$4 inc
	local graph=0 qc=0 objs=0 total=0 linked=0 crate_dirs=0 sessions=0
	mapfile -t inc < <(find "$1" -type d -name incremental -prune)
	if [[ ${#inc[@]} -gt 0 ]]; then
		graph=$(sum_sizes "${inc[@]}" -name dep-graph.bin)
		qc=$(sum_sizes "${inc[@]}" -name query-cache.bin)
		objs=$(sum_sizes "${inc[@]}" -name '*.o')
		# A file with another link, like the metadata.rmeta of a session, shares its blocks with the target dir. It costs no disk here.
		linked=$(sum_sizes "${inc[@]}" -links +1 ! -name dep-graph.bin ! -name query-cache.bin ! -name "*.o")
		total=$(( $(sum_sizes "${inc[@]}") - linked ))
		crate_dirs=$(find "${inc[@]}" -mindepth 1 -maxdepth 1 -type d | wc -l)
		sessions=$(find "${inc[@]}" -mindepth 2 -maxdepth 2 -type d -name 's-*' | wc -l)
	fi
	printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%.1f\t%d\t%d\t%d\t%d\t%d\t%d\t%d\n' \
		"$workload" "$config" "$compiler" "$toolchain" "$flags" "$mode" "$step" "$secs" \
		"$graph" "$qc" "$objs" "$((total - graph - qc - objs))" "$total" "$crate_dirs" "$sessions" >> "$results"
	printf '%-5s %-22s %-7s %-14s %8.1fs graph=%d qcache=%d objs=%d total=%d linked=%d crate_dirs=%d sessions=%d\n' \
		"$workload" "$config" "$mode" "$step" "$secs" "$graph" "$qc" "$objs" "$total" "$linked" "$crate_dirs" "$sessions"
}

build() {
	local tdir=$1 mode=$2 step=$3 incr=1 start end log reuse graph extra=()
	[[ $mode == noincr ]] && incr=0
	if [[ $workload == rustc ]]; then
		extra=(--build-dir "$tdir" --set "rust.incremental=$([[ $incr == 1 ]] && echo true || echo false)")
	fi
	log=$target_root/$mode.${step//:/-}.log
	start=$(date +%s.%N)
	rm -f "$log.crates"
	if ! BENCH_CRATE_TIMES="$log.crates" CARGO_TARGET_DIR="$tdir" CARGO_INCREMENTAL=$incr "${cmd[@]}" "${extra[@]}" > "$log" 2>&1; then
		tail -n 80 "$log" >&2
		echo "ci-bench: the $mode $step build failed, full log in $log" >&2
		exit 1
	fi
	end=$(date +%s.%N)
	# Only an -Zincremental-info run prints these counts: bodies decoded from the .rmeta, bodies the encoder asked for, and bodies the dep graph marked green.
	reuse=$({ grep -oE 'decoded [0-9]+ of [0-9]+ MIR bodies from the previous session \([0-9]+ marked green' "$log" || true; } | tr -d '(' | awk '{d += $2; r += $4; g += $11} END {printf "%d\t%d\t%d", d, r, g}')
	if [[ $reuse != $'0\t0\t0' ]]; then
		printf '%s\t%s\t%s\t%s\n' "$config" "$mode" "$step" "$reuse" >> "${results%.tsv}.reuse.tsv"
		echo "rmeta reuse $mode $step: $reuse (decoded, requested, green)"
	fi
	# An -Zincremental-info run prints the MIR bodies codegen took from the metadata: decoded, rejected by the decoder.
	local codegen
	codegen=$({ grep -oE 'codegen: decoded [0-9]+ MIR bodies from metadata, [0-9]+ rejected, [0-9]+ mismatched' "$log" || true; } | tr -d ',' | awk '{d += $3; r += $8; m += $10} END {printf "%d\t%d\t%d", d, r, m}')
	if [[ $codegen != $'0\t0\t0' ]]; then
		echo "codegen MIR from metadata $mode $step: $codegen (decoded, rejected, mismatched)"
		{ grep -E '^\[incremental\] codegen: (mismatch of|field |spans |first span )' "$log" || true; } | sed -n 1,30p | cut -c1-600
	fi
	# An -Zincremental-info run also prints the bytes each crate's dep graph spends on node heads, keys, values and edges.
	graph=$({ grep -oE 'dep graph node bytes: total [0-9]+ heads [0-9]+ keys [0-9]+ values [0-9]+ edges [0-9]+' "$log" || true; } | awk '{t += $6; h += $8; k += $10; v += $12; e += $14} END {printf "%d\t%d\t%d\t%d\t%d", t, h, k, v, e}')
	if [[ $graph != $'0\t0\t0\t0\t0' ]]; then
		printf '%s\t%s\t%s\t%s\n' "$config" "$mode" "$step" "$graph" >> "${results%.tsv}.graph.tsv"
		echo "dep graph bytes $mode $step: $graph (total, heads, keys, values, edges)"
	fi
	# An -Zincremental-info run prints what happened to each node, summed here over the crates this step compiled.
	local seen=$target_root/$mode.fates-seen
	touch "$seen"
	fates=$({ grep -oE 'node fates: previous [0-9]+ promoted [0-9]+ rerun-green [0-9]+ rerun-red [0-9]+ unreached [0-9]+ new [0-9]+ pid [0-9]+' "$log" || true; } | sort -u | tee -a "$seen.new" | { grep -vxF -f "$seen" || true; } | awk '{p += $4; pr += $6; g += $8; r += $10; u += $12; n += $14} END {printf "%d\t%d\t%d\t%d\t%d\t%d", p, pr, g, r, u, n}')
	sort -u -o "$seen" "$seen" "$seen.new"
	rm -f "$seen.new"
	if [[ $fates != $'0\t0\t0\t0\t0\t0' ]]; then
		fates_file=${results%.tsv}.fates.tsv
		[[ -s $fates_file ]] || printf 'fates\tconfig\tstep\tprevious\tpromoted\trerun_green\trerun_red\tunreached\tnew\n' > "$fates_file"
		printf '%s\t%s\t%s\t%s\n' "$workload" "$config" "$step" "$fates" >> "$fates_file"
		echo "node fates $mode $step: $fates (previous, promoted, rerun-green, rerun-red, unreached, new)"
	fi
	# An -Zincremental-explain-red run names the red input behind each node of the listed kinds.
	local red_seen=$target_root/$mode.red-seen
	touch "$red_seen"
	{ grep -E '^\[incremental\] red: ' "$log" || true; } | sort -u > "$red_seen.new"
	{ grep -vxF -f "$red_seen" "$red_seen.new" || true; } > "$log.red"
	sort -u -o "$red_seen" "$red_seen" "$red_seen.new"
	rm -f "$red_seen.new"
	if [[ -s $log.red ]]; then
		echo "red nodes $mode $step: $(wc -l < "$log.red") chains, the first 40:"
		sed -n 1,40p "$log.red" | cut -c1-700
	fi
	if [[ $step == cold && $graph != $'0\t0\t0\t0\t0' ]]; then
		# The per-kind table rows end in the bytes column. Sum each kind over all crates.
		awk -F'|' 'NF == 6 && $5 ~ /^ *[0-9]+ *$/ {
			kind = $1; sub(/^\[incremental\] +/, "", kind); sub(/ +$/, "", kind)
			n[kind] += $3; b[kind] += $5
		} END { for (k in b) printf "%s\t%d\t%d\n", k, n[k], b[k] }' "$log" | sort -t$'\t' -k3,3nr > "${results%.tsv}.kinds.tsv"
		echo "top dep kinds by graph bytes (kind, nodes, bytes):"
		head -n 25 "${results%.tsv}.kinds.tsv"
	fi
	if [[ -s $log.crates ]]; then
		echo "slowest crate compiles $mode $step (crate:kind, seconds):"
		# sort writes to a file: a `sort | head` pipe fails under pipefail.
		sort -t$'\t' -k2,2nr -o "$log.crates" "$log.crates"
		sed -n 1,8p "$log.crates"
		sed -n "1,20s|^|$config\t$mode\t$step\t|p" "$log.crates" >> "${results%.tsv}.crates.tsv"
	fi
	measure "$tdir" "$mode" "$step" "$(echo "$end - $start" | bc)"
}

# Each edit must change the tree, or the step would measure a no-op.
edit() {
	local before
	before=$(git diff | sha256sum)
	case $1 in
	comment)
		if [[ -n $comment_after ]]; then
			sed -i "s|^${comment_after}\$|&\n    // incr-bench comment edit|" "$item_file"
		else
			printf '\n// incr-bench comment edit\n' >> "$item_file"
		fi
		;;
	body)
		sed -i "s|^$(printf '%s' "$body_from" | sed 's/[][\.*^$|]/\\&/g')\$|$(printf '%s' "$body_to" | sed 's/[\&|]/\\&/g')|" "$body_file"
		;;
	new-item)
		printf '\n#[allow(dead_code)]\nfn incr_bench_extra_item() -> u32 {\n    7\n}\n' >> "$item_file"
		;;
	signature)
		# The item from the new-item step becomes part of the public API, with a new return type.
		sed -i 's|^fn incr_bench_extra_item() -> u32 {$|pub fn incr_bench_extra_item() -> u64 {|' "$item_file"
		;;
	esac
	if [[ $(git diff | sha256sum) == "$before" ]]; then
		echo "ci-bench: the $1 edit changed nothing in $src" >&2
		exit 1
	fi
}

# One more edit of a kind, outside the measured steps, that self-profiles the slowest crates.
profile_edit() {
	local tdir=$1 kind=$2 dir=$target_root/profile prof
	rm -rf "$dir"
	edit "$kind"
	if ! BENCH_PROFILE_DIR="$dir" BENCH_PROFILE_CRATES="$profile_crates" CARGO_TARGET_DIR="$tdir" CARGO_INCREMENTAL=1 "${cmd[@]}" > "$target_root/profile.log" 2>&1; then
		tail -n 80 "$target_root/profile.log" >&2
		echo "ci-bench: the profiled $kind edit failed" >&2
		exit 1
	fi
	git checkout -q -- .
	for prof in "$dir"/*/*.mm_profdata; do
		[[ -e $prof ]] || { echo "ci-bench: the profiled edit wrote no profile for $profile_crates" >&2; exit 1; }
		echo "self-profile of the $kind edit, $(basename "$(dirname "$prof")"):"
		summarize summarize "$prof" > "$dir/summary.txt"
		sed -n 1,40p "$dir/summary.txt" | cut -c1-200
	done
	rm -rf "$dir"
}

run_mode() {
	local mode=$1 tdir=$target_root/$1
	rm -rf "$tdir"
	build "$tdir" "$mode" cold
	for e in comment body new-item signature; do
		edit "$e"
		build "$tdir" "$mode" "edit:$e"
	done
	git checkout -q -- .
	build "$tdir" "$mode" revert
	if [[ $mode == incr && -n $profile_crates ]] && command -v summarize > /dev/null; then
		profile_edit "$tdir" body
		profile_edit "$tdir" comment
		# The signature edit changes the item that the new-item edit adds. Build that item unprofiled first.
		edit new-item
		if ! CARGO_TARGET_DIR="$tdir" CARGO_INCREMENTAL=1 "${cmd[@]}" > "$target_root/profile.log" 2>&1; then
			tail -n 80 "$target_root/profile.log" >&2
			echo "ci-bench: the build before the profiled signature edit failed" >&2
			exit 1
		fi
		profile_edit "$tdir" signature
	fi
	# A changed profile or feature set is a new build configuration, and cargo
	# gives it new crate dirs.
	if [[ $workload != rustc ]]; then
		CARGO_PROFILE_DEV_DEBUG_ASSERTIONS=false build "$tdir" "$mode" config-change
	fi
	rm -rf "$tdir"
}

"$rustc_bin" -vV
run_mode incr
run_mode noincr
