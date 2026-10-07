package delivery

import (
	"context"
	"encoding/base64"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/url"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"github.com/savid/clanker-proxy/internal/store"
	"github.com/savid/clanker-proxy/pkg/webhook"
)

func TestWebhookRetryAfterBounds(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 10, 7, 0, 0, 0, 0, time.UTC)
	until := now.Add(time.Hour)
	for _, tc := range []struct {
		value string
		until time.Time
		want  time.Time
	}{
		{"garbage", until, time.Time{}},
		{"-1", until, time.Time{}},
		{"9223372036854775808", until, time.Time{}},
		{"9223372036854775807", until, until},
		{"0", until, now},
		{"Sun, 06 Nov 1994 08:49:37 GMT", until, time.Time{}},
		{"Wed, 07 Oct 2026 02:00:00 GMT", until, until},
		{"120", now.Add(time.Minute), now.Add(time.Minute)},
		// A receiver cannot pause an endpoint for longer than maxBackoff.
		{"604800", now.Add(7 * 24 * time.Hour), now.Add(maxBackoff)},
		{"Wed, 14 Oct 2026 00:00:00 GMT", now.Add(7 * 24 * time.Hour), now.Add(maxBackoff)},
	} {
		if got := webhookRetryAfter(tc.value, now, tc.until); !got.Equal(tc.want) {
			t.Errorf("%q gave %s, want %s", tc.value, got, tc.want)
		}
	}
	a, b := webhookBackoff("a", 0), webhookBackoff("b", 0)
	if a == b || a < 15*time.Second || a > 19*time.Second || b < 15*time.Second || b > 19*time.Second {
		t.Fatal("retry jitter does not spread bounded attempts")
	}
}

func TestWebhookTransportErrors(t *testing.T) {
	t.Parallel()
	ln, err := new(net.ListenConfig).Listen(t.Context(), "tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	addr := ln.Addr().String()
	_ = ln.Close()
	_, refused := new(net.Dialer).DialContext(t.Context(), "tcp", addr)
	for _, tc := range []struct {
		err  error
		want string
	}{
		{&url.Error{Op: "Post", Err: refused}, "connection refused"},
		{&url.Error{Op: "Post", Err: &net.DNSError{Err: "no such host", IsNotFound: true}}, "DNS lookup failed"},
		{&url.Error{Op: "Post", Err: context.DeadlineExceeded}, "timed out"},
		{fmt.Errorf("wrapped: %w", errors.New("reset")), "connection failed"},
	} {
		if got := transportError(tc.err); got != tc.want {
			t.Errorf("%v gave %q, want %q", tc.err, got, tc.want)
		}
	}
}

func TestWebhookRunHoldsEndpointAfterStoreError(t *testing.T) {
	t.Parallel()
	st, err := store.Open(t.Context(), filepath.Join(t.TempDir(), "cp.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = st.Close() })
	now := time.Date(2026, 10, 7, 0, 0, 0, 0, time.UTC)
	// Nothing listens on port 1, so the released attempt fails fast.
	hook := webhook.Config{Type: "generic", Name: "agent", URL: "http://127.0.0.1:1/", Secret: base64.StdEncoding.EncodeToString(make([]byte, 32)), Events: []string{"*"}, Origin: "both", Enabled: true}
	if err = st.CreateWebhook(t.Context(), hook); err != nil {
		t.Fatal(err)
	}
	if _, err = st.AddRequest(t.Context(), store.Request{ID: "req1", Name: "bob", URL: "http://bob", Secret: "cpp_bob", At: now}, 10, now.Add(-time.Hour)); err != nil {
		t.Fatal(err)
	}
	d := NewWebhooks(slog.New(slog.DiscardHandler), st, Config{Now: func() time.Time { return now }})
	r := &webhookRun{active: map[string]string{}, held: map[string]time.Time{"agent": now.Add(minBackoff)}}
	results := make(chan webhookResult, webhookWorkers)
	var workers sync.WaitGroup
	if err = d.startWebhooks(t.Context(), r, results, &workers); err != nil {
		t.Fatal(err)
	}
	if len(r.active) != 0 {
		t.Fatal("a held endpoint was restarted before its hold ended")
	}
	now = now.Add(minBackoff)
	if err = d.startWebhooks(t.Context(), r, results, &workers); err != nil {
		t.Fatal(err)
	}
	workers.Wait()
	if len(r.active) != 1 || len(r.held) != 0 {
		t.Fatal("endpoint stayed held after its hold ended")
	}
}
