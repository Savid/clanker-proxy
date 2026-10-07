package delivery_test

import (
	"bytes"
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strconv"
	"sync/atomic"
	"testing"
	"time"

	"github.com/savid/clanker-proxy/internal/delivery"
	"github.com/savid/clanker-proxy/internal/store"
	"github.com/savid/clanker-proxy/pkg/webhook"
)

type webhookAttempt struct {
	id, timestamp, signature string
	body                     []byte
}

func webhookStore(t *testing.T) *store.Store {
	t.Helper()
	st, err := store.Open(t.Context(), filepath.Join(t.TempDir(), "cp.db"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = st.Close() })
	if err = st.AddPeer(t.Context(), store.Peer{Name: "bob", URL: "http://bob", Secret: "cpp_bob", Status: store.PeerActive, AddedAt: t0}); err != nil {
		t.Fatal(err)
	}
	return st
}

func addHook(t *testing.T, st *store.Store, name, url string) {
	t.Helper()
	err := st.CreateWebhook(t.Context(), webhook.Config{Type: "generic", Name: name, URL: url, Secret: base64.StdEncoding.EncodeToString(make([]byte, 32)), Events: []string{"*"}, Origin: "outgoing", Enabled: true})
	if err != nil {
		t.Fatal(err)
	}
}

func history(t *testing.T, st *store.Store, name string) []store.WebhookDelivery {
	t.Helper()
	ds, err := st.WebhookDeliveries(t.Context(), name, store.WebhookFilter{})
	if err != nil {
		t.Fatal(err)
	}
	return ds
}

func drainHooks(t *testing.T, st *store.Store, now time.Time) {
	t.Helper()
	d := delivery.NewWebhooks(slog.New(slog.DiscardHandler), st, delivery.Config{Now: func() time.Time { return now }})
	if err := d.Drain(t.Context()); err != nil {
		t.Fatal(err)
	}
}

func TestWebhookRetriesAndSignatures(t *testing.T) {
	t.Parallel()
	st := webhookStore(t)
	attempts := make(chan webhookAttempt, 10)
	var status atomic.Int32
	status.Store(http.StatusServiceUnavailable)
	receiver := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, err := io.ReadAll(r.Body)
		if err != nil {
			t.Error(err)
		}
		if r.Method != http.MethodPost || r.Header.Get("Content-Type") != "application/json" || r.Header.Get("Authorization") != "" {
			t.Error("wrong method or headers")
		}
		attempts <- webhookAttempt{r.Header.Get("Webhook-Id"), r.Header.Get("Webhook-Timestamp"), r.Header.Get("Webhook-Signature"), body}
		w.WriteHeader(int(status.Load()))
	}))
	defer receiver.Close()
	addHook(t, st, "agent", receiver.URL)
	queue(t, st, 1)
	drainHooks(t, st, t0)
	first := <-attempts
	mac := hmac.New(sha256.New, make([]byte, 32))
	_, _ = mac.Write(append([]byte(first.id+"."+first.timestamp+"."), first.body...))
	if first.signature != "v1,"+base64.StdEncoding.EncodeToString(mac.Sum(nil)) || first.timestamp != strconv.FormatInt(t0.Unix(), 10) {
		t.Fatal("signature verification failed")
	}
	ds := history(t, st, "agent")
	if ds[0].Status != "pending" || ds[0].Attempts != 1 || (ds[0].NextAttemptAt.Before(t0.Add(15*time.Second)) || ds[0].NextAttemptAt.After(t0.Add(19*time.Second))) {
		t.Fatal("incorrect backoff")
	}
	drainHooks(t, st, t0)
	if len(attempts) != 0 {
		t.Fatal("retried before deadline")
	}
	status.Store(http.StatusNoContent)
	// A new worker resumes the database queue without in-memory state.
	drainHooks(t, st, ds[0].NextAttemptAt)
	second := <-attempts
	if second.id != first.id || !bytes.Equal(second.body, first.body) || second.timestamp == first.timestamp {
		t.Fatal("retry identity or timestamp is wrong")
	}
	ds = history(t, st, "agent")
	if ds[0].Status != "delivered" || ds[0].Attempts != 2 {
		t.Fatal("delivery did not complete")
	}
}

