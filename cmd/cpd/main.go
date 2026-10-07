// Command cpd is the clanker-proxy daemon. It serves one API: the owner
// drives it with cpctl, peers deliver to it, and anyone may ask to become a
// peer. It delivers the owner's events to peers.
//
//	cpd [-name <you>] [-url https://<where peers reach you>] [flags]
//	cpd -version
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"log/slog"
	"os"
	"os/signal"
	"os/user"
	"path/filepath"
	"strings"
	"syscall"

	"golang.org/x/sync/errgroup"

	"github.com/savid/clanker-proxy/internal/delivery"
	"github.com/savid/clanker-proxy/internal/httpserve"
	"github.com/savid/clanker-proxy/internal/inbox"
	"github.com/savid/clanker-proxy/internal/server"
	"github.com/savid/clanker-proxy/internal/store"
	"github.com/savid/clanker-proxy/pkg/thread"
)

// version is set at build time: -ldflags "-X main.version=...".
var version = "dev"

// tokenFile is the owner token's file in the data directory.
const tokenFile = "owner.token"

func main() {
	if err := run(os.Args[1:], os.Stdout, os.Stderr); err != nil {
		fmt.Fprintln(os.Stderr, "cpd:", err)
		os.Exit(1)
	}
}

type options struct {
	name      string
	url       string
	dir       string
	listen    string
	logFormat string
	logLevel  slog.Level
	version   bool
}

// errHelp is returned when -h or -help was given; usage has been printed.
var errHelp = errors.New("help requested")

// defaultDir is $CP_DIR, else ~/.cp: where the database and owner token
// live.
func defaultDir() string {
	if v := os.Getenv("CP_DIR"); v != "" {
		return v
	}

	home, err := os.UserHomeDir()
	if err != nil {
		return ".cp"
	}

	return filepath.Join(home, ".cp")
}

func parse(args []string, stderr io.Writer) (options, error) {
	o := options{logLevel: slog.LevelInfo}

	fs := flag.NewFlagSet("cpd", flag.ContinueOnError)
	fs.SetOutput(stderr)
	fs.StringVar(&o.name, "name", "", "your name, offered to peers (default: your login name); remembered")
	fs.StringVar(&o.url, "url", "", "where peers reach this daemon, e.g. https://cp.example.com; remembered")
	fs.StringVar(&o.dir, "dir", defaultDir(), "data directory: database and owner token (env CP_DIR)")
	fs.StringVar(&o.listen, "listen", "127.0.0.1:8080", "listen address; put TLS in front of it")
	fs.StringVar(&o.logFormat, "log-format", "text", "log format: text or json")
	fs.TextVar(&o.logLevel, "log-level", o.logLevel, "log level: DEBUG, INFO, WARN or ERROR")
	fs.BoolVar(&o.version, "version", false, "print the version and exit")

	if err := fs.Parse(args); err != nil {
		if errors.Is(err, flag.ErrHelp) {
			return o, errHelp
		}

		return o, fmt.Errorf("flags: %w", err)
	}

	if fs.NArg() > 0 {
		return o, fmt.Errorf("unexpected arguments: %s", strings.Join(fs.Args(), " "))
	}

	if o.logFormat != "text" && o.logFormat != "json" {
		return o, fmt.Errorf("-log-format %q: text or json", o.logFormat)
	}

	if o.url != "" {
		if err := inbox.ValidURL(o.url); err != nil {
			return o, fmt.Errorf("-url: %w", err)
		}
	}

	if o.name != "" {
		name, ok := thread.NormalizeName(o.name)
		if !ok {
			return o, fmt.Errorf("-name %q: lower-case letters, digits and single hyphens", o.name)
		}

		o.name = name
	}

	return o, nil
}

func run(args []string, stdout, stderr io.Writer) error {
	o, err := parse(args, stderr)
	if errors.Is(err, errHelp) {
		return nil
	}

	if err != nil {
		return err
	}

	if o.version {
		fmt.Fprintln(stdout, version)

		return nil
	}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	log := newLogger(stderr, o).With("version", version)

	return serve(ctx, log, o)
}

