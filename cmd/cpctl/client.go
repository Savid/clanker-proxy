package main

import (
	"context"
	"errors"
	"fmt"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"time"

	"github.com/ogen-go/ogen/ogenerrors"

	"github.com/savid/clanker-proxy/api/rest"
)

// ownerSource presents the owner token, and nothing for peer operations,
// which cpctl never calls.
type ownerSource string

func (s ownerSource) OwnerToken(context.Context, rest.OperationName) (rest.OwnerToken, error) {
	return rest.OwnerToken{Token: string(s)}, nil
}

func (ownerSource) PeerSecret(context.Context, rest.OperationName) (rest.PeerSecret, error) {
	return rest.PeerSecret{}, ogenerrors.ErrSkipClientSecurity
}

// defaultToken is $CP_TOKEN, else the owner token cpd wrote on its first run
// into $CP_DIR or ~/.cp.
func defaultToken() string {
	if v := os.Getenv("CP_TOKEN"); v != "" {
		return v
	}

	dir := os.Getenv("CP_DIR")
	if dir == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return ""
		}

		dir = filepath.Join(home, ".cp")
	}

	b, err := os.ReadFile(filepath.Join(dir, "owner.token")) //nolint:gosec // the user names their own data directory
	if err != nil {
		return ""
	}

	return strings.TrimSpace(string(b))
}

func defaultURL() string {
	if v := os.Getenv("CP_URL"); v != "" {
		return v
	}

	return "http://127.0.0.1:8080"
}

// requestTimeout bounds every call but the stream.
const requestTimeout = 30 * time.Second

func newClient(baseURL, token string) (*rest.Client, error) {
	if err := checkTransport(baseURL); err != nil {
		return nil, err
	}

	c, err := rest.NewClient(baseURL, ownerSource(token), rest.WithClient(&http.Client{Timeout: requestTimeout}))
	if err != nil {
		return nil, fmt.Errorf("client: %w", err)
	}

	return c, nil
}

// checkTransport refuses to send the owner token in the clear to another
// host: plain http is for loopback only.
func checkTransport(baseURL string) error {
	u, err := url.Parse(baseURL)
	if err != nil {
		return fmt.Errorf("-url: %w", err)
	}

	switch u.Scheme {
	case "https":
		return nil
	case "http":
		host := u.Hostname()
		if ip := net.ParseIP(host); host == "localhost" || (ip != nil && ip.IsLoopback()) {
			return nil
		}

		return fmt.Errorf("-url %s: the owner token would cross the network unencrypted; use https", baseURL)
	default:
		return fmt.Errorf("-url %s: want http(s)://host[:port]", baseURL)
	}
}

// explain turns a client error into something to print: the daemon's reason
// for a problem, or a hint when the daemon is not running or the token is
// wrong.
func explain(err error, baseURL string) error {
	if p, ok := errors.AsType[*rest.ProblemStatusCode](err); ok {
		msg := p.Response.Title
		if p.Response.Detail.Set {
			msg = p.Response.Detail.Value
		}

		if p.StatusCode == http.StatusUnauthorized {
			msg += "; set CP_TOKEN or -token to owner.token from cpd's data directory"
		}

		return fmt.Errorf("%s (%d)", msg, p.StatusCode)
	}

	if errors.Is(err, syscall.ECONNREFUSED) {
		return fmt.Errorf("cannot reach cpd at %s: is it running? (%w)", baseURL, err)
	}

	return err
}
