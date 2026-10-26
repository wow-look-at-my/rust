#!/usr/bin/env bash
# Writes one table of every CI benchmark run, one row per run, workload and
# config at the body edit.
set -euo pipefail
dir=${1:?usage: ci-summary.sh RESULTS_DIR}

printf '%s\n' '# CI benchmark summary' '' \
	'One row per run, workload and config, at the `edit:body` step. Numbers from different runs come from different runners and patched commits. Compare a ratio only with a row of the same run.' '' \
	'`all but *.o` is everything in the incremental dirs except the object files. The `-Zincremental-inplace-cache` rows keep their query cache in a file that `graph+cache` does not count, so read those rows by `all but *.o`.' '' \
	'| run | workload | config | compiler commit | rustc flags | graph+cache MB | all but *.o MB | *.o MB | graph+cache % of *.o | all but *.o % of *.o | body edit incr / non-incr |' \
	'|---|---|---|---|---|---:|---:|---:|---:|---:|---:|'

for f in "$dir"/[0-9]*.md; do
	run=$(basename "$f" .md)
	awk -F'|' -v run="$run" '
		function trim(s) { gsub(/^ +| +$/, "", s); return s }
		# The config table: | config | compiler | commit | rustc flags |
		NF == 6 && trim($3) ~ /^(baseline|patched)$/ { commit[trim($2)] = trim($4); flags[trim($2)] = trim($5) }
		NF == 16 && trim($4) == "edit:body" {
			cfg = trim($3)
			objs = trim($10)
			rest = trim($13) - objs
			printf "| %s | %s | %s | %s | %s | %.0f | %.0f | %.0f | %s | %.0f%% | %s |\n", run, trim($2), cfg, commit[cfg], flags[cfg], trim($8) + trim($9), rest, objs, trim($11), 100 * rest / objs, trim($7)
		}
	' "$f"
done
