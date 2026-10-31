package main

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestKindEdges(t *testing.T) {
	dir := t.TempDir()
	dg := filepath.Join(dir, "dep-graph.bin")
	require.NoError(t, os.WriteFile(dg, append(testGraphWith(false), endMagic...), 0o644))
	names := make([]string, testKinds)
	for i := range names {
		names[i] = "k" + string(rune('a'+i))
	}
	kinds := filepath.Join(dir, "kinds.txt")
	require.NoError(t, os.WriteFile(kinds, []byte(strings.Join(names, "\n")), 0o644))

	var out bytes.Buffer
	rootCmd.SetOut(&out)
	rootCmd.SetArgs([]string{"kindedges", kinds, dg})
	require.NoError(t, rootCmd.Execute())
	require.Equal(t, "kc -> kb\nkc -> kc\nkd -> kb\nkd -> kc\n", out.String())

	require.NoError(t, os.WriteFile(kinds, []byte("a\nb\n"), 0o644))
	rootCmd.SetArgs([]string{"kindedges", kinds, dg})
	require.ErrorContains(t, rootCmd.Execute(), "2 names for 10 kinds")
	rootCmd.SetArgs([]string{"kindedges", kinds, filepath.Join(dir, "missing")})
	require.Error(t, rootCmd.Execute())
	rootCmd.SetArgs([]string{"kindedges", filepath.Join(dir, "missing"), dg})
	require.Error(t, rootCmd.Execute())
	require.NoError(t, os.WriteFile(dg, []byte("RSIC"), 0o644))
	rootCmd.SetArgs([]string{"kindedges", kinds, dg})
	require.Error(t, rootCmd.Execute())
}
