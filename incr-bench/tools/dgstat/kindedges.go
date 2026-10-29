// kindEdges prints each distinct pair of source kind and target kind once, sorted, in the "a -> b" form of the dep_graph.txt.

package main

import (
	"fmt"
	"io"
	"slices"

	"github.com/spf13/cobra"
	"github.com/wow-look-at-my/go-containers/set"
)

// kindEdges prints each distinct pair of source kind and target kind once, sorted, in the
// "a -> b" form of the dep_graph.txt. That dep_graph.txt -Z dump-dep-graph writes.
func kindEdges(out io.Writer, kn kindNamer, g *cgraph) {
	type pair struct{ a, b uint16 }
	seen := set.New[pair]()
	for p := range g.nodes() {
		e := g.edges(p)
		for j := range e.n {
			if t := g.position(e.at(j)); t >= 0 {
				seen.Add(pair{g.kind(p), g.kind(t)})
			}
		}
	}
	lines := make([]string, 0, seen.Len())
	for k := range seen.All() {
		lines = append(lines, kn.name(k.a)+" -> "+kn.name(k.b))
	}
	slices.Sort(lines)
	for _, l := range lines {
		fmt.Fprintln(out, l)
	}
}

func init() {
	rootCmd.AddCommand(&cobra.Command{
		Use:   "kindedges KINDS.txt DEP-GRAPH.bin",
		Short: "Print the distinct kind-to-kind edges of a dep graph, to check a kind list against -Z dump-dep-graph",
		Args:  cobra.ExactArgs(2),
		RunE: func(cmd *cobra.Command, args []string) error {
			names, err := readNames(args[0])
			if err != nil {
				return err
			}
			m, data, err := mapFile(args[1])
			if err != nil {
				return err
			}
			defer m.Unmap()
			g, err := parseCompact(data)
			if err != nil {
				return err
			}
			if len(names) != g.numKinds {
				return fmt.Errorf("%s: %d names for %d kinds", args[0], len(names), g.numKinds)
			}
			kindEdges(cmd.OutOrStdout(), kindNamer{names, g.numKinds}, g)
			return nil
		},
	})
}
