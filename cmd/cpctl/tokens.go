package main

import (
	"errors"
	"flag"
	"fmt"
	"io"
	"io/fs"
	"os"
	"strconv"
	"strings"
	"text/tabwriter"
	"time"

	"github.com/savid/clanker-proxy/api/rest"
)

func tokenCommands() []*command {
	return []*command{
		{
			name: "token add", args: "<name> -o <file> [-peers <names>] [-expires <duration>]", minArgs: 1, maxArgs: 1,
			summary: "create an agent token that works threads (all, or -peers' only) and nothing else",
			about: "An agent token can list, read and act on every thread, current and future, wait on them and " +
				"read your name. Opening threads, peering, webhooks and tokens refuse it with 403 (exit 5). Give it to an agent as CP_TOKEN " +
				"instead of the owner token. -peers bob,carol limits it to threads with those peers, which must be " +
				"peers or former peers: it lists and follows only theirs, and any other thread answers as if it " +
				"did not exist. -peers '*', or no -peers, is every peer; an empty -peers is refused. The token is shown only now: -o writes it to a new file readable only " +
				"by you; -o - writes it to stdout, for piping into a secret store. -expires takes a duration such as " +
				"90d or 12h; without it, the token works until cpctl token rm.",
			example: "cpctl token add amp-inbox -o amp.token -expires 90d",
			flags:   tokenAddFlags,
		},
		{
			name: "token ls", summary: "list agent tokens and when they were last used",
			about:   "Expired tokens stay listed, refused, until removed or until a new token takes their name.",
			example: "cpctl token ls",
			flags:   func(*flag.FlagSet) func(*app, []string) error { return runTokens },
		},
		{
			name: "token rm", args: "<name>", minArgs: 1, maxArgs: 1,
			summary: "revoke an agent token",
			about:   "It stops working at once; an open wait or watch with it ends at its next update or within 15s.",
			example: "cpctl token rm amp-inbox",
			flags:   func(*flag.FlagSet) func(*app, []string) error { return runTokenRemove },
		},
	}
}

func tokenAddFlags(fs *flag.FlagSet) func(*app, []string) error {
	out := fs.String("o", "", "new `file` for the token, or - for stdout")
	expires := fs.String("expires", "", "lifetime such as 90d or 12h (default: until revoked)")
	var peers peersFlag
	fs.Var(&peers, "peers", "comma-separated peer `names` it may reach, or '*' for every peer (the default)")

	return func(a *app, pos []string) error {
		if *out == "" {
			return usageError("-o is required: the token is shown only once")
		}

		scope, err := peers.list()
		if err != nil {
			return err
		}

		req := &rest.AgentTokenInput{Name: name(pos[0]), Peers: scope}

		if *expires != "" {
			d, lifeErr := parseLifetime(*expires)
			if lifeErr != nil {
				return lifeErr
			}

			req.ExpiresAt = rest.NewOptDateTime(a.now().Add(d).UTC())
		}

		// Created before the token exists, so a file that cannot be written
		// does not leave a token nobody has.
		w, closeOut, err := tokenOutput(a, *out)
		if err != nil {
			return err
		}

		t, err := a.client.CreateAgentToken(a.ctx, req)
		if err != nil {
			_ = closeOut(false)

			return err
		}

		_, err = fmt.Fprintln(w, t.Token)
		if err = errors.Join(err, closeOut(err == nil)); err != nil {
			// A token nobody holds is only a risk.
			_ = a.client.DeleteAgentToken(a.ctx, rest.DeleteAgentTokenParams{Name: t.Name})

			return fmt.Errorf("write token: %w", err)
		}

		// Stdout holds only the token, so it can be piped.
		if *out == "-" {
			return nil
		}

		created := &rest.AgentTokenSummary{Name: t.Name, Peers: t.Peers, CreatedAt: t.CreatedAt, ExpiresAt: t.ExpiresAt}

		return a.print(created, func(w io.Writer) {
			fmt.Fprintf(w, "created agent token %s for %s, %s\nwrote it to %s\n", t.Name, forPeers(t.Peers), expiry(t.ExpiresAt, a.now()), *out)
			next(w, step{"CP_TOKEN=\"$(cat " + *out + ")\" cpctl inbox", "what the agent can run with it"},
				step{"cpctl token rm " + string(t.Name), "revoke it"})
		})
	}
}

