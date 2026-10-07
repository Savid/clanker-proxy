package server

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"strings"

	"github.com/go-faster/yaml"
	"github.com/ogen-go/ogen/ogenerrors"

	"github.com/savid/clanker-proxy/api/rest"
	"github.com/savid/clanker-proxy/internal/inbox"
)

// errUnauthorized is every authentication failure: which part was wrong is
// not the caller's business.
var errUnauthorized = errors.New("missing or invalid bearer token")

// errAgentForbidden refuses a valid agent token where only the owner may act.
var errAgentForbidden = errors.New("an agent token can only list, read and act on threads, follow the stream and read the owner's name; this needs the owner token")

// security implements the spec's security schemes for the generated server,
// which calls it before decoding the request.
type security struct {
	inbox      Inbox
	ownerToken string
	// agentOps are the operations whose security admits agentToken.
	agentOps map[rest.OperationName]bool
}

var _ rest.SecurityHandler = (*security)(nil)

type peerKey struct{}

// HandleOwnerToken admits the owner. Both schemes are bearer tokens, so it
// also sees agent tokens: it leaves them to HandleAgentToken where the
// operation admits agents, and refuses valid ones with 403 elsewhere.
func (s *security) HandleOwnerToken(ctx context.Context, op rest.OperationName, t rest.OwnerToken) (context.Context, error) {
	if strings.HasPrefix(t.Token, inbox.AgentPrefix) {
		if s.agentOps[op] {
			return ctx, ogenerrors.ErrSkipServerSecurity
		}

		if err := s.agent(ctx, t.Token); err != nil {
			return ctx, err
		}

		return ctx, errAgentForbidden
	}

	if !s.isOwner(t.Token) {
		return ctx, errUnauthorized
	}

	return ctx, nil
}

// HandleAgentToken admits an agent token. HandleOwnerToken has already
// decided on any other token.
func (s *security) HandleAgentToken(ctx context.Context, _ rest.OperationName, t rest.AgentToken) (context.Context, error) {
	if !strings.HasPrefix(t.Token, inbox.AgentPrefix) {
		return ctx, ogenerrors.ErrSkipServerSecurity
	}

	if err := s.agent(ctx, t.Token); err != nil {
		return ctx, err
	}

	return ctx, nil
}

// agent checks an agent token, reporting any caller error as errUnauthorized.
func (s *security) agent(ctx context.Context, token string) error {
	_, err := s.inbox.AgentByToken(ctx, token)
	if _, isCaller := inbox.HTTPStatus(err); err != nil && isCaller {
		return errUnauthorized
	}

	return err
}

// ownerOrAgent admits the owner or an agent, for hand-routed operations
// whose security lists both.
func (s *security) ownerOrAgent(ctx context.Context, token string) (bool, error) {
	if s.isOwner(token) {
		return true, nil
	}

	if !strings.HasPrefix(token, inbox.AgentPrefix) {
		return false, nil
	}

	err := s.agent(ctx, token)
	if errors.Is(err, errUnauthorized) {
		return false, nil
	}

	return err == nil, err
}

// agentOperations reads, from the spec, which operations admit agentToken,
// so the spec stays the access policy. Operation names follow ogen's: the
// operationId with its first letter upper-cased.
func agentOperations(spec []byte) (map[rest.OperationName]bool, error) {
	var doc struct {
		Paths map[string]map[string]any `yaml:"paths"`
	}
	if err := yaml.Unmarshal(spec, &doc); err != nil {
		return nil, fmt.Errorf("read spec: %w", err)
	}

	ops := map[rest.OperationName]bool{}

	for _, item := range doc.Paths {
		for _, raw := range item {
			op, isOp := raw.(map[string]any)
			if !isOp {
				continue
			}

			id, _ := op["operationId"].(string)
			requirements, _ := op["security"].([]any)

			for _, r := range requirements {
				if schemes, _ := r.(map[string]any); schemes != nil && schemes["agentToken"] != nil && id != "" {
					ops[strings.ToUpper(id[:1])+id[1:]] = true
				}
			}
		}
	}

	return ops, nil
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
