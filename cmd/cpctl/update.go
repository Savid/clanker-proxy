package main

import (
	"context"
	"flag"
	"fmt"
	"os"

	"github.com/savid/clanker-proxy/cli/update"
)

func updateCommands() []*command {
	return []*command{{
		name: "update", local: true,
		summary: "install the latest stable release of local cpd and cpctl",
		about: "Downloads from GitHub and verifies SHA-256 checksums before replacing both binaries " +
			"in this executable's directory. No daemon connection or owner token is needed. " +
			"The directory must be writable. Restart cpd afterward; running daemons keep their old version. " +
			"Use -check to check without changing files. In containers, update the image instead.",
		example: "cpctl update -check\n  cpctl update",
		flags: func(fs *flag.FlagSet) func(*app, []string) error {
			check := fs.Bool("check", false, "check for a newer stable release without installing")

			return func(a *app, _ []string) error {
				return a.runUpdate(*check, update.NewClient(), update.ExecutableDir)
			}
		},
	}}
}

type releaseClient interface {
	Check(context.Context, string) (update.Result, error)
	Install(context.Context, string, string) (update.Result, error)
}

func (a *app) runUpdate(check bool, client releaseClient, executableDir func() (string, error)) error {
	var result update.Result
	var err error

	if check {
		result, err = client.Check(a.ctx, version)
	} else {
		var dir string
		dir, err = executableDir()
		if err == nil {
			result, err = client.Install(a.ctx, version, dir)
		}
	}

	if err != nil {
		return &failure{
			exit: exitError, msg: err.Error(),
			hint: "check GitHub connectivity and installation directory permissions; see cpctl help update",
		}
	}

	if a.json {
		return jsonLine(a.stdout, result)
	}

	switch {
	case result.Installed:
		fmt.Fprintf(a.stdout, "installed cpd and cpctl %s; restart cpd to use the new version\n", result.Latest)
		next(a.stdout, step{"cpctl -version", "confirm the installed CLI version"},
			step{"cpctl me", "check the daemon after restarting it"})
	case result.Available:
		fmt.Fprintf(a.stdout, "new release: %s (current %s)\n", result.Latest, result.Current)
		next(a.stdout, step{"cpctl update", "install both binaries, then restart cpd"})
	default:
		fmt.Fprintf(a.stdout, "current %s; latest stable %s\n", result.Current, result.Latest)
		next(a.stdout, step{"cpctl help", "see available commands"})
	}

	return nil
}

func (a *app) updateNotice() {
	if a.json || os.Getenv("CI") != "" {
		return
	}

	dir, err := update.CacheDir()
	if err != nil {
		return
	}

	result, err := update.NewClient().Automatic(a.ctx, version, dir)
	if err == nil && result.Available {
		fmt.Fprintf(a.stderr, "update available: %s (current %s); run cpctl update, then restart cpd\n", result.Latest, version)
	}
}
