package importguard_test

import (
	"slices"
	"testing"

	"github.com/savid/clanker-proxy/internal/testutil/importguard"
)

// The repository's dependency boundaries. Each entry names a package and the
// only packages allowed to import it; anything else importing it fails here.
// Add a line when a new boundary matters, and change one deliberately.
var boundaries = []struct {
	pkg     string
	allowed []string
	why     string
	// exact limits the boundary to pkg itself, not the packages under it.
	exact bool
}{
	{
		pkg: importguard.Module + "/api/rest",
		allowed: []string{
			importguard.Module + "/internal/server", importguard.Module + "/internal/delivery",
			importguard.Module + "/cmd/cpctl",
		},
		why: "generated API types stay at the edges that serve and call the API",
	},
	{
		pkg:     importguard.Module + "/api",
		allowed: []string{importguard.Module + "/internal/server"},
		why:     "the spec is served by the server and read by nothing else in production",
		exact:   true,
	},
	{
		pkg: importguard.Module + "/internal/store",
		allowed: []string{
			importguard.Module + "/internal/inbox", importguard.Module + "/internal/delivery",
			importguard.Module + "/internal/server", importguard.Module + "/internal/testutil/inboxtest",
			importguard.Module + "/cmd/cpd",
		},
		why: "only the daemon touches the database; cpctl reaches it through the API",
	},
	{
		pkg: importguard.Module + "/internal",
		allowed: []string{
			importguard.Module + "/cmd/cpd",
		},
		why: "cpctl is a client of the API: it shares pkg/ and api/rest, never the daemon's internals",
	},
}

func TestBoundaries(t *testing.T) {
	t.Parallel()

	packages := importguard.Packages(t, "./...")

	for _, b := range boundaries {
		importers := importguard.Importers(packages, b.pkg)
		if b.exact {
			importers = importguard.ExactImporters(packages, b.pkg)
		}

		for _, importer := range importers {
			if !slices.Contains(b.allowed, importer) {
				t.Errorf("%s imports %s; %s", importer, b.pkg, b.why)
			}
		}
	}
}

// pkg/ holds decision packages that must stay free of the application and
// the generated API.
func TestDecisionPackagesStayPure(t *testing.T) {
	t.Parallel()

	for _, p := range importguard.Packages(t, "./pkg/...") {
		for _, imp := range slices.Concat(p.Imports, p.TestImports, p.XTestImports) {
			if importguard.Within(imp, importguard.Module+"/internal") || importguard.Within(imp, importguard.Module+"/api") {
				t.Errorf("%s imports %s; pkg/ must not depend on the application", p.ImportPath, imp)
			}
		}
	}
}
