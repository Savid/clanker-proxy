package inbox_test

import (
	"encoding/base64"
	"testing"
	"time"

	"github.com/savid/clanker-proxy/internal/store"
	"github.com/savid/clanker-proxy/internal/testutil/inboxtest"
	"github.com/savid/clanker-proxy/pkg/thread"
	"github.com/savid/clanker-proxy/pkg/webhook"
)

func TestWebhookReceiveDeduplicatesAndFiltersOrigin(t *testing.T) {
	t.Parallel()
	ib := inboxtest.New(t)
	ib.ActivePeer(t, "bob")
	c := webhook.Config{Name: "agent", URL: "https://runner.example", Events: []string{"*"}, Origin: "incoming", Enabled: true, Secret: base64.StdEncoding.EncodeToString(make([]byte, 32))}
	if _, err := ib.CreateWebhook(t.Context(), c); err != nil {
		t.Fatal(err)
	}
	e := thread.Event{ID: "aaaa0000-0000-4000-8000-000000000001", Thread: "aaaa0000-0000-4000-8000-000000000001", Clock: 1, At: time.Now().UTC(), Action: thread.ActionOpen, Title: "test", Kind: thread.KindRequest}
	for i := range 2 {
		r, err := ib.Receive(t.Context(), "bob", e)
		if err != nil || r.Duplicate != (i == 1) {
			t.Fatalf("receive duplicate=%t error=%v", r.Duplicate, err)
		}
	}
	ib.Notify(t.Context(), e.Thread)
	if _, err := ib.Act(t.Context(), e.Thread, thread.ActionAck, ""); err != nil {
		t.Fatal(err)
	}
	ds, err := ib.WebhookDeliveries(t.Context(), "agent", store.WebhookFilter{})
	if err != nil || len(ds) != 1 {
		t.Fatalf("notifications=%d error=%v", len(ds), err)
	}
	e.ID = "aaaa0000-0000-4000-8000-000000000002"
	e.Clock = 3
	e.Action = thread.ActionComment
	e.Title = ""
	e.Kind = ""
	e.Body = "more context"
	if _, err = ib.Receive(t.Context(), "bob", e); err != nil {
		t.Fatal(err)
	}
	ds, err = ib.WebhookDeliveries(t.Context(), "agent", store.WebhookFilter{})
	if err != nil || len(ds) != 2 || ds[0].Event != "thread.reply" {
		t.Fatal("reply did not notify while still owner's turn")
	}
	oldKey := c.Secret
	c.Secret = ""
	c.Enabled = false
	if _, err = ib.UpdateWebhook(t.Context(), c); err != nil {
		t.Fatal(err)
	}
	got, err := ib.Webhook(t.Context(), c.Name)
	if err != nil || got.Secret != oldKey {
		t.Fatal("omitted key was not retained")
	}
}
