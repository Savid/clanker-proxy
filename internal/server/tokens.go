package server

import (
	"context"
	"time"

	"github.com/savid/clanker-proxy/api/rest"
	"github.com/savid/clanker-proxy/internal/store"
)

func (o *operations) CreateAgentToken(ctx context.Context, req *rest.AgentTokenInput) (*rest.NewAgentToken, error) {
	token, t, err := o.inbox.CreateAgentToken(ctx, string(req.Name), peerNames(req.Peers), req.ExpiresAt.Or(time.Time{}))
	if err != nil {
		return nil, err
	}

	return &rest.NewAgentToken{Name: rest.Name(t.Name), Token: token, Peers: toPeers(t.Peers), CreatedAt: t.CreatedAt, ExpiresAt: optTime(t.ExpiresAt)}, nil
}

func (o *operations) ListAgentTokens(ctx context.Context) (*rest.AgentTokenList, error) {
	tokens, err := o.inbox.AgentTokens(ctx)
	if err != nil {
		return nil, err
	}

	out := make([]rest.AgentTokenSummary, 0, len(tokens))
	for _, t := range tokens {
		out = append(out, agentToken(t))
	}

	return &rest.AgentTokenList{Tokens: out}, nil
}

func (o *operations) DeleteAgentToken(ctx context.Context, params rest.DeleteAgentTokenParams) error {
	return o.inbox.DeleteAgentToken(ctx, string(params.Name))
}

func agentToken(t store.AgentToken) rest.AgentTokenSummary {
	return rest.AgentTokenSummary{Name: rest.Name(t.Name), Peers: toPeers(t.Peers), CreatedAt: t.CreatedAt, ExpiresAt: optTime(t.ExpiresAt), UsedAt: optTime(t.UsedAt)}
}

func optTime(t time.Time) rest.OptDateTime {
	if t.IsZero() {
		return rest.OptDateTime{}
	}

	return rest.NewOptDateTime(t)
}
