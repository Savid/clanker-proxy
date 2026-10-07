package store_test

import (
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"path/filepath"
	"testing"
	"time"

	"github.com/savid/clanker-proxy/internal/store"
	"github.com/savid/clanker-proxy/pkg/thread"
	"github.com/savid/clanker-proxy/pkg/webhook"
)

func hook(name, origin string, events ...string) webhook.Config {
	return webhook.Config{Name: name, URL: "https://runner.example/hooks", Events: events, Origin: origin, Enabled: true, Secret: base64.StdEncoding.EncodeToString(make([]byte, 32))}
}

func deliveries(t *testing.T, st *store.Store, name string, n int) []store.WebhookDelivery {
	t.Helper()
	ds, err := st.WebhookDeliveries(t.Context(), name, store.WebhookFilter{})
	if err != nil || len(ds) != n {
		t.Fatalf("%s: count=%d want=%d err=%v", name, len(ds), n, err)
	}
	return ds
}

func TestWebhookFiltersAndPersistence(t *testing.T) {
	t.Parallel()
	path := filepath.Join(t.TempDir(), "cp.db")
	st, err := store.Open(t.Context(), path)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = st.Close() })
	configs := []webhook.Config{hook("all", "both", "*"), hook("incoming", "incoming", "thread.open", "thread.reply"), hook("outgoing", "outgoing", "thread.open"), hook("requests", "incoming", "peering.requested"), hook("paused", "both", "*")}
	configs[4].Enabled = false
	for _, c := range configs {
		if err = st.CreateWebhook(t.Context(), c); err != nil {
			t.Fatal(err)
		}
	}
	if err = st.AddPeer(t.Context(), store.Peer{Name: "bob", URL: "http://bob", Secret: "cpp_bob", Status: store.PeerActive, AddedAt: t0}); err != nil {
		t.Fatal(err)
	}
	addThread(t, st, "aaaa0000-0000-4000-8000-000000000001", "bob", "me", nil, t0)
	addThread(t, st, "aaaa0000-0000-4000-8000-000000000002", "me", "bob", nil, t0)
	e := thread.Event{ID: "aaaa0000-0000-4000-8000-000000000003", Thread: "aaaa0000-0000-4000-8000-000000000001", From: "bob", To: "me", Action: thread.ActionComment, At: t0, Clock: 2, Body: "must not be included"}
	sum, err := st.Summary(t.Context(), e.Thread)
	if err != nil {
		t.Fatal(err)
	}
	if err = st.AddEvent(t.Context(), store.Event{Event: e, StoredAt: t0}, false, sum); err != nil {
		t.Fatal(err)
	}
	if _, err = st.AddRequest(t.Context(), store.Request{ID: "12345678", Name: "carol", URL: "http://carol", Secret: "private", At: t0}, 20, t0.Add(-time.Hour)); err != nil {
		t.Fatal(err)
	}
	deliveries(t, st, "all", 4)
	ds := deliveries(t, st, "incoming", 2)
	deliveries(t, st, "outgoing", 1)
	deliveries(t, st, "requests", 1)
	deliveries(t, st, "paused", 0)
	var payload map[string]any
	if err = json.Unmarshal(ds[0].Payload, &payload); err != nil {
		t.Fatal(err)
	}
	if payload["type"] != "thread.reply" || payload["myTurn"] != true || payload["body"] != nil || payload["secret"] != nil {
		t.Fatalf("unexpected payload fields: %v", payload)
	}
	if err = st.AddEvent(t.Context(), store.Event{Event: e, StoredAt: t0}, false, sum); err == nil {
		t.Fatal("duplicate stored twice")
	}
	deliveries(t, st, "all", 4)
	if err = st.Close(); err != nil {
		t.Fatal(err)
	}
	st, err = store.Open(t.Context(), path)
	if err != nil {
		t.Fatal(err)
	}
	deliveries(t, st, "all", 4)
}

