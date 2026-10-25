#!/usr/bin/env bash
# Measures the incremental cache size and the rebuild time of one rustc on the corpus workspace. Usage.
set -euo pipefail

if [[ $# -ne 3 ]]; then
	echo "usage: $0 RUSTC LABEL OUT_CSV" >&2
	exit 2
fi
RUSTC_BIN=$(readlink -f "$1")
LABEL=$2
OUT_CSV=$3
[[ -x $RUSTC_BIN ]] || { echo "error: RUSTC '$1' is not executable" >&2; exit 2; }
[[ $LABEL =~ ^[A-Za-z0-9._-]+$ ]] || { echo "error: LABEL must match [A-Za-z0-9._-]+" >&2; exit 2; }

HERE=$(cd "$(dirname "$0")" && pwd)
STAGE0_CARGO=/home/user/rust/build/x86_64-unknown-linux-gnu/stage0/bin/cargo
if [[ -z ${CARGO:-} ]]; then
	if [[ -x $STAGE0_CARGO ]]; then CARGO=$STAGE0_CARGO; else CARGO=$(command -v cargo); fi
fi
CORPUS=${CORPUS:-/home/user/bench-corpus/ws}
TARGET_ROOT=${TARGET_ROOT:-/home/user/bench-corpus/targets}
EXTRA_RUSTFLAGS=${EXTRA_RUSTFLAGS:-}
REPS=${REPS:-2}
SCENARIOS=${SCENARIOS:-"edits churn-cmd churn-feat"}
EDITS=${EDITS:-"comment body sig"}
CMD_ROUNDS=${CMD_ROUNDS:-3}
FEAT_ROUNDS=${FEAT_ROUNDS:-1}
NICE=${NICE:-19}
JOBS_ARGS=()
[[ -n ${JOBS:-} ]] && JOBS_ARGS=(-j "$JOBS")

LIB_CRATE=syn
BIN_CRATE=app
LIB_SRC=$CORPUS/syn/src/lib.rs
FEATURE_SETS=(
	"default"
	"full"
	"full,extra-traits"
	"full,visit"
	"full,visit,visit-mut,fold"
	"full,extra-traits,visit,visit-mut,fold"
)

LABEL_DIR=$TARGET_ROOT/$LABEL
LOG_DIR=$LABEL_DIR/logs
# A private CARGO_HOME. The shared ~/.cargo package-cache lock is held by other
# cargo processes, and a wait on it is counted in the wall time.
CARGO_HOME_DIR=$LABEL_DIR/cargo-home
RUSTC_RELEASE=$("$RUSTC_BIN" -vV | sed -n 's/^release: //p')

die() {
	echo "error: $*" >&2
	exit 1
}

revert_corpus() {
	git -C "$CORPUS" checkout -q -- syn app
}

git -C "$CORPUS" diff --quiet -- syn app ||
	die "corpus $CORPUS has uncommitted changes; run: git -C $CORPUS checkout -- syn app"
trap revert_corpus EXIT

rm -rf "$LABEL_DIR"
mkdir -p "$LOG_DIR"

CSV_HEADER="label,rep,scenario,step,cmd,wall_s,incr_bytes,incr_kb_alloc,crate_dirs,session_dirs,depgraph_files,depgraph_bytes,querycache_bytes,new_sessions,wp_objs,wp_reused,lib_nodes,lib_edges,bin_nodes,bin_edges,incr_invocations,ignored_artifacts,rustc_release,extra_rustflags"
if [[ ! -s $OUT_CSV ]]; then
	echo "$CSV_HEADER" >"$OUT_CSV"
elif [[ $(head -1 "$OUT_CSV") != "$CSV_HEADER" ]]; then
	die "$OUT_CSV has a different header; use a new file"
fi

# The edits. Each must change the tree, or the step fails.
apply_edit() {
	case $1 in
	comment)
		sed -i 's|^    // Strip the BOM if it is present$|&\n    // bench: comment-only edit|' "$LIB_SRC"
		;;
	body)
		sed -i 's|^    file.shebang = shebang;$|    file.shebang = shebang.filter(\|s\| !s.is_empty());|' "$LIB_SRC"
		;;
	sig)
		sed -i 's|^pub fn parse_file(mut content: &str) -> Result<File> {$|pub fn parse_file(mut content: \&str, _bench_extra: u32) -> Result<File> {|' "$LIB_SRC"
		;;
	newitem)
		echo 'pub fn bench_new_item() -> u32 { 1 }' >>"$LIB_SRC"
		;;
	round*)
		echo "// bench: churn $1" >>"$LIB_SRC"
		;;
	*) die "unknown edit $1" ;;
	esac
	! git -C "$CORPUS" diff --quiet -- syn || die "edit '$1' did not change $LIB_SRC"
}