func serve(ctx context.Context, log *slog.Logger, o options) error {
	if err := os.MkdirAll(o.dir, 0o700); err != nil {
		return fmt.Errorf("data dir: %w", err)
	}

	st, err := store.Open(ctx, filepath.Join(o.dir, "cp.db"))
	if err != nil {
		return err
	}
	defer st.Close()

	name, publicURL, err := settings(ctx, st, o)
	if err != nil {
		return err
	}

	token, err := ownerToken(filepath.Join(o.dir, tokenFile))
	if err != nil {
		return err
	}

	log = log.With("owner", name)
	deliverer := delivery.New(log, st, delivery.Config{})
	ib := inbox.New(log, st, deliverer, inbox.Config{Self: name, URL: publicURL})
	deliverer.Attach(ib, ib.Wake())

	api, err := server.New(log, ib, server.Config{OwnerToken: token, Version: version})
	if err != nil {
		return err
	}

	ln, err := httpserve.Listen(ctx, o.listen)
	if err != nil {
		return err
	}

	if publicURL == "" {
		log.WarnContext(ctx, "no -url: peers can reach you, but you cannot ask anyone to peer")
	}

	log.InfoContext(ctx, "starting", "dir", o.dir, "url", publicURL)

	g, ctx := errgroup.WithContext(ctx)
	g.Go(func() error { return httpserve.Serve(ctx, log, ln, api, api.Shutdown) })
	g.Go(func() error { return deliverer.Run(ctx) })

	if err = g.Wait(); err != nil {
		return err
	}

	log.InfoContext(ctx, "stopped")

	return nil
}

// settings returns the owner's name and public URL. The name is fixed on
// first run, since threads record it; the URL is the flag when given, which
// is then remembered, else what was remembered.
func settings(ctx context.Context, st *store.Store, o options) (name, publicURL string, err error) {
	if name, err = st.Meta(ctx, "name"); err != nil {
		return "", "", err
	}

	switch {
	case name != "" && o.name != "" && o.name != name:
		return "", "", fmt.Errorf("-name %s: this data directory's threads belong to %s; use another -dir for a new name", o.name, name)
	case name == "" && o.name != "":
		name = o.name
	case name == "":
		if name, err = loginName(); err != nil {
			return "", "", err
		}
	}

	if err = st.SetMeta(ctx, "name", name); err != nil {
		return "", "", err
	}

	publicURL, err = remember(ctx, st, "url", strings.TrimRight(o.url, "/"))

	return name, publicURL, err
}

// remember stores value under key when it is set, and returns what is
// stored.
func remember(ctx context.Context, st *store.Store, key, value string) (string, error) {
	if value != "" {
		return value, st.SetMeta(ctx, key, value)
	}

	return st.Meta(ctx, key)
}

func loginName() (string, error) {
	u, err := user.Current()
	if err != nil {
		return "", fmt.Errorf("pass -name: %w", err)
	}

	name, ok := thread.NormalizeName(u.Username)
	if !ok {
		return "", fmt.Errorf("login name %q is not a valid name; pass -name", u.Username)
	}

	return name, nil
}

// ownerToken reads the owner token from path, creating it on first run.
func ownerToken(path string) (string, error) {
	b, err := os.ReadFile(path)
	if err == nil {
		return strings.TrimSpace(string(b)), nil
	}

	if !errors.Is(err, os.ErrNotExist) {
		return "", fmt.Errorf("owner token: %w", err)
	}

	token := inbox.NewSecret(inbox.OwnerPrefix)

	f, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if err != nil {
		return "", fmt.Errorf("owner token: %w", err)
	}

	if _, err = fmt.Fprintln(f, token); err != nil {
		_ = f.Close()

		return "", fmt.Errorf("owner token: %w", err)
	}

	if err = f.Close(); err != nil {
		return "", fmt.Errorf("owner token: %w", err)
	}

	return token, nil
}

// newLogger builds the process logger: text for a terminal, JSON for a log
// collector.
func newLogger(w io.Writer, o options) *slog.Logger {
	opts := &slog.HandlerOptions{Level: o.logLevel}
	if o.logFormat == "json" {
		return slog.New(slog.NewJSONHandler(w, opts))
	}

	return slog.New(slog.NewTextHandler(w, opts))
}
