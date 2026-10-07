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

// tokenSource presents cpctl's token, the owner's or an agent's, under the
// owner scheme: both are bearer tokens and cpd tells them apart by prefix.
// cpctl never calls peer operations.
type tokenSource string

func (s tokenSource) OwnerToken(context.Context, rest.OperationName) (rest.OwnerToken, error) {
	return rest.OwnerToken{Token: string(s)}, nil
}

func (tokenSource) AgentToken(context.Context, rest.OperationName) (rest.AgentToken, error) {
	return rest.AgentToken{}, ogenerrors.ErrSkipClientSecurity
}

func (tokenSource) PeerSecret(context.Context, rest.OperationName) (rest.PeerSecret, error) {
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

var errRedirect = errors.New("cpd redirected the request")

// Owner credentials belong to the configured daemon. Use the same redirect
// policy for ordinary calls and streams so neither can forward them elsewhere.
func ownerHTTPClient(timeout time.Duration) *http.Client {
	return &http.Client{
		Timeout: timeout,
		CheckRedirect: func(*http.Request, []*http.Request) error {
			return errRedirect
		},
	}
}

func newClient(baseURL, token string) (*rest.Client, error) {
	if err := checkTransport(baseURL); err != nil {
		return nil, err
	}

	c, err := rest.NewClient(baseURL, tokenSource(token), rest.WithClient(ownerHTTPClient(requestTimeout)))
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

// classify turns an error into what to report: the exit code, the reason
// in plain words, and how to recover.
func (a *app) classify(err error, cmd *command) *failure {
	if f, ok := errors.AsType[*failure](err); ok {
		if f.exit == exitUsage && f.hint == "" && cmd != nil {
			f.hint = "cpctl help " + cmd.name
		}

		return f
	}

	if errors.Is(err, errRedirect) {
		return &failure{
			exit: exitUsage, msg: errRedirect.Error(),
			hint: "set CP_URL (or -url) to cpd's direct URL; redirects are refused",
		}
	}

	if errors.Is(err, errTimeout) {
		return &failure{exit: exitTimeout, msg: err.Error(), hint: "nothing changed in time; run the same wait again to keep waiting"}
	}

	if p, ok := errors.AsType[*rest.ProblemStatusCode](err); ok {
		return problemFailure(p, cmd, a.ref)
	}

	if errors.Is(err, syscall.ECONNREFUSED) || isDialError(err) {
		return &failure{
			exit: exitUnreachable, msg: fmt.Sprintf("cannot reach cpd at %s: %v", a.url, err),
			hint: "start cpd, or set CP_URL (or -url) to where it listens",
		}
	}

	return &failure{exit: exitError, msg: err.Error()}
}

func problemFailure(p *rest.ProblemStatusCode, cmd *command, ref string) *failure {
	f := &failure{exit: exitError, status: p.StatusCode, msg: p.Response.Detail.Or(p.Response.Title)}

	name := ""
	if cmd != nil {
		name = cmd.name
	}

	switch p.StatusCode {
	case http.StatusBadRequest:
		f.exit, f.hint = exitUsage, "cpctl help "+name
	case http.StatusUnauthorized:
		f.exit, f.hint = exitAuth, "set CP_TOKEN (or -token) to the owner.token in cpd's data directory (CP_DIR, else ~/.cp), or to an agent token from cpctl token add"
	case http.StatusForbidden:
		f.exit, f.hint = exitRefused, "CP_TOKEN is an agent token: it lists, reads and acts on threads; ask the owner to run this"
	case http.StatusNotFound:
		f.exit, f.hint = exitNotFound, notFoundHint(name)
	case http.StatusConflict, http.StatusUnprocessableEntity:
		f.exit = exitRefused
		switch {
		case name == "webhook add":
			f.hint = "choose another name, or change the existing one with cpctl webhook set <name>"
		case name == "token add":
			f.hint = "choose another name, or revoke the existing one with cpctl token rm <name>"
		case name == "webhook retry":
			f.hint = "only failed deliveries of enabled webhooks can be retried; cpctl webhook deliveries <name> -status failed lists them, and cpctl webhook set <name> -enabled=true resumes a paused webhook"
		case ref != "":
			f.hint = "cpctl show " + ref + " lists what you can do now, as commands"
		}
	case http.StatusBadGateway:
		f.hint = "the other daemon did not answer; check its URL and that its cpd is running, then try again"
	}

	return f
}

func notFoundHint(cmd string) string {
	if strings.HasPrefix(cmd, "token ") {
		return "cpctl token ls lists agent tokens"
	}
	if strings.HasPrefix(cmd, "webhook ") {
		return "cpctl webhook ls lists webhook names; cpctl webhook deliveries <name> lists recent delivery IDs"
	}
	switch cmd {
	case "approve", "deny":
		return "cpctl requests lists pending requests and their IDs"
	case "send", "peer show", "peer rm":
		return "cpctl peer ls lists peers; cpctl peer add <name> <url> connects someone new"
	default:
		return "cpctl ls lists threads; a <ref> is a thread ID or a unique prefix of it"
	}
}

func isDialError(err error) bool {
	var op *net.OpError

	return errors.As(err, &op) && op.Op == "dial"
}
