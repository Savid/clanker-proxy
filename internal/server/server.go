// Package server serves the daemon's one HTTP API. Who may call each
// operation is declared in the spec (owner token, peer secret, or public)
// and checked by the generated server before any handler runs; this package
// supplies the checks (security.go), the handlers (operations.go) and the
// hand-routed event stream (stream.go).
package server

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"strings"
	"time"

	"github.com/ogen-go/ogen/ogenerrors"
	"github.com/ogen-go/ogen/validate"

	"github.com/savid/clanker-proxy/api"
	"github.com/savid/clanker-proxy/api/rest"
	"github.com/savid/clanker-proxy/internal/httpserve"
	"github.com/savid/clanker-proxy/internal/inbox"
	"github.com/savid/clanker-proxy/internal/store"
	"github.com/savid/clanker-proxy/pkg/thread"
	"github.com/savid/clanker-proxy/pkg/webhook"
)

// maxBody bounds request bodies: one thread body plus JSON escaping.
const maxBody = 1 << 20

// Inbox is what the API needs from the daemon's core.
type Inbox interface {
	Webhooks(context.Context) ([]webhook.Config, error)
	Webhook(context.Context, string) (webhook.Config, error)
	CreateWebhook(context.Context, webhook.Config) (webhook.Config, error)
	UpdateWebhook(context.Context, string, webhook.Update) (webhook.Config, error)
	DeleteWebhook(context.Context, string) error
	WebhookDeliveries(ctx context.Context, name, cursor string, limit int, status string) ([]store.WebhookDelivery, string, error)
	RetryWebhook(context.Context, string, string) error
	Self() string
	URL() string
	PeerBySecret(ctx context.Context, secret string) (store.Peer, error)
	RequestReceived(ctx context.Context, req inbox.PeeringRequest) (string, error)
	Requests(ctx context.Context) ([]store.Request, error)
	Approve(ctx context.Context, id, name string) (store.Peer, error)
	Deny(ctx context.Context, id string) error
	Accepted(ctx context.Context, peer string) error
	AddPeer(ctx context.Context, name, url, note string) (store.Peer, error)
	Peer(ctx context.Context, name string) (store.Peer, error)
	Peers(ctx context.Context) ([]store.Peer, error)
	RemovePeer(ctx context.Context, name string) error
	Threads(ctx context.Context, f store.Filter) ([]store.Summary, error)
	Thread(ctx context.Context, ref string) (inbox.View, error)
	Open(ctx context.Context, n inbox.NewThread) (inbox.View, error)
	Act(ctx context.Context, ref string, action thread.Action, body string) (inbox.View, error)
	Receive(ctx context.Context, peer string, e thread.Event) (inbox.Receipt, error)
	Subscribe() (<-chan store.Summary, func())
}

// Config holds what the server needs.
type Config struct {
	// OwnerToken is the owner's bearer token.
	OwnerToken string
	Version    string
}

// Server is the API's handler.
type Server struct {
	log      *slog.Logger
	ops      *operations
	sec      *security
	handler  http.Handler
	shutdown chan struct{}
}

// New builds the route table: the generated server under /api/v1/, the
// stream (which ogen cannot generate) beside it, and the spec.
func New(log *slog.Logger, ib Inbox, cfg Config) (*Server, error) {
	if len(cfg.OwnerToken) < 32 || !strings.HasPrefix(cfg.OwnerToken, inbox.OwnerPrefix) {
		return nil, fmt.Errorf("owner token: want %s and at least 28 more characters", inbox.OwnerPrefix)
	}

	log = httpserve.Logger(log)
	s := &Server{
		log:      log,
		ops:      &operations{log: log, inbox: ib, version: cfg.Version, now: time.Now},
		sec:      &security{inbox: ib, ownerToken: cfg.OwnerToken},
		shutdown: make(chan struct{}),
	}

	generated, err := rest.NewServer(s.ops, s.sec,
		rest.WithErrorHandler(func(ctx context.Context, w http.ResponseWriter, _ *http.Request, err error) {
			// ogen reports a body cut off by the size cap as a decode error.
			if _, tooBig := errors.AsType[*http.MaxBytesError](err); tooBig {
				httpserve.WriteProblem(w, http.StatusRequestEntityTooLarge, fmt.Sprintf("body is over %d bytes", maxBody))

				return
			}

			code := ogenerrors.ErrorCode(err)
			if code >= http.StatusInternalServerError {
				log.ErrorContext(ctx, "request failed", "error", err)
				httpserve.WriteProblem(w, code, "")

				return
			}

			httpserve.WriteProblem(w, code, invalidInput(err))
		}),
		rest.WithNotFound(func(w http.ResponseWriter, _ *http.Request) {
			httpserve.WriteProblem(w, http.StatusNotFound, "no such API route")
		}),
		rest.WithMethodNotAllowed(func(w http.ResponseWriter, _ *http.Request, allowed string) {
			w.Header().Set("Allow", allowed)
			httpserve.WriteProblem(w, http.StatusMethodNotAllowed, "")
		}),
	)
	if err != nil {
		return nil, fmt.Errorf("api: %w", err)
	}

	mux := http.NewServeMux()
	mux.HandleFunc("GET /api/v1/stream", s.streamEvents)
	mux.Handle("/api/v1/", generated)
	mux.HandleFunc("GET /openapi.yaml", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/yaml")
		_, _ = w.Write(api.Spec)
	})
	mux.HandleFunc("/", func(w http.ResponseWriter, _ *http.Request) {
		httpserve.WriteProblem(w, http.StatusNotFound, "")
	})

	s.handler = httpserve.Wrap(log, hints(mux), maxBody)

	return s, nil
}

// ServeHTTP serves the API.
func (s *Server) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	s.handler.ServeHTTP(w, r)
}

// Shutdown ends open streams; pass it to httpserve.Serve.
func (s *Server) Shutdown() {
	close(s.shutdown)
}

// invalidInput names what in a request ogen refused and why, without the
// decoder's call chain.
func invalidInput(err error) string {
	if v, ok := errors.AsType[*validate.Error](err); ok {
		parts := make([]string, 0, len(v.Fields))
		for _, f := range v.Fields {
			parts = append(parts, fmt.Sprintf("invalid %s: %v", f.Name, f.Error))
		}

		return strings.Join(parts, "; ")
	}

	if p, ok := errors.AsType[*ogenerrors.DecodeParamError](err); ok {
		return fmt.Sprintf("invalid %s: %v", p.Name, p.Err)
	}

	if r, ok := errors.AsType[*ogenerrors.DecodeRequestError](err); ok {
		return fmt.Sprintf("invalid request body: %v", r.Err)
	}

	return err.Error()
}