func TestWebhookEndpointsAreIndependent(t *testing.T) {
	t.Parallel()
	st := webhookStore(t)
	started := make(chan struct{})
	release := make(chan struct{})
	fast := make(chan struct{}, 1)
	slow := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		close(started)
		<-release
		w.WriteHeader(http.StatusServiceUnavailable)
	}))
	defer slow.Close()
	quick := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { fast <- struct{}{}; w.WriteHeader(http.StatusNoContent) }))
	defer quick.Close()
	addHook(t, st, "slow", slow.URL)
	addHook(t, st, "quick", quick.URL)
	queue(t, st, 1)
	d := delivery.NewWebhooks(slog.New(slog.DiscardHandler), st, delivery.Config{Now: func() time.Time { return t0 }})
	done := make(chan error, 1)
	go func() { done <- d.Drain(t.Context()) }()
	<-started
	waitCtx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	defer cancel()
	select {
	case <-fast:
	case <-waitCtx.Done():
		t.Error("fast endpoint blocked by slow endpoint")
	}
	close(release)
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	if history(t, st, "slow")[0].Status != "pending" || history(t, st, "quick")[0].Status != "delivered" {
		t.Fatal("endpoint statuses are not independent")
	}
}

func TestWebhookHTTPFailures(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		status int
		want   string
	}{{400, "failed"}, {401, "failed"}, {404, "failed"}, {410, "failed"}, {422, "failed"}, {408, "pending"}, {429, "pending"}, {500, "pending"}} {
		t.Run(strconv.Itoa(tc.status), func(t *testing.T) {
			t.Parallel()
			st := webhookStore(t)
			receiver := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
				w.WriteHeader(tc.status)
				_, _ = w.Write([]byte("secret response must not appear in history"))
			}))
			defer receiver.Close()
			addHook(t, st, "agent", receiver.URL)
			queue(t, st, 1)
			drainHooks(t, st, t0)
			d := history(t, st, "agent")[0]
			if d.Status != tc.want || d.LastError != "HTTP "+strconv.Itoa(tc.status) {
				t.Fatalf("status=%s error=%s", d.Status, d.LastError)
			}
		})
	}
}

func TestWebhookRefusesRedirect(t *testing.T) {
	t.Parallel()
	st := webhookStore(t)
	var forwarded atomic.Bool
	target := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) { forwarded.Store(true) }))
	defer target.Close()
	redirect := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Redirect(w, r, target.URL, http.StatusTemporaryRedirect)
	}))
	defer redirect.Close()
	addHook(t, st, "agent", redirect.URL)
	queue(t, st, 1)
	drainHooks(t, st, t0)
	if forwarded.Load() || history(t, st, "agent")[0].Status != "failed" {
		t.Fatal("redirect was followed or retried")
	}
}

func TestWebhookCancellationLeavesPending(t *testing.T) {
	t.Parallel()
	st := webhookStore(t)
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	receiver := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { cancel(); w.WriteHeader(http.StatusNoContent) }))
	defer receiver.Close()
	addHook(t, st, "agent", receiver.URL)
	queue(t, st, 1)
	d := delivery.NewWebhooks(slog.New(slog.DiscardHandler), st, delivery.Config{Now: func() time.Time { return t0 }})
	_ = d.Drain(ctx)
	got := history(t, st, "agent")[0]
	if got.Status != "pending" || got.Attempts != 0 {
		t.Fatal("shutdown consumed a delivery attempt")
	}
}

func TestWebhookPendingUsesUpdatedDestination(t *testing.T) {
	t.Parallel()
	st := webhookStore(t)
	attempts := make(chan webhookAttempt, 1)
	receiver := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, err := io.ReadAll(r.Body)
		if err != nil {
			t.Error(err)
		}
		attempts <- webhookAttempt{r.Header.Get("Webhook-Id"), r.Header.Get("Webhook-Timestamp"), r.Header.Get("Webhook-Signature"), body}
		w.WriteHeader(http.StatusNoContent)
	}))
	defer receiver.Close()
	addHook(t, st, "agent", "https://unreachable.example")
	queue(t, st, 1)
	c, err := st.Webhook(t.Context(), "agent")
	if err != nil {
		t.Fatal(err)
	}
	key := bytes.Repeat([]byte{1}, 32)
	c.Secret = base64.StdEncoding.EncodeToString(key)
	c.URL = receiver.URL
	c.Enabled = false
	if err = st.UpdateWebhook(t.Context(), c); err != nil {
		t.Fatal(err)
	}
	drainHooks(t, st, t0)
	if len(attempts) != 0 {
		t.Fatal("paused hook was sent")
	}
	c.Enabled = true
	if err = st.UpdateWebhook(t.Context(), c); err != nil {
		t.Fatal(err)
	}
	drainHooks(t, st, t0)
	a := <-attempts
	mac := hmac.New(sha256.New, key)
	_, _ = mac.Write(append([]byte(a.id+"."+a.timestamp+"."), a.body...))
	if a.signature != "v1,"+base64.StdEncoding.EncodeToString(mac.Sum(nil)) {
		t.Fatal("pending delivery used old signing key")
	}
}

