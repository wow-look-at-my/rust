#!/usr/bin/env bash
# Prints the grok-bench TSV files as markdown tables: every row, then baseline against patched per step.
# Usage: ci-report.sh TSV...
set -euo pipefail
awk -F'\t' '
FNR == 1 { next }
{
	n++
	label[n] = $1; tc[n] = $2; step[n] = $3; wall[n] = $4
	g[n] = $5; q[n] = $6; o[n] = $7; x[n] = $8; t[n] = $9; d[n] = $10; s[n] = $11
	if (!($3 in seen)) { seen[$3] = 1; order[++steps] = $3 }
	w[$1, $3] = $4; tot[$1, $3] = $9; gr[$1, $3] = $5; qc[$1, $3] = $6
}
function gb(v) { return sprintf("%.3f", v / 1e9) }
function ratio(a, b) { return b > 0 ? sprintf("%.2fx", a / b) : "n/a" }
END {
	if (n == 0) { print "ci-report: no rows in the input" > "/dev/stderr"; exit 1 }
	print "| toolchain | commit | step | wall s | dep-graph.bin GB | query-cache.bin GB | *.o GB | other GB | total GB | crate dirs | sessions |"
	print "|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|"
	for (i = 1; i <= n; i++)
		printf "| %s | `%s` | %s | %s | %s | %s | %s | %s | %s | %s | %s |\n", label[i], substr(tc[i], 1, 12), step[i], wall[i], gb(g[i]), gb(q[i]), gb(o[i]), gb(x[i]), gb(t[i]), d[i], s[i]
	if (!(("baseline", order[1]) in w) || !(("patched", order[1]) in w)) exit 0
	print ""
	print "| step | wall baseline s | wall patched s | wall patched/baseline | total baseline GB | total patched GB | total patched/baseline | dep-graph patched/baseline | query-cache patched/baseline |"
	print "|---|---:|---:|---:|---:|---:|---:|---:|---:|"
	for (i = 1; i <= steps; i++) {
		k = order[i]
		printf "| %s | %s | %s | %s | %s | %s | %s | %s | %s |\n", k, w["baseline", k], w["patched", k], ratio(w["patched", k], w["baseline", k]), gb(tot["baseline", k]), gb(tot["patched", k]), ratio(tot["patched", k], tot["baseline", k]), ratio(gr["patched", k], gr["baseline", k]), ratio(qc["patched", k], qc["baseline", k])
	}
}
' "$@"
