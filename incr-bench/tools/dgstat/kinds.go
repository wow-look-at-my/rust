// parseKinds reads rustc_middle's queries.rs.

package main

import (
	"bufio"
	"errors"
	"fmt"
	"os"
	"regexp"
	"strings"

	"github.com/spf13/cobra"
)

type kindDef struct {
	name                    string
	query                   bool
	cacheOnDisk, evalAlways bool
}

var (
	nonQueryRe = regexp.MustCompile(`^\s*non_query\s+(\w+)`)
	queryRe    = regexp.MustCompile(`^\s*query\s+(\w+)\s*\(`)
)

// parseKinds reads rustc_middle's queries.rs. The result is in DepKind order:
// the non_query names first, then the queries in file order.
func parseKinds(src string) ([]kindDef, error) {
	var nonQueries, queries []kindDef
	var cur *kindDef
	sc := bufio.NewScanner(strings.NewReader(src))
	sc.Buffer(nil, 1<<20)
	for sc.Scan() {
		line := sc.Text()
		if cur != nil {
			// A query block ends at the first line that is only a brace at the query's indent.
			if line == "    }" {
				queries = append(queries, *cur)
				cur = nil
				continue
			}
			code, _, _ := strings.Cut(line, "//")
			switch strings.TrimSpace(code) {
			case "cache_on_disk":
				cur.cacheOnDisk = true
			case "eval_always":
				cur.evalAlways = true
			}
			continue
		}
		if m := nonQueryRe.FindStringSubmatch(line); m != nil {
			nonQueries = append(nonQueries, kindDef{name: m[1]})
		} else if m := queryRe.FindStringSubmatch(line); m != nil {
			cur = &kindDef{name: m[1], query: true}
		}
	}
	if err := sc.Err(); err != nil {
		return nil, err
	}
	if cur != nil {
		return nil, fmt.Errorf("query %s has no closing brace", cur.name)
	}
	if len(nonQueries) == 0 || len(queries) == 0 {
		return nil, errors.New("no non_query or no query definitions found")
	}
	return append(nonQueries, queries...), nil
}

func readKinds(path string) ([]kindDef, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	return parseKinds(string(b))
}

func kindNames(defs []kindDef) []string {
	names := make([]string, len(defs))
	for i, d := range defs {
		names[i] = d.name
	}
	return names
}

func init() {
	rootCmd.AddCommand(&cobra.Command{
		Use:   "kinds QUERIES.rs",
		Short: "Print the DepKind names of a rustc_middle queries.rs, one per line",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			defs, err := readKinds(args[0])
			if err != nil {
				return err
			}
			for _, n := range kindNames(defs) {
				fmt.Fprintln(cmd.OutOrStdout(), n)
			}
			return nil
		},
	})
}