# Sums the "Total Node/Edge Count" lines of one crate in one step.
incr_sum() {
	local prefix=$1 crate=$2 key=$3
	{ cat "$prefix".$crate.* 2>/dev/null || true; } | awk -v k="$key" '
		index($0, "[incremental] Total " k " Count: ") == 1 { s += $NF; n++ }
		END { if (n) print s }'
}

# Runs one cargo command in $TDIR and appends one CSV row.
run_step() {
	local rep=$1 scenario=$2 step=$3
	shift 3
	local inc=$TDIR/debug/incremental
	local tag="r${rep}.${scenario}.${step//[:,]/_}"
	local prefix=$LOG_DIR/$tag.incr
	local marker=$LOG_DIR/$tag.marker
	local pre_inodes=$LOG_DIR/$tag.inodes

	if [[ -d $inc ]]; then
		find "$inc" -name '*.o' -printf '%i\n' | sort -u >"$pre_inodes"
	else
		: >"$pre_inodes"
	fi
	touch "$marker"
	sleep 0.01

	local t0 t1
	t0=$(date +%s.%N)
	if ! (cd "$CORPUS" && nice -n "$NICE" env \
		CARGO_HOME="$CARGO_HOME_DIR" CARGO_TARGET_DIR="$TDIR" CARGO_INCREMENTAL=1 RUSTC_BOOTSTRAP=1 \
		RUSTC="$RUSTC_BIN" RUSTC_WRAPPER="$HERE/rustc-wrap.sh" \
		RUSTFLAGS="-Zincremental-info${EXTRA_RUSTFLAGS:+ $EXTRA_RUSTFLAGS}" \
		BENCH_INCR_LOG="$prefix" \
		"$CARGO" "$@" "${JOBS_ARGS[@]}" >"$LOG_DIR/$tag.log" 2>&1); then
		tail -30 "$LOG_DIR/$tag.log" >&2
		die "step $tag failed: cargo $*; log: $LOG_DIR/$tag.log"
	fi
	t1=$(date +%s.%N)

	local bytes=0 kb=0 crate_dirs=0 session_dirs=0 dg_files=0 dg_bytes=0 qc_bytes=0
	local new_sessions=0 wp_objs=0 wp_reused=0
	if [[ -d $inc ]]; then
		bytes=$(du -sb "$inc" | cut -f1)
		kb=$(du -sk "$inc" | cut -f1)
		crate_dirs=$(find "$inc" -mindepth 1 -maxdepth 1 -type d | wc -l)
		session_dirs=$(find "$inc" -mindepth 2 -maxdepth 2 -type d -name 's-*' | wc -l)
		read -r dg_files dg_bytes < <(find "$inc" -name dep-graph.bin -printf '%s\n' | awk '{ n++; s += $1 } END { print n + 0, s + 0 }')
		qc_bytes=$(find "$inc" -name query-cache.bin -printf '%s\n' | awk '{ s += $1 } END { print s + 0 }')
		local sess
		while IFS= read -r sess; do
			new_sessions=$((new_sessions + 1))
			local n r
			n=$(find "$sess" -maxdepth 1 -name '*.o' | wc -l)
			r=$(find "$sess" -maxdepth 1 -name '*.o' -printf '%i\n' | sort -u | comm -12 - "$pre_inodes" | wc -l)
			wp_objs=$((wp_objs + n))
			wp_reused=$((wp_reused + r))
		done < <(find "$inc" -mindepth 2 -maxdepth 2 -type d -name 's-*' -newer "$marker")
	fi

	local lib_nodes lib_edges bin_nodes bin_edges invocations ignored
	lib_nodes=$(incr_sum "$prefix" "$LIB_CRATE" Node)
	lib_edges=$(incr_sum "$prefix" "$LIB_CRATE" Edge)
	bin_nodes=$(incr_sum "$prefix" "$BIN_CRATE" Node)
	bin_edges=$(incr_sum "$prefix" "$BIN_CRATE" Edge)
	invocations=$(find "$LOG_DIR" -maxdepth 1 -name "$(basename "$prefix").*" | wc -l)
	ignored=$(cat "$prefix".* 2>/dev/null | grep -c 'ignoring cache' || true)

	local wall
	wall=$(awk -v a="$t0" -v b="$t1" 'BEGIN { printf "%.3f", b - a }')
	printf '%s,%s,%s,%s,"%s",%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,"%s"\n' \
		"$LABEL" "$rep" "$scenario" "$step" "$*" "$wall" "$bytes" "$kb" "$crate_dirs" "$session_dirs" \
		"$dg_files" "$dg_bytes" "$qc_bytes" "$new_sessions" "$wp_objs" "$wp_reused" \
		"$lib_nodes" "$lib_edges" "$bin_nodes" "$bin_edges" "$invocations" "$ignored" \
		"$RUSTC_RELEASE" "$EXTRA_RUSTFLAGS" >>"$OUT_CSV"
	printf '%-10s r%s %-10s %-22s %8ss  incr=%s B  dirs=%s  wp=%s/%s reused\n' \
		"$LABEL" "$rep" "$scenario" "$step" "$wall" "$bytes" "$crate_dirs" "$wp_reused" "$wp_objs" >&2
}

