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

# A paired leg names each config NAME@rROUND. Its rounds ran on one runner, so their wall times divide without a non-incremental build.
awk -F'\t' '
FNR == 1 { table = ($1 == "workload"); next }
!table || $6 != "incr" || $2 !~ /@r[0-9]+$/ { next }
{
	name = $2; sub(/@r[0-9]+$/, "", name); round = substr($2, length(name) + 3) + 0
	k = $1 SUBSEP $7 SUBSEP round
	wall[k, name] = $8
	if ($3 == "baseline") base[k] = name
	if (!(($1, $7) in seenstep)) { seenstep[$1, $7] = 1; steps[++ns] = $1 SUBSEP $7 }
	if (!(name in seencfg)) {
		seencfg[name] = 1
		# The baseline config goes first in each step.
		if ($3 == "baseline") { for (i = ++nc; i > 1; i--) cfgs[i] = cfgs[i - 1]; cfgs[1] = name } else cfgs[++nc] = name
	}
	if (round > rounds) rounds = round
}
function median(a, n,    i, j, t) {
	for (i = 2; i <= n; i++) for (j = i; j > 1 && a[j - 1] > a[j]; j--) { t = a[j]; a[j] = a[j - 1]; a[j - 1] = t }
	return n % 2 ? a[(n + 1) / 2] : (a[n / 2] + a[n / 2 + 1]) / 2
}
END {
	if (ns == 0) exit
	print ""
	print "Paired legs: each config ran on the same runner, round after round. A ratio is the wall time of the config divided by the wall time of the baseline compiler in the same round. The last column is the median over the rounds."
	print ""
	printf "| workload | step | config |"
	for (r = 1; r <= rounds; r++) printf " round %d s | round %d ratio |", r, r
	print " median ratio |"
	printf "|---|---|---|"
	for (r = 1; r <= rounds; r++) printf "---:|---:|"
	print "---:|"
	for (s = 1; s <= ns; s++) {
		split(steps[s], ws, SUBSEP)
		for (c = 1; c <= nc; c++) {
			printf "| %s | %s | %s |", ws[1], ws[2], cfgs[c]
			m = 0; delete rs
			for (r = 1; r <= rounds; r++) {
				k = ws[1] SUBSEP ws[2] SUBSEP r
				w = ((k, cfgs[c]) in wall) ? wall[k, cfgs[c]] : ""
				b = (k in base) ? wall[k, base[k]] : ""
				if (w != "" && b > 0) { rs[++m] = w / b; printf " %s | %.2fx |", w, w / b } else printf " %s | n/a |", w
			}
			print (m > 0 ? sprintf(" %.2fx |", median(rs, m)) : " n/a |")
		}
	}
}
' "$@"
