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
			name: "token add", args: "<name> -o <file> [-expires <duration>]", minArgs: 1, maxArgs: 1,
			summary: "create an agent token, limited to existing threads",
			about: "An agent token can list, read and act on threads, wait on them and read your name. Peering, " +
				"opening threads, webhooks and tokens refuse it with 403 (exit 5). Give it to an agent as CP_TOKEN " +
				"instead of the owner token. The token is shown only now: -o writes it to a new file readable only " +
				"by you; -o - writes it to stdout, for piping into a secret store. -expires takes a duration such as " +
				"90d or 12h; without it, the token works until cpctl token rm.",
			example: "cpctl token add amp-inbox -o amp.token -expires 90d",
			flags:   tokenAddFlags,
		},
		{
			name: "token ls", summary: "list agent tokens and when they were last used",
			example: "cpctl token ls",
			flags:   func(*flag.FlagSet) func(*app, []string) error { return runTokens },
		},
		{
			name: "token rm", args: "<name>", minArgs: 1, maxArgs: 1,
			summary: "revoke an agent token",
			about:   "It stops working at once.",
			example: "cpctl token rm amp-inbox",
			flags:   func(*flag.FlagSet) func(*app, []string) error { return runTokenRemove },
		},
	}
}

func tokenAddFlags(fs *flag.FlagSet) func(*app, []string) error {
	out := fs.String("o", "", "new `file` for the token, or - for stdout")
	expires := fs.String("expires", "", "lifetime such as 90d or 12h (default: until revoked)")

	return func(a *app, pos []string) error {
		if *out == "" {
			return usageError("-o is required: the token is shown only once")
		}

		req := &rest.AgentTokenInput{Name: name(pos[0])}

		if *expires != "" {
			d, err := parseLifetime(*expires)
			if err != nil {
				return err
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
			closeOut(false)

			return err
		}

		if _, err = fmt.Fprintln(w, t.Token); err != nil {
			closeOut(false)
			// A token nobody holds is only a risk.
			_ = a.client.DeleteAgentToken(a.ctx, rest.DeleteAgentTokenParams{Name: t.Name})

			return fmt.Errorf("write token: %w", err)
		}

		closeOut(true)

		// Stdout holds only the token, so it can be piped.
		if *out == "-" {
			return nil
		}

		created := &rest.AgentTokenSummary{Name: t.Name, CreatedAt: t.CreatedAt, ExpiresAt: t.ExpiresAt}

		return a.print(created, func(w io.Writer) {
			fmt.Fprintf(w, "created agent token %s, %s\nwrote it to %s\n", t.Name, expiry(t.ExpiresAt), *out)
			next(w, step{"CP_TOKEN=\"$(cat " + *out + ")\" cpctl inbox", "what the agent can run with it"},
				step{"cpctl token rm " + string(t.Name), "revoke it"})
		})
	}
}

// tokenOutput opens where the token goes. A file must be new, so an
// existing secret is never overwritten; done(false) removes it again.
func tokenOutput(a *app, path string) (io.Writer, func(ok bool), error) {
	if path == "-" {
		return a.stdout, func(bool) {}, nil
	}

	f, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	switch {
	case errors.Is(err, fs.ErrExist):
		return nil, nil, usageError("%s already exists; choose a new file", path)
	case err != nil:
		return nil, nil, usageError("cannot create %s: %v", path, err)
	}

	return f, func(ok bool) {
		_ = f.Close()
		if !ok {
			_ = os.Remove(path)
		}
	}, nil
}

// parseLifetime reads a Go duration, or whole days as "<n>d".
func parseLifetime(s string) (time.Duration, error) {
	if days, ok := strings.CutSuffix(s, "d"); ok {
		n, err := strconv.Atoi(days)
		if err == nil && n > 0 {
			return time.Duration(n) * 24 * time.Hour, nil
		}
	} else if d, err := time.ParseDuration(s); err == nil && d > 0 {
		return d, nil
	}

	return 0, usageError("-expires must be a positive duration such as 90d or 12h")
}

func expiry(t rest.OptDateTime) string {
	if at, ok := t.Get(); ok {
		return "expires " + at.UTC().Format(time.RFC3339)
	}

	return "never expires"
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

			fmt.Fprintf(tw, "%s\tcreated %s\t%s\t%s\n", t.Name, t.CreatedAt.UTC().Format(time.RFC3339), expiry(t.ExpiresAt), used)
		}

		_ = tw.Flush()

		next(w, step{"cpctl token add <name> -o <file>", "create one for an agent"}, step{"cpctl token rm <name>", "revoke one"})
	})
}

func runTokenRemove(a *app, pos []string) error {
	if err := a.client.DeleteAgentToken(a.ctx, rest.DeleteAgentTokenParams{Name: name(pos[0])}); err != nil {
		return err
	}

	if !a.json {
		fmt.Fprintf(a.stdout, "revoked agent token %s\n", pos[0])
		next(a.stdout, step{"cpctl token ls", "see the remaining tokens"})
	}

	return nil
}
