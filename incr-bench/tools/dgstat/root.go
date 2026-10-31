package main

import (
	"os"
	"runtime"

	"github.com/spf13/cobra"
)

var rootCmd = &cobra.Command{
	Use:          "dgstat KINDS.txt DEP-GRAPH.bin...",
	Short:        "Measure rustc incremental dep-graph.bin and query-cache.bin files",
	Args:         cobra.MinimumNArgs(2),
	SilenceUsage: true,
	RunE: func(cmd *cobra.Command, args []string) error {
		return run(cmd.OutOrStdout(), args, rootWorkers)
	},
}

var rootWorkers int

func init() {
	rootCmd.Flags().IntVar(&rootWorkers, "workers", runtime.GOMAXPROCS(0), "number of workers for the scan, the sorts and the compression")
}

func main() {
	if err := rootCmd.Execute(); err != nil {
		os.Exit(1)
	}
}
