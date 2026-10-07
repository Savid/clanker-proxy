package server_test

import (
	"bufio"
	"context"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/savid/clanker-proxy/internal/inbox"
	"github.com/savid/clanker-proxy/internal/server"
	"github.com/savid/clanker-proxy/internal/testutil/inboxtest"
)

func TestStreamEndsWhenAgentTokenIsRevoked(t *testing.T) {
	t.Parallel()
	ib := inboxtest.New(t)
	ib.ActivePeer(t, "friend")
	agent, _, err := ib.CreateAgentToken(t.Context(), "helper", nil, time.Time{})
	if err != nil {
		t.Fatal(err)
	}
	s, err := server.New(slog.New(slog.DiscardHandler), ib.Inbox, server.Config{OwnerToken: inbox.NewSecret(inbox.OwnerPrefix), Version: "test"})
	if err != nil {
		t.Fatal(err)
	}
	srv := httptest.NewServer(s)
	t.Cleanup(func() { s.Shutdown(); srv.Close() })

	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, srv.URL+"/api/v1/stream", nil)
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("Authorization", "Bearer "+agent)
	resp, err := http.DefaultClient.Do(req)
	if err != nil || resp.StatusCode != http.StatusOK {
		t.Fatalf("stream: %v %v", resp, err)
	}
	defer resp.Body.Close()
	lines := bufio.NewScanner(resp.Body)
	next := func() []string {
		var got []string
		for lines.Scan() {
			got = append(got, lines.Text())
			if strings.HasPrefix(lines.Text(), "data:") {
				return got
			}
		}
		return got
	}

	if _, err = ib.Open(t.Context(), inbox.NewThread{To: "friend", Title: "before revoking"}); err != nil {
		t.Fatal(err)
	}
	if got := strings.Join(next(), "\n"); !strings.Contains(got, "before revoking") {
		t.Fatalf("no event while the token is valid: %q", got)
	}

	if err = ib.DeleteAgentToken(t.Context(), "helper"); err != nil {
		t.Fatal(err)
	}
	if _, err = ib.Open(t.Context(), inbox.NewThread{To: "friend", Title: "after revoking"}); err != nil {
		t.Fatal(err)
	}
	if got := strings.Join(next(), "\n"); strings.Contains(got, "after revoking") || ctx.Err() != nil {
		t.Fatalf("revoked token kept streaming: %q", got)
	}
}

func TestStreamKeepsToAgentScope(t *testing.T) {
	t.Parallel()
	ib := inboxtest.New(t)
	ib.ActivePeer(t, "friend")
	ib.ActivePeer(t, "other")
	agent, _, err := ib.CreateAgentToken(t.Context(), "helper", []string{"friend"}, time.Time{})
	if err != nil {
		t.Fatal(err)
	}
	s, err := server.New(slog.New(slog.DiscardHandler), ib.Inbox, server.Config{OwnerToken: inbox.NewSecret(inbox.OwnerPrefix), Version: "test"})
	if err != nil {
		t.Fatal(err)
	}
	srv := httptest.NewServer(s)
	t.Cleanup(func() { s.Shutdown(); srv.Close() })

	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, srv.URL+"/api/v1/stream", nil)
	if err != nil {
		t.Fatal(err)
	}
	req.Header.Set("Authorization", "Bearer "+agent)
	resp, err := http.DefaultClient.Do(req)
	if err != nil || resp.StatusCode != http.StatusOK {
		t.Fatalf("stream: %v %v", resp, err)
	}
	defer resp.Body.Close()

	for _, to := range []string{"other", "friend"} {
		if _, err = ib.Open(t.Context(), inbox.NewThread{To: to, Title: "to " + to}); err != nil {
			t.Fatal(err)
		}
	}
	lines := bufio.NewScanner(resp.Body)
	for lines.Scan() {
		if data, ok := strings.CutPrefix(lines.Text(), "data:"); ok {
			if !strings.Contains(data, `"to friend"`) {
				t.Fatalf("first event outside the scope: %s", data)
			}
			return
		}
	}
	t.Fatal("no event in scope")
}
