package main

import (
	"bytes"
	"errors"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/savid/clanker-proxy/internal/store"
)

func TestParse(t *testing.T) {
	t.Parallel()

	defaults := options{dir: defaultDir(), listen: "127.0.0.1:8080", logFormat: "text", logLevel: slog.LevelInfo}

	for name, tc := range map[string]struct {
		args    []string
		want    options
		wantErr string
	}{
		"defaults": {args: nil, want: defaults},
		"everything set": {
			args: []string{
				"-name", "Savid", "-url", "https://cp.example.com", "-dir", "/data", "-listen", ":9000",
				"-log-format", "json", "-log-level", "debug", "-version",
			},
			want: options{
				name: "savid", url: "https://cp.example.com", dir: "/data", listen: ":9000",
				logFormat: "json", logLevel: slog.LevelDebug, version: true,
			},
		},
		"bad name":           {args: []string{"-name", "not a name"}, wantErr: "-name"},
		"bad url":            {args: []string{"-url", "cp.example.com"}, wantErr: "-url"},
		"bad format":         {args: []string{"-log-format", "yaml"}, wantErr: `-log-format "yaml": text or json`},
		"bad level":          {args: []string{"-log-level", "loud"}, wantErr: "flags: "},
		"unknown":            {args: []string{"-nope"}, wantErr: "flags: "},
		"stray argument":     {args: []string{"serve"}, wantErr: "unexpected arguments"},
		"update and check":   {args: []string{"-update", "-check-update"}, wantErr: "use only one"},
		"update and version": {args: []string{"-update", "-version"}, wantErr: "use only one"},
		"help wanted":        {args: []string{"-h"}, wantErr: errHelp.Error()},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			var stderr bytes.Buffer

			got, err := parse(tc.args, &stderr)

			switch {
			case tc.wantErr == "" && err != nil:
				t.Fatalf("parse(%q) = %v", tc.args, err)
			case tc.wantErr != "" && (err == nil || !strings.Contains(err.Error(), tc.wantErr)):
				t.Fatalf("parse(%q) error = %v, want %q", tc.args, err, tc.wantErr)
			case err == nil && got != tc.want:
				t.Fatalf("parse(%q) = %+v, want %+v", tc.args, got, tc.want)
			}
		})
	}
}

func TestRunPrintsVersionAndHelp(t *testing.T) {
	t.Parallel()

	var stdout, stderr bytes.Buffer

	if err := run([]string{"-version"}, &stdout, &stderr); err != nil || strings.TrimSpace(stdout.String()) != version {
		t.Fatalf("run(-version) = %v, stdout %q", err, stdout.String())
	}

	if err := run([]string{"-h"}, &stdout, &stderr); err != nil || !strings.Contains(stderr.String(), "-listen") {
		t.Fatalf("run(-h) = %v, stderr %q", err, stderr.String())
	}

	err := run([]string{"-log-format", "yaml"}, &stdout, &stderr)
	if err == nil || errors.Is(err, errHelp) {
		t.Fatalf("run(bad flag) = %v", err)
	}
}

// The name is fixed on first run; the URL is remembered and may change.
func TestSettings(t *testing.T) {
	t.Parallel()

	st, err := store.Open(t.Context(), filepath.Join(t.TempDir(), "cp.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()

	for _, tc := range []struct {
		o             options
		name, wantURL string
		wantErr       bool
	}{
		{options{name: "alice", url: "https://a.example/"}, "alice", "https://a.example", false},
		{options{}, "alice", "https://a.example", false},
		{options{name: "alice", url: "https://b.example"}, "alice", "https://b.example", false},
		{options{name: "al"}, "", "", true},
	} {
		name, u, setErr := settings(t.Context(), st, tc.o)
		if (setErr != nil) != tc.wantErr || name != tc.name || u != tc.wantURL {
			t.Errorf("settings(%+v) = %q, %q, %v; want %q, %q, error %v", tc.o, name, u, setErr, tc.name, tc.wantURL, tc.wantErr)
		}
	}
}

// The owner token is made once, private, and read back the same.
func TestOwnerToken(t *testing.T) {
	t.Parallel()

	path := filepath.Join(t.TempDir(), tokenFile)

	first, err := ownerToken(path)
	if err != nil || !strings.HasPrefix(first, "cpo_") || len(first) != 47 {
		t.Fatalf("ownerToken = %q, %v", first, err)
	}

	fi, err := os.Stat(path)
	if err != nil || fi.Mode().Perm() != 0o600 {
		t.Fatalf("token file mode = %v, %v", fi.Mode(), err)
	}

	if again, _ := ownerToken(path); again != first {
		t.Errorf("token changed: %q then %q", first, again)
	}
}
