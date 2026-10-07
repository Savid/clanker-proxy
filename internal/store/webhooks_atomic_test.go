package store

import (
	"encoding/json"
	"fmt"
	"path/filepath"
	"testing"
	"time"

	"github.com/savid/clanker-proxy/pkg/thread"
	"github.com/savid/clanker-proxy/pkg/webhook"
)

func TestWebhookQueueFailureRollsBackSource(t *testing.T) {
	t.Parallel()
	ctx := t.Context()
	st, err := Open(ctx, filepath.Join(t.TempDir(), "cp.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()
	if err = st.CreateWebhook(ctx, webhook.Config{Type: "generic", Name: "test", Events: []string{"*"}, Origin: "both", Enabled: true}); err != nil {
		t.Fatal(err)
	}
	if _, err = st.db.ExecContext(ctx, `CREATE TRIGGER reject_webhook BEFORE INSERT ON webhook_deliveries BEGIN SELECT RAISE(ABORT, 'injected write failure'); END`); err != nil {
		t.Fatal(err)
	}
	now := time.Now().UTC()
	id := "aaaa0000-0000-4000-8000-000000000001"
	e := thread.Event{ID: id, Thread: id, From: "bob", To: "alice", Action: thread.ActionOpen, Title: "test", Kind: thread.KindRequest, Clock: 1, At: now}
	th, err := thread.Replay([]thread.Event{e})
	if err != nil {
		t.Fatal(err)
	}
	if err = st.AddEvent(ctx, Event{Event: e, StoredAt: now}, false, Projection(th, "alice")); err == nil {
		t.Fatal("source committed despite queue failure")
	}
	for _, table := range []string{"events", "threads", "webhook_deliveries"} {
		var n int
		if err = st.db.QueryRowContext(ctx, `SELECT count(*) FROM `+table).Scan(&n); err != nil || n != 0 {
			t.Fatalf("%s count=%d err=%v", table, n, err)
		}
	}
	if _, err = st.AddRequest(ctx, Request{ID: "12345678", Name: "bob", At: now}, 20, now.Add(-time.Hour)); err == nil {
		t.Fatal("request committed despite queue failure")
	}
	requests, err := st.Requests(ctx, now.Add(-time.Hour))
	if err != nil || len(requests) != 0 {
		t.Fatal("request survived rollback")
	}
}

func TestPublicPeeringNotificationsAreBounded(t *testing.T) {
	t.Parallel()
	st, err := Open(t.Context(), filepath.Join(t.TempDir(), "cp.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()
	ctx := t.Context()
	for i := range 32 {
		if err = st.CreateWebhook(ctx, webhook.Config{Type: "generic", Name: fmt.Sprintf("h%d", i), URL: "https://runner.example", Origin: "incoming", Events: []string{"*"}, Enabled: true}); err != nil {
			t.Fatal(err)
		}
	}
	t0 := time.Date(2026, 10, 7, 0, 0, 0, 0, time.UTC)
	e := thread.Event{ID: "aaaa0000-0000-4000-8000-000000000001", Thread: "aaaa0000-0000-4000-8000-000000000001", From: "bob", To: "me", Action: thread.ActionOpen, Clock: 1, At: t0, Title: "test", Kind: thread.KindRequest}
	th, err := thread.Replay([]thread.Event{e})
	if err != nil {
		t.Fatal(err)
	}
	if err = st.AddEvent(ctx, Event{Event: e, StoredAt: t0}, false, Projection(th, "me")); err != nil {
		t.Fatal(err)
	}
	floodPeeringRequests(t, st, t0)
	requests, err := st.Requests(ctx, t0.Add(-time.Hour))
	if err != nil || len(requests) != 20 {
		t.Fatal("source request bound changed")
	}
	assertBoundedPeering(t, st)
	due, err := st.DueWebhooks(ctx, t0.Add(time.Hour))
	if err != nil || len(due) != 32 {
		t.Fatal("incorrect fanout")
	}
	for _, d := range due {
		if d.Event != "thread.open" {
			t.Fatal("peering limit deleted the earlier thread notification")
		}
	}
}

func historyHead(t *testing.T, st *Store, name string) WebhookDelivery {
	t.Helper()
	ds, err := st.WebhookDeliveries(t.Context(), name, WebhookFilter{})
	if err != nil || len(ds) == 0 {
		t.Fatal("missing queued delivery")
	}
	return ds[0]
}

func floodPeeringRequests(t *testing.T, st *Store, t0 time.Time) {
	t.Helper()
	ctx := t.Context()
	var err error
	for i := range 150 {
		_, err = st.AddRequest(ctx, Request{ID: fmt.Sprintf("%08x", i), Name: "bob", URL: "http://bob", Secret: "private", At: t0.Add(time.Duration(i) * time.Second)}, 20, t0.Add(-time.Hour))
		if err != nil {
			t.Fatal(err)
		}
		if i < 10 {
			for j := range 32 {
				d := historyHead(t, st, fmt.Sprintf("h%d", j))
				if err = st.FinishWebhook(ctx, d.ID, "delivered", "", t0, t0); err != nil {
					t.Fatal(err)
				}
			}
		}
	}
}

// assertBoundedPeering checks that the flood left each endpoint its delivered
// peering notifications, one pending peering notification for the newest
// request, and the earlier thread notification.
func assertBoundedPeering(t *testing.T, st *Store) {
	t.Helper()
	ctx := t.Context()
	for i := range 32 {
		name := fmt.Sprintf("h%d", i)
		var delivered, pending, threads int
		if err := st.db.QueryRowContext(ctx, `SELECT
 (SELECT count(*) FROM webhook_deliveries WHERE webhook=?1 AND event='peering.requested' AND status='delivered'),
 (SELECT count(*) FROM webhook_deliveries WHERE webhook=?1 AND event='peering.requested' AND status='pending'),
 (SELECT count(*) FROM webhook_deliveries WHERE webhook=?1 AND event='thread.open')`, name).Scan(&delivered, &pending, &threads); err != nil {
			t.Fatal(err)
		}
		if delivered != 10 || pending != 1 || threads != 1 {
			t.Fatalf("%s: delivered=%d pending=%d threads=%d", name, delivered, pending, threads)
		}
		head := historyHead(t, st, name)
		var payload webhook.Payload
		if err := json.Unmarshal(head.Payload, &payload); err != nil {
			t.Fatal(err)
		}
		if head.Status != "pending" || payload.Subject != fmt.Sprintf("%08x", 149) {
			t.Fatalf("%s: pending peering notification is %s %s, want the newest request", name, head.Status, payload.Subject)
		}
	}
}
