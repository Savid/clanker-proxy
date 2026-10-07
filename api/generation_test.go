package api_test

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// The generated package is a server and a client and nothing more: no
// Unimplemented stub, so a new operation fails to compile until it is
// implemented, and no telemetry hooks, which belong to the daemon's own
// middleware.
func TestGeneratedPackage(t *testing.T) {
	t.Parallel()

	const dir = "rest"

	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}

	if len(entries) == 0 {
		t.Fatalf("api/%s is empty: run make generate", dir)
	}

	var server, client bool

	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".go") {
			continue
		}

		if strings.HasSuffix(e.Name(), "_unimplemented_gen.go") {
			t.Errorf("api/%s/%s: no Unimplemented stub may be generated", dir, e.Name())
		}

		src, readErr := os.ReadFile(filepath.Join(dir, e.Name()))
		if readErr != nil {
			t.Fatal(readErr)
		}

		for _, banned := range []string{"ogen-go/ogen/otelogen", "type UnimplementedHandler"} {
			if strings.Contains(string(src), banned) {
				t.Errorf("api/%s/%s contains %q", dir, e.Name(), banned)
			}
		}

		server = server || strings.Contains(string(src), "func NewServer(")
		client = client || strings.Contains(string(src), "func NewClient(")
	}

	if !server || !client {
		t.Errorf("api/%s: server %v, client %v; want both", dir, server, client)
	}
}
