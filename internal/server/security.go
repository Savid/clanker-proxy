package server

import (
	"context"
	"errors"
	"net/http"
	"strings"

	"github.com/savid/clanker-proxy/api/rest"
	"github.com/savid/clanker-proxy/internal/inbox"
)

// errUnauthorized is every authentication failure: which part was wrong is
// not the caller's business.
var errUnauthorized = errors.New("missing or invalid bearer token")

// security implements the spec's security schemes for the generated server,
// which calls it before decoding the request.
type security struct {
	inbox      Inbox
	ownerToken string
}

var _ rest.SecurityHandler = (*security)(nil)

type peerKey struct{}

// HandleOwnerToken admits the owner.
func (s *security) HandleOwnerToken(ctx context.Context, _ rest.OperationName, t rest.OwnerToken) (context.Context, error) {
	if !s.isOwner(t.Token) {
		return ctx, errUnauthorized
	}

	return ctx, nil
}

// HandlePeerSecret admits a peer and records which one in the context.
func (s *security) HandlePeerSecret(ctx context.Context, _ rest.OperationName, t rest.PeerSecret) (context.Context, error) {
	if !strings.HasPrefix(t.Token, inbox.PeerPrefix) {
		return ctx, errUnauthorized
	}

	p, err := s.inbox.PeerBySecret(ctx, t.Token)
	if err != nil {
		if _, isCaller := inbox.HTTPStatus(err); isCaller {
			return ctx, errUnauthorized
		}

		return ctx, err
	}

	return context.WithValue(ctx, peerKey{}, p.Name), nil
}

func (s *security) isOwner(token string) bool {
	return strings.HasPrefix(token, inbox.OwnerPrefix) && inbox.Equal(token, s.ownerToken)
}

// peerFrom is the peer HandlePeerSecret admitted to this operation.
func peerFrom(ctx context.Context) string {
	p, _ := ctx.Value(peerKey{}).(string)

	return p
}

// bearer is the request's bearer token, for hand-routed operations.
func bearer(r *http.Request) string {
	scheme, token, ok := strings.Cut(r.Header.Get("Authorization"), " ")
	if !ok || !strings.EqualFold(scheme, "Bearer") {
		return ""
	}

	return strings.TrimSpace(token)
}

// hints adds the WWW-Authenticate header every 401 calls for (RFC 6750 §3),
// which NewError cannot set.
func hints(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		next.ServeHTTP(&hintWriter{ResponseWriter: w}, r)
	})
}

type hintWriter struct {
	http.ResponseWriter
}

func (w *hintWriter) WriteHeader(code int) {
	if code == http.StatusUnauthorized {
		w.Header().Set("WWW-Authenticate", `Bearer realm="clanker-proxy"`)
	}

	w.ResponseWriter.WriteHeader(code)
}

// Unwrap lets http.ResponseController reach the writer beneath, which the
// event stream needs.
func (w *hintWriter) Unwrap() http.ResponseWriter { return w.ResponseWriter }
