#!/usr/bin/env bash
# Prints the ci-bench.sh TSV files as markdown: the configs, then one table across them. Usage: ci-report.sh TSV...
set -euo pipefail
# A leg also writes side files (crate times, graph bytes) next to its TSV. Only a file whose
# header starts with "workload" is a results table.
awk -F'\t' '
FNR == 1 { table = ($1 == "workload"); next }
!table { next }
{
	k = $1 SUBSEP $2 SUBSEP $7
	if ($6 == "noincr") { full[k] = $8; next }
	n++
	row[n] = k; w[n] = $1; c[n] = $2; st[n] = $7; wall[n] = $8
	g[n] = $9; q[n] = $10; o[n] = $11; x[n] = $12; t[n] = $13; d[n] = $14; s[n] = $15
	if (!($2 in cfg)) { cfg[$2] = sprintf("| %s | %s | `%s` | %s |", $2, $3, substr($4, 1, 12), $5 == "" ? "none" : "`" $5 "`"); order[++ncfg] = $2 }
}
function mb(v) { return sprintf("%.1f", v / 1e6) }
function ratio(a, b) { return b > 0 ? sprintf("%.2fx", a / b) : "n/a" }
END {
	if (n == 0) { print "ci-report: no incremental rows in the input" > "/dev/stderr"; exit 1 }
	print "| config | compiler | commit | rustc flags |"
	print "|---|---|---|---|"
	for (i = 1; i <= ncfg; i++) print cfg[order[i]]
	print ""
	print "| workload | config | step | incr wall s | non-incr wall s | incr / non-incr | dep-graph.bin MB | query-cache.bin MB | *.o MB | graph+cache % of *.o | other MB | total MB | crate dirs | sessions |"
	print "|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
	for (i = 1; i <= n; i++) {
		f = (row[i] in full) ? full[row[i]] : ""
		pct = o[i] > 0 ? sprintf("%.0f%%", 100 * (g[i] + q[i]) / o[i]) : "n/a"
		printf "| %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s |\n", w[i], c[i], st[i], wall[i], f, ratio(wall[i], f), mb(g[i]), mb(q[i]), mb(o[i]), pct, mb(x[i]), mb(t[i]), d[i], s[i]
	}
}
' "$@"

# An -Zincremental-info leg also writes a node fates file, whose header starts with "fates".
awk -F'\t' '
FNR == 1 { table = ($1 == "fates"); next }
!table { next }
{ rows[++n] = sprintf("| %s | %s | %s | %s | %s | %s | %s | %s | %s |", $1, $2, $3, $4, $5, $6, $7, $8, $9) }
END {
	if (n == 0) exit
	print ""
	print "Node fates from -Zincremental-info, summed over all crates. A red node ran again and its result changed."
	print ""
	print "| workload | config | step | previous nodes | promoted green | re-run green | re-run red | unreached | new |"
	print "|---|---|---|---:|---:|---:|---:|---:|---:|"
	for (i = 1; i <= n; i++) print rows[i]
}
' "$@"