// tokenOutput opens where the token goes. A file must be new, so an
// existing secret is never overwritten. done(true) syncs and closes it and
// reports any failure; done(false), or a failure, removes it.
func tokenOutput(a *app, path string) (io.Writer, func(ok bool) error, error) {
	if path == "-" {
		return a.stdout, func(bool) error { return nil }, nil
	}

	f, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	switch {
	case errors.Is(err, fs.ErrExist):
		return nil, nil, usageError("%s already exists; choose a new file", path)
	case err != nil:
		return nil, nil, usageError("cannot create %s: %v", path, err)
	}

	return f, func(ok bool) error {
		var syncErr error
		if ok {
			syncErr = f.Sync()
		}

		closeErr := errors.Join(syncErr, f.Close())
		if !ok || closeErr != nil {
			_ = os.Remove(path)
		}

		return closeErr
	}, nil
}

// maxLifetime bounds -expires well inside time.Duration.
const maxLifetime = 10 * 365 * 24 * time.Hour

// parseLifetime reads a Go duration, or whole days as "<n>d".
func parseLifetime(s string) (time.Duration, error) {
	d, err := time.ParseDuration(s)
	if days, isDays := strings.CutSuffix(s, "d"); isDays {
		n, atoiErr := strconv.Atoi(days)
		d, err = time.Duration(n)*24*time.Hour, atoiErr
		if n > int(maxLifetime/(24*time.Hour)) {
			d = maxLifetime + 1
		}
	}

	if err != nil || d < time.Minute || d > maxLifetime {
		return 0, usageError("-expires must be between 1m and 3650d, such as 90d or 12h")
	}

	return d, nil
}

func expiry(t rest.OptDateTime, now time.Time) string {
	at, ok := t.Get()
	switch {
	case !ok:
		return "never expires"
	case !at.After(now):
		return "expired " + at.UTC().Format(time.RFC3339)
	default:
		return "expires " + at.UTC().Format(time.RFC3339)
	}
}

func runTokens(a *app, _ []string) error {
	l, err := a.client.ListAgentTokens(a.ctx)
	if err != nil {
		return err
	}

	return a.print(l, func(w io.Writer) {
		if len(l.Tokens) == 0 {
			fmt.Fprintln(w, "no agent tokens")
		}

		tw := tabwriter.NewWriter(w, 0, 4, 2, ' ', 0)
		for _, t := range l.Tokens {
			used := "never used"
			if at, ok := t.UsedAt.Get(); ok {
				used = "used " + at.UTC().Format(time.RFC3339)
			}

			fmt.Fprintf(tw, "%s\tpeers: %s\tcreated %s\t%s\t%s\n", t.Name, forPeers(t.Peers), t.CreatedAt.UTC().Format(time.RFC3339), expiry(t.ExpiresAt, a.now()), used)
		}

		_ = tw.Flush()

		next(w, step{"cpctl token add <name> -o <file>", "create one for an agent"}, step{"cpctl token rm <name>", "revoke one, or remove an expired one"})
	})
}

func runTokenRemove(a *app, pos []string) error {
	if err := a.client.DeleteAgentToken(a.ctx, rest.DeleteAgentTokenParams{Name: name(pos[0])}); err != nil {
		return err
	}

	a.done(fmt.Sprintf("revoked agent token %s", name(pos[0])), step{"cpctl token ls", "see the remaining tokens"})

	return nil
}
