package delivery_test

import (
	"bytes"
	"encoding/json"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/savid/clanker-proxy/internal/delivery"
	"github.com/savid/clanker-proxy/pkg/webhook"
)

func TestProviderDelivery(t *testing.T) {
	t.Parallel()
	for _, p := range webhook.Providers() {
		t.Run(p.Type, func(t *testing.T) {
			t.Parallel()
			st := webhookStore(t)
			var calls atomic.Int32
			receiver := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				calls.Add(1)
				assertProviderRequest(t, p.Type, r)
				w.WriteHeader(http.StatusNoContent)
			}))
			defer receiver.Close()
			c := webhook.Config{Name: "notify", Type: p.Type, URL: receiver.URL + "/private-path?wait=false&thread_id=123", Origin: "outgoing", Events: []string{"*"}, Enabled: true, Headers: []webhook.Header{{Name: "Authorization", Value: "Bearer test-private"}}}
			if err := c.Validate(); err != nil {
				t.Fatal(err)
			}
			if err := st.CreateWebhook(t.Context(), c); err != nil {
				t.Fatal(err)
			}
			queue(t, st, 1)
			drainHooks(t, st, t0)
			if calls.Load() != 1 || history(t, st, c.Name)[0].Status != "delivered" {
				t.Fatal("provider notification was not delivered")
			}
		})
	}
}

func assertProviderRequest(t *testing.T, kind string, r *http.Request) {
	t.Helper()
	body, err := io.ReadAll(r.Body)
	if err != nil {
		t.Error(err)
	}
	if r.Header.Get("Authorization") != "Bearer test-private" || r.Header.Get("Webhook-Signature") != "" {
		t.Error("wrong authentication or unexpected signing")
	}
	if r.Method != http.MethodPost || r.URL.Path != "/private-path" {
		t.Error("wrong HTTP request")
	}
	if kind == "discord" && (r.URL.Query().Get("wait") != "true" || r.URL.Query().Get("thread_id") != "123") {
		t.Error("Discord must confirm message persistence and retain thread_id")
	}
	if kind == "generic" && r.Header.Get("Webhook-Id") == "" {
		t.Error("unsigned generic delivery needs an ID for deduplication")
	}
	if kind == "ntfy" {
		if r.Header.Get("Content-Type") != "text/plain; charset=utf-8" || r.Header.Get("Title") == "" || !strings.Contains(string(body), "cpctl show") {
			t.Error("incorrect ntfy request")
		}
	} else if !json.Valid(body) || r.Header.Get("Content-Type") != "application/json" {
		t.Error("incorrect JSON request")
	}
}

func TestProviderFailurePolicies(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name, kind, body, want string
		status                 int
		pause                  time.Duration
	}{
		{"discord fractional rate limit", "discord", `{"retry_after":120.5}`, "pending", 429, 121 * time.Second},
		{"discord huge rate limit", "discord", `{"retry_after":1e100}`, "pending", 429, time.Hour},
		{"discord malformed rate limit", "discord", `{"retry_after":"bad"}`, "pending", 429, 0},
		{"slack gone", "slack", `private-url`, "failed", 410, 0},
		{"teams expired", "teams", `private-url`, "failed", 401, 0},
		{"apprise downstream failure", "apprise", `private-url`, "pending", 424, 0},
		{"generic failed dependency", "generic", `private-url`, "failed", 424, 0},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			st := webhookStore(t)
			receiver := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
				w.WriteHeader(tc.status)
				_, _ = io.WriteString(w, tc.body)
			}))
			defer receiver.Close()
			if err := st.CreateWebhook(t.Context(), webhook.Config{Name: "notify", Type: tc.kind, URL: receiver.URL + "/private-url", Origin: "outgoing", Events: []string{"*"}, Enabled: true}); err != nil {
				t.Fatal(err)
			}
			queue(t, st, 1)
			var logs bytes.Buffer
			d := delivery.NewWebhooks(slog.New(slog.NewTextHandler(&logs, nil)), st, delivery.Config{Now: func() time.Time { return t0 }})
			if err := d.Drain(t.Context()); err != nil {
				t.Fatal(err)
			}
			h := history(t, st, "notify")[0]
			if h.Status != tc.want || (tc.pause > 0 && !h.NextAttemptAt.Equal(t0.Add(tc.pause))) {
				t.Fatalf("status=%s next=%s", h.Status, h.NextAttemptAt)
			}
			if strings.Contains(logs.String()+h.LastError, "private-url") || strings.Contains(logs.String(), receiver.URL) {
				t.Fatal("destination or response body exposed")
			}
		})
	}
}

func TestPendingWebhookUsesChangedHeadersAndSigning(t *testing.T) {
	t.Parallel()
	st := webhookStore(t)
	receiver := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") != "Bearer new-value" || r.Header.Get("Webhook-Signature") != "" {
			t.Error("pending delivery used replaced credentials")
		}
		w.WriteHeader(http.StatusNoContent)
	}))
	defer receiver.Close()
	addHook(t, st, "notify", receiver.URL)
	queue(t, st, 1)
	c, err := st.Webhook(t.Context(), "notify")
	if err != nil {
		t.Fatal(err)
	}
	c.Secret = ""
	c.Headers = []webhook.Header{{Name: "Authorization", Value: "Bearer new-value"}}
	if _, err = st.UpdateWebhook(t.Context(), c.Name, webhook.Update{Secret: new(c.Secret), Headers: &c.Headers}); err != nil {
		t.Fatal(err)
	}
	drainHooks(t, st, t0)
	if history(t, st, "notify")[0].Status != "delivered" {
		t.Fatal("pending delivery failed")
	}
}
