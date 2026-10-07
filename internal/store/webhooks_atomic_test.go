package store

import (
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
	if err = st.CreateWebhook(ctx, webhook.Config{Name: "test", Events: []string{"*"}, Origin: "both", Enabled: true}); err != nil {
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