func TestWebhookPauseDeleteAndRetry(t *testing.T) {
	t.Parallel()
	st := open(t)
	ctx := t.Context()
	c := hook("agent", "incoming", "*")
	if err := st.CreateWebhook(ctx, c); err != nil {
		t.Fatal(err)
	}
	addThread(t, st, "aaaa0000-0000-4000-8000-000000000001", "bob", "me", nil, t0)
	d := deliveries(t, st, "agent", 1)[0]
	c.Enabled = false
	if err := st.UpdateWebhook(ctx, c); err != nil {
		t.Fatal(err)
	}
	due, err := st.DueWebhooks(ctx, t0)
	if err != nil || len(due) != 0 {
		t.Fatalf("paused due=%d err=%v", len(due), err)
	}
	addThread(t, st, "aaaa0000-0000-4000-8000-000000000002", "bob", "me", nil, t0)
	deliveries(t, st, "agent", 1)
	if err = st.MaintainWebhooks(ctx, t0.Add(store.WebhookTTL), nil); err != nil {
		t.Fatal(err)
	}
	if err = st.RetryWebhook(ctx, c.Name, d.ID, t0.Add(store.WebhookTTL)); !errors.Is(err, store.ErrWebhookRetry) {
		t.Fatalf("retry disabled: %v", err)
	}
	c.Enabled = true
	if err = st.UpdateWebhook(ctx, c); err != nil {
		t.Fatal(err)
	}
	if err = st.RetryWebhook(ctx, c.Name, d.ID, t0.Add(store.WebhookTTL)); err != nil {
		t.Fatal(err)
	}
	got := deliveries(t, st, "agent", 1)[0]
	if got.Status != "pending" || string(got.Payload) != string(d.Payload) || got.ID != d.ID {
		t.Fatal("retry changed notification identity")
	}
	if err = st.RetryWebhook(ctx, c.Name, d.ID, t0); !errors.Is(err, store.ErrWebhookRetry) {
		t.Fatal("retried pending delivery")
	}
	if err = st.DeleteWebhook(ctx, c.Name); err != nil {
		t.Fatal(err)
	}
	if err = st.CreateWebhook(ctx, c); err != nil {
		t.Fatal(err)
	}
	if _, err = st.WebhookDestination(ctx, d.ID); !errors.Is(err, store.ErrNotFound) {
		t.Fatal("recreated hook can receive deleted notification")
	}
	deliveries(t, st, "agent", 0)
}

func TestWebhookRetention(t *testing.T) {
	t.Parallel()
	st := open(t)
	ctx := t.Context()
	for i := range 32 {
		if err := st.CreateWebhook(ctx, hook(fmt.Sprintf("h%d", i), "incoming", "*")); err != nil {
			t.Fatal(err)
		}
	}
	addThread(t, st, "aaaa0000-0000-4000-8000-000000000001", "bob", "me", nil, t0)
	d := deliveries(t, st, "h0", 1)[0]
	if err := st.FinishWebhook(ctx, d.ID, "delivered", "", t0, t0); err != nil {
		t.Fatal(err)
	}
	if err := st.MaintainWebhooks(ctx, t0.Add(store.WebhookTTL+time.Second), nil); err != nil {
		t.Fatal(err)
	}
	deliveries(t, st, "h0", 0)
	if got := deliveries(t, st, "h1", 1)[0]; got.Status != "failed" {
		t.Fatal("expired pending delivery remained pending")
	}
}

func TestWebhookExpiryWaitsForActiveAttempt(t *testing.T) {
	t.Parallel()
	st := open(t)
	ctx := t.Context()
	if err := st.CreateWebhook(ctx, hook("agent", "incoming", "*")); err != nil {
		t.Fatal(err)
	}
	addThread(t, st, "aaaa0000-0000-4000-8000-000000000001", "bob", "me", nil, t0)
	d := deliveries(t, st, "agent", 1)[0]
	now := t0.Add(store.WebhookTTL)
	if err := st.MaintainWebhooks(ctx, now, []string{d.ID}); err != nil {
		t.Fatal(err)
	}
	if err := st.RetryWebhook(ctx, "agent", d.ID, now); !errors.Is(err, store.ErrWebhookRetry) {
		t.Fatalf("retried active attempt: %v", err)
	}
	if err := st.FinishWebhook(ctx, d.ID, "delivered", "", now, now); err != nil {
		t.Fatal(err)
	}
	if got := deliveries(t, st, "agent", 1)[0]; got.Status != "delivered" || got.Attempts != 1 {
		t.Fatal("successful in-flight response was lost to expiry")
	}
}
