package inbox

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"strings"
	"time"

	"github.com/savid/clanker-proxy/internal/store"
	"github.com/savid/clanker-proxy/pkg/thread"
)

// agentTouchEvery bounds how often a token's last use is written.
const agentTouchEvery = time.Minute

// tokenHash is what the store keeps of an agent token. The token is 32
// random bytes, so a fast hash is enough.
func tokenHash(token string) string {
	sum := sha256.Sum256([]byte(token))
	return hex.EncodeToString(sum[:])
}

// CreateAgentToken makes an agent token named name that stops working at
// expires, or never when expires is zero. It returns the token, which is
// not kept.
func (b *Inbox) CreateAgentToken(ctx context.Context, name string, peers []string, expires time.Time) (string, store.AgentToken, error) {
	n, ok := thread.NormalizeName(name)
	if !ok {
		return "", store.AgentToken{}, errorf(KindInvalid, "invalid token name %q: use lower-case letters, digits and single hyphens", name)
	}
	scope := make([]string, 0, len(peers))
	for _, p := range peers {
		pn, _ := thread.NormalizeName(p)
		scope = append(scope, pn)
	}
	if err := thread.ValidPeers(scope); err != nil {
		return "", store.AgentToken{}, errorf(KindInvalid, "%v", err)
	}
	if err := b.knownPeers(ctx, scope); err != nil {
		return "", store.AgentToken{}, err
	}
	now := b.now().UTC()
	if !expires.IsZero() && !expires.After(now) {
		return "", store.AgentToken{}, errorf(KindInvalid, "expiresAt must be in the future")
	}
	token := NewSecret(AgentPrefix)
	t := store.AgentToken{Name: n, Hash: tokenHash(token), Peers: scope, CreatedAt: now, ExpiresAt: expires.UTC()}
	err := b.store.AddAgentToken(ctx, t)
	if errors.Is(err, store.ErrExists) {
		return "", t, errorf(KindConflict, "an agent token named %s exists; revoke it or choose another name", n)
	}
	if err != nil {
		return "", t, err
	}
	return token, t, nil
}

// AgentTokens lists agent tokens by name.
func (b *Inbox) AgentTokens(ctx context.Context) ([]store.AgentToken, error) {
	return b.store.AgentTokens(ctx)
}

// DeleteAgentToken revokes an agent token.
func (b *Inbox) DeleteAgentToken(ctx context.Context, name string) error {
	err := b.store.DeleteAgentToken(ctx, name)
	if errors.Is(err, store.ErrNotFound) {
		return errorf(KindNotFound, "no agent token named %s", name)
	}
	return err
}

// AgentByToken returns the agent token this is, if it works now, and
// records the use.
func (b *Inbox) AgentByToken(ctx context.Context, token string) (store.AgentToken, error) {
	if !strings.HasPrefix(token, AgentPrefix) {
		return store.AgentToken{}, errorf(KindUnauthorized, "not an agent token")
	}
	t, err := b.store.AgentTokenByHash(ctx, tokenHash(token))
	if errors.Is(err, store.ErrNotFound) {
		return t, errorf(KindUnauthorized, "unknown agent token")
	}
	if err != nil {
		return t, err
	}
	now := b.now().UTC()
	if !t.ExpiresAt.IsZero() && !now.Before(t.ExpiresAt) {
		return t, errorf(KindUnauthorized, "agent token expired")
	}
	if now.Sub(t.UsedAt) >= agentTouchEvery {
		if err = b.store.TouchAgentToken(ctx, t.Name, now); err != nil {
			b.log.WarnContext(ctx, "agent token use not recorded", "name", t.Name, "error", err)
		}
	}
	return t, nil
}

// knownPeers refuses names that are neither peers nor former peers, so a
// typo does not quietly scope something to nobody.
func (b *Inbox) knownPeers(ctx context.Context, names []string) error {
	for _, n := range names {
		known, err := b.store.KnownPeer(ctx, n)
		if err != nil {
			return err
		}

		if !known {
			return errorf(KindInvalid, "no peer is named %s; cpctl peer ls lists them", n)
		}
	}

	return nil
}