fresh_target() {
	TDIR=$LABEL_DIR/$1
	rm -rf "$TDIR"
}

BUILD=(build --workspace)

scenario_edits() {
	local rep edit
	for ((rep = 1; rep <= REPS; rep++)); do
		fresh_target "edits-r$rep"
		run_step "$rep" edits cold "${BUILD[@]}"
		for edit in $EDITS; do
			apply_edit "$edit"
			run_step "$rep" edits "edit:$edit" "${BUILD[@]}"
			run_step "$rep" edits "nochange:$edit" "${BUILD[@]}"
			revert_corpus
			run_step "$rep" edits "revert:$edit" "${BUILD[@]}"
		done
	done
}

# Alternates build, test --no-run and check. Rounds after the first add a
# comment line to lib.rs first, so each round recompiles every config.
scenario_churn_cmd() {
	local round
	fresh_target churn-cmd
	for ((round = 1; round <= CMD_ROUNDS; round++)); do
		((round > 1)) && apply_edit "round$round"
		run_step 1 churn-cmd "round$round:build" build --workspace
		run_step 1 churn-cmd "round$round:test" test --workspace --no-run
		run_step 1 churn-cmd "round$round:check" check --workspace
	done
	revert_corpus
}

# Builds syn alone with each feature set. Each set is a separate crate dir.
scenario_churn_feat() {
	local round i
	fresh_target churn-feat
	for ((round = 1; round <= FEAT_ROUNDS; round++)); do
		((round > 1)) && apply_edit "round$round"
		for i in "${!FEATURE_SETS[@]}"; do
			run_step 1 churn-feat "round$round:feat$((i + 1))" build -p syn --features "${FEATURE_SETS[$i]}"
		done
	done
	revert_corpus
}

for s in $SCENARIOS; do
	case $s in
	edits) scenario_edits ;;
	churn-cmd) scenario_churn_cmd ;;
	churn-feat) scenario_churn_feat ;;
	*) die "unknown scenario $s" ;;
	esac
done
echo "done: rows appended to $OUT_CSV; logs in $LOG_DIR" >&2
