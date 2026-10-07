package main

import (
	"context"
	"fmt"
	"io"
	"log/slog"
	"os"
	"time"

	"github.com/savid/clanker-proxy/cli/update"
)

func runUpdate(ctx context.Context, stdout io.Writer, check bool) error {
	client := update.NewClient()
	var result update.Result
	var err error

	if check {
		result, err = client.Check(ctx, version)
	} else {
		var dir string
		dir, err = update.ExecutableDir()
		if err == nil {
			result, err = client.Install(ctx, version, dir)
		}
	}

	if err != nil {
		return fmt.Errorf("update: %w; check GitHub connectivity and installation directory permissions", err)
	}

	switch {
	case result.Installed:
		fmt.Fprintf(stdout, "installed cpd and cpctl %s; restart cpd to use the new version\n", result.Latest)
	case result.Available:
		fmt.Fprintf(stdout, "new release: %s (current %s); run cpd -update, then restart cpd\n", result.Latest, version)
	default:
		fmt.Fprintf(stdout, "current %s; latest stable %s\n", version, result.Latest)
	}

	return nil
}

func watchUpdates(ctx context.Context, log *slog.Logger) error {
	if os.Getenv("CP_NO_UPDATE_CHECK") == "1" || os.Getenv("CI") != "" {
		return nil
	}

	dir, err := update.CacheDir()
	if err != nil {
		return nil
	}

	client := update.NewClient()
	ticker := time.NewTicker(24 * time.Hour)
	defer ticker.Stop()

	for {
		result, checkErr := client.Automatic(ctx, version, dir)
		if checkErr == nil && result.Available {
			log.InfoContext(ctx, "update available; run cpctl update, then restart cpd", "latest", result.Latest)
		}

		select {
		case <-ctx.Done():
			return nil
		case <-ticker.C:
		}
	}
}
