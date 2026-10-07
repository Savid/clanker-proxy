package inbox

import (
	"log/slog"
	"net/http"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/savid/clanker-proxy/internal/store"
)

func TestAgentTokens(t *testing.T) {
	t.Parallel()
	ctx := t.Context()
	st, err := store.Open(ctx, filepath.Join(t.TempDir(), "cp.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = st.Close() })
	now := time.Date(2026, 10, 7, 12, 0, 0, 0, time.UTC)
	b := New(slog.New(slog.DiscardHandler), st, nil, Config{Self: "me"})
	b.now = func() time.Time { return now }

	status := func(err error) int {
		code, _ := HTTPStatus(err)
		return code
	}

	if _, _, err = b.CreateAgentToken(ctx, "Bad Name", nil, time.Time{}); status(err) != http.StatusBadRequest {
		t.Fatalf("bad name: %v", err)
	}
	if _, _, err = b.CreateAgentToken(ctx, "late", nil, now); status(err) != http.StatusBadRequest {
		t.Fatalf("expiry not in the future: %v", err)
	}
	forever, created, err := b.CreateAgentToken(ctx, "Amp", nil, time.Time{})
	if err != nil || created.Name != "amp" || !strings.HasPrefix(forever, AgentPrefix) || created.Hash == forever {
		t.Fatalf("create: %+v %v", created, err)
	}
	if _, _, err = b.CreateAgentToken(ctx, "amp", nil, time.Time{}); status(err) != http.StatusConflict {
		t.Fatalf("duplicate name: %v", err)
	}
	brief, _, err := b.CreateAgentToken(ctx, "brief", nil, now.Add(time.Hour))
	if err != nil {
		t.Fatal(err)
	}

	got, err := b.AgentByToken(ctx, forever)
	if err != nil || got.Name != "amp" {
		t.Fatalf("valid token: %+v %v", got, err)
	}
	for _, token := range []string{"", "cpo_" + forever[4:], forever + "x", NewSecret(AgentPrefix)} {
		if _, err = b.AgentByToken(ctx, token); status(err) != http.StatusUnauthorized {
			t.Fatalf("%q admitted: %v", token, err)
		}
	}

	now = now.Add(time.Hour)
	if _, err = b.AgentByToken(ctx, brief); status(err) != http.StatusUnauthorized {
		t.Fatalf("expired token admitted: %v", err)
	}
	if _, _, err = b.CreateAgentToken(ctx, "brief", nil, time.Time{}); err != nil {
		t.Fatalf("an expired token kept its name: %v", err)
	}
	if _, err = b.AgentByToken(ctx, brief); status(err) != http.StatusUnauthorized {
		t.Fatalf("replaced token admitted: %v", err)
	}
	tokens, err := b.AgentTokens(ctx)
	if err != nil || len(tokens) != 2 || tokens[0].Name != "amp" || !tokens[0].UsedAt.Equal(now.Add(-time.Hour)) || tokens[1].Name != "brief" || !tokens[1].UsedAt.IsZero() {
		t.Fatalf("list: %+v %v", tokens, err)
	}

	if err = b.DeleteAgentToken(ctx, "amp"); err != nil {
		t.Fatal(err)
	}
	if _, err = b.AgentByToken(ctx, forever); status(err) != http.StatusUnauthorized {
		t.Fatalf("revoked token admitted: %v", err)
	}
	if err = b.DeleteAgentToken(ctx, "amp"); status(err) != http.StatusNotFound {
		t.Fatalf("revoke twice: %v", err)
	}
}

func TestAgentTokenPeers(t *testing.T) {
	t.Parallel()
	ctx := t.Context()
	st, err := store.Open(ctx, filepath.Join(t.TempDir(), "cp.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = st.Close() })
	b := New(slog.New(slog.DiscardHandler), st, nil, Config{Self: "me"})

	if err = st.AddPeer(ctx, store.Peer{Name: "bob", URL: "http://bob", Secret: "cpp_bob", Status: store.PeerActive, AddedAt: time.Now().UTC()}); err != nil {
		t.Fatal(err)
	}

	for _, peers := range [][]string{{"not a name"}, {"bob", "Bob"}, {"carol"}} {
		if _, _, err = b.CreateAgentToken(ctx, "scoped", peers, time.Time{}); err == nil {
			t.Fatalf("peers %v accepted", peers)
		}
	}
	_, scoped, err := b.CreateAgentToken(ctx, "bob-agent", []string{"Bob"}, time.Time{})
	if err != nil || len(scoped.Peers) != 1 || scoped.Peers[0] != "bob" {
		t.Fatalf("scoped token: %+v %v", scoped, err)
	}
}

func TestPeeringRateWindow(t *testing.T) {
	t.Parallel()
	b := &Inbox{}
	now := time.Date(2026, 10, 7, 12, 0, 0, 0, time.UTC)
	for i := range RequestsPerMinute {
		if !b.allowRequest(now.Add(time.Duration(i) * time.Second)) {
			t.Fatalf("request %d refused", i)
		}
	}
	if b.allowRequest(now.Add(30 * time.Second)) {
		t.Fatal("request past the limit allowed")
	}
	// A minute after the first request, its slot is free again.
	if !b.allowRequest(now.Add(time.Minute)) {
		t.Fatal("request after the window refused")
	}
	if b.allowRequest(now.Add(time.Minute)) {
		t.Fatal("second slot freed too early")
	}
}
