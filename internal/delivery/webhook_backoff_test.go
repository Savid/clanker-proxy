package delivery_test

import (
	"net/http"
	"net/http/httptest"
	"sync/atomic"
	"testing"
	"time"

	"github.com/savid/clanker-proxy/internal/store"
	"github.com/savid/clanker-proxy/pkg/webhook"
)

func hookState(t *testing.T, st *store.Store) webhook.Config {
	t.Helper()
	c, err := st.Webhook(t.Context(), "agent")
	if err != nil {
		t.Fatal(err)
	}
	return c
}

func TestWebhookFailingEndpointIsProbedOnce(t *testing.T) {
	t.Parallel()
	st := webhookStore(t)
	var calls atomic.Int32
	var status atomic.Int32
	status.Store(http.StatusInternalServerError)
	receiver := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		calls.Add(1)
		w.WriteHeader(int(status.Load()))
	}))
	defer receiver.Close()
	addHook(t, st, "agent", receiver.URL)
	queue(t, st, 3)
	drainHooks(t, st, t0)
	drainHooks(t, st, t0)
	if calls.Load() != 1 {
		t.Fatalf("calls = %d; queued deliveries bypassed the failing endpoint's backoff", calls.Load())
	}
	h := hookState(t, st)
	if wait := h.PausedUntil.Sub(t0); h.Failures != 1 || wait < 15*time.Second || wait > 19*time.Second {
		t.Fatalf("failures=%d pause=%s", h.Failures, wait)
	}
	at := h.PausedUntil
	drainHooks(t, st, at)
	h = hookState(t, st)
	if wait := h.PausedUntil.Sub(at); calls.Load() != 2 || h.Failures != 2 || wait < 30*time.Second || wait > 38*time.Second {
		t.Fatalf("calls=%d failures=%d pause=%s; endpoint backoff did not grow", calls.Load(), h.Failures, wait)
	}
	status.Store(http.StatusNoContent)
	drainHooks(t, st, h.PausedUntil)
	if h = hookState(t, st); h.Failures != 0 {
		t.Fatal("a delivery did not reset the endpoint's failures")
	}
}

func TestWebhookLongRetryAfterIsCappedAndCleared(t *testing.T) {
	t.Parallel()
	st := webhookStore(t)
	var calls atomic.Int32
	receiver := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		if calls.Add(1) == 1 {
			w.Header().Set("Retry-After", "604800")
			w.WriteHeader(http.StatusServiceUnavailable)
			return
		}
		w.WriteHeader(http.StatusNoContent)
	}))
	defer receiver.Close()
	addHook(t, st, "agent", receiver.URL)
	queue(t, st, 2)
	drainHooks(t, st, t0)
	for _, d := range history(t, st, "agent") {
		if d.Status != "pending" {
			t.Fatalf("a long Retry-After ended delivery %s as %s", d.ID, d.Status)
		}
	}
	h := hookState(t, st)
	if !h.PausedUntil.Equal(t0.Add(time.Hour)) {
		t.Fatalf("paused until %s, want an hour", h.PausedUntil)
	}
	// Re-enabling (any owner update) is how an operator resumes a paused endpoint.
	if _, err := st.UpdateWebhook(t.Context(), h.Name, webhook.Update{Enabled: new(true)}); err != nil {
		t.Fatal(err)
	}
	if h = hookState(t, st); !h.PausedUntil.IsZero() || h.Failures != 0 {
		t.Fatal("update did not clear the endpoint backoff")
	}
	drainHooks(t, st, t0)
	if calls.Load() != 2 {
		t.Fatal("endpoint did not resume after update")
	}
}