func TestWebhookRunRefillsWhileAnotherEndpointIsSlow(t *testing.T) {
	t.Parallel()
	st := webhookStore(t)
	release := make(chan struct{})
	fast := make(chan struct{}, 4)
	slow := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { <-release; w.WriteHeader(http.StatusNoContent) }))
	defer slow.Close()
	quick := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) { fast <- struct{}{}; w.WriteHeader(http.StatusNoContent) }))
	defer quick.Close()
	addHook(t, st, "slow", slow.URL)
	addHook(t, st, "quick", quick.URL)
	queue(t, st, 2)
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	d := delivery.NewWebhooks(slog.New(slog.DiscardHandler), st, delivery.Config{Now: func() time.Time { return t0 }})
	done := make(chan error, 1)
	go func() { done <- d.Run(ctx) }()
	waitCtx, stop := context.WithTimeout(t.Context(), 5*time.Second)
	defer stop()
	for range 2 {
		select {
		case <-fast:
		case <-waitCtx.Done():
			t.Error("healthy endpoint waited for unrelated slow request")
		}
	}
	cancel()
	close(release)
	if err := <-done; err != nil {
		t.Fatal(err)
	}
}

func TestWebhookRetryAfterPausesEndpoint(t *testing.T) {
	t.Parallel()
	for _, header := range []string{"120", t0.Add(2 * time.Minute).Format(http.TimeFormat)} {
		t.Run(header, func(t *testing.T) {
			testWebhookEndpointPause(t, header)
		})
	}
}

func testWebhookEndpointPause(t *testing.T, header string) {
	t.Helper()
	t.Parallel()
	st := webhookStore(t)
	var calls atomic.Int32
	receiver := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		if calls.Add(1) == 1 {
			w.Header().Set("Retry-After", header)
			w.WriteHeader(http.StatusTooManyRequests)
			return
		}
		w.WriteHeader(http.StatusNoContent)
	}))
	defer receiver.Close()
	addHook(t, st, "agent", receiver.URL)
	queue(t, st, 2)
	drainHooks(t, st, t0)
	ds := history(t, st, "agent")
	if !ds[1].NextAttemptAt.Equal(t0.Add(2 * time.Minute)) {
		t.Fatal("Retry-After was ignored")
	}
	drainHooks(t, st, t0.Add(time.Minute))
	if calls.Load() != 1 {
		t.Fatal("later notification bypassed endpoint backpressure")
	}
	drainHooks(t, st, t0.Add(2*time.Minute))
	drainHooks(t, st, t0.Add(2*time.Minute))
	if calls.Load() != 3 {
		t.Fatal("endpoint did not resume")
	}
	for _, d := range history(t, st, "agent") {
		if d.Status != "delivered" {
			t.Fatal("notification did not complete")
		}
	}
}

func TestWebhookChangedURLIgnoresOldReceiverBackpressure(t *testing.T) {
	t.Parallel()
	st := webhookStore(t)
	var calls atomic.Int32
	replacement := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		calls.Add(1)
		w.WriteHeader(http.StatusNoContent)
	}))
	defer replacement.Close()
	old := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		hook, err := st.Webhook(t.Context(), "agent")
		if err != nil {
			t.Error(err)
			return
		}
		hook.URL = replacement.URL
		if err = st.UpdateWebhook(t.Context(), hook); err != nil {
			t.Error(err)
		}
		w.Header().Set("Retry-After", "3600")
		w.WriteHeader(http.StatusTooManyRequests)
	}))
	defer old.Close()
	addHook(t, st, "agent", old.URL)
	queue(t, st, 2)
	drainHooks(t, st, t0)
	drainHooks(t, st, t0)
	if calls.Load() != 1 {
		t.Fatal("old receiver paused the replacement destination")
	}
}

func TestWebhookBackpressureEndsWithRetryWindow(t *testing.T) {
	t.Parallel()
	st := webhookStore(t)
	var calls atomic.Int32
	receiver := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		if calls.Add(1) == 1 {
			w.WriteHeader(http.StatusTooManyRequests)
			return
		}
		w.WriteHeader(http.StatusNoContent)
	}))
	defer receiver.Close()
	addHook(t, st, "agent", receiver.URL)
	queue(t, st, 2)
	ds := history(t, st, "agent")
	nearExpiry := t0.Add(store.WebhookTTL - time.Second)
	// Give the newer delivery a fresh retry window while the older one is about to expire.
	if err := st.FinishWebhook(t.Context(), ds[0].ID, "failed", "HTTP 400", t0, t0); err != nil {
		t.Fatal(err)
	}
	if err := st.RetryWebhook(t.Context(), "agent", ds[0].ID, nearExpiry); err != nil {
		t.Fatal(err)
	}
	drainHooks(t, st, nearExpiry)
	drainHooks(t, st, t0.Add(store.WebhookTTL))
	if calls.Load() != 2 {
		t.Fatal("expired delivery's backoff held up fresh work")
	}
}
