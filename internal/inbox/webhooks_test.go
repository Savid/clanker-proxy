package inbox_test

import (
	"encoding/base64"
	"errors"
	"testing"
	"time"

	"github.com/savid/clanker-proxy/internal/inbox"
	"github.com/savid/clanker-proxy/internal/testutil/inboxtest"
	"github.com/savid/clanker-proxy/pkg/thread"
	"github.com/savid/clanker-proxy/pkg/webhook"
)

func TestWebhookReceiveDeduplicatesAndFiltersOrigin(t *testing.T) {
	t.Parallel()
	ib := inboxtest.New(t)
	ib.ActivePeer(t, "bob")
	c := webhook.Config{Type: "generic", Name: "agent", URL: "https://runner.example", Events: []string{"*"}, Origin: "incoming", Enabled: true, Secret: base64.StdEncoding.EncodeToString(make([]byte, 32))}
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
	ds, _, err := ib.WebhookDeliveries(t.Context(), "agent", "", 100, "")
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
	ds, _, err = ib.WebhookDeliveries(t.Context(), "agent", "", 100, "")
	if err != nil || len(ds) != 2 || ds[0].Event != "thread.reply" {
		t.Fatal("reply did not notify while still owner's turn")
	}
	oldKey := c.Secret
	if _, err = ib.UpdateWebhook(t.Context(), c.Name, webhook.Update{Enabled: new(false)}); err != nil {
		t.Fatal(err)
	}
	got, err := ib.Webhook(t.Context(), c.Name)
	if err != nil || got.Secret != oldKey {
		t.Fatal("omitted key was not retained")
	}
}

func TestConcurrentWebhookUpdatesPreserveOmittedSettings(t *testing.T) {
	t.Parallel()
	ib := inboxtest.New(t)
	key := base64.StdEncoding.EncodeToString(make([]byte, 32))
	replacement := base64.StdEncoding.EncodeToString([]byte("0123456789abcdef0123456789abcdef"))
	c := webhook.Config{Name: "notify", Type: "generic", URL: "https://runner.example", Origin: "incoming", Events: []string{"*"}, Secret: key}
	if _, err := ib.CreateWebhook(t.Context(), c); err != nil {
		t.Fatal(err)
	}
	for range 32 {
		if _, err := ib.UpdateWebhook(t.Context(), c.Name, webhook.Update{Origin: new("incoming"), Enabled: new(false), Secret: &key}); err != nil {
			t.Fatal(err)
		}
		start := make(chan struct{})
		results := make(chan error, 2)
		for _, update := range []webhook.Update{{Origin: new("both"), Enabled: new(true)}, {Secret: &replacement}} {
			go func() {
				<-start
				_, err := ib.UpdateWebhook(t.Context(), c.Name, update)
				results <- err
			}()
		}
		close(start)
		for range 2 {
			if err := <-results; err != nil {
				t.Fatal(err)
			}
		}
		got, err := ib.Webhook(t.Context(), c.Name)
		if err != nil || got.Secret != replacement || got.Origin != "both" || !got.Enabled {
			t.Fatal("concurrent partial update overwrote omitted credentials or filters")
		}
	}
}

func TestConcurrentWebhookReplacementKeepsProviderCredentials(t *testing.T) {
	t.Parallel()
	ib := inboxtest.New(t)
	key := base64.StdEncoding.EncodeToString(make([]byte, 32))
	old := webhook.Config{Name: "notify", Type: "generic", URL: "https://runner.example", Origin: "incoming", Events: []string{"*"}, Secret: key}
	replacement := webhook.Config{Name: "notify", Type: "discord", URL: "https://discord.com/api/webhooks/id/token", Origin: "both", Events: []string{"*"}, Headers: []webhook.Header{{Name: "Authorization", Value: "Bearer private"}}}
	for range 32 {
		if _, err := ib.CreateWebhook(t.Context(), old); err != nil {
			t.Fatal(err)
		}
		start := make(chan struct{})
		result := make(chan error, 1)
		go func() {
			<-start
			_, err := ib.UpdateWebhook(t.Context(), old.Name, webhook.Update{Enabled: new(true)})
			result <- err
		}()
		close(start)
		if err := ib.DeleteWebhook(t.Context(), old.Name); err != nil {
			t.Fatal(err)
		}
		if _, err := ib.CreateWebhook(t.Context(), replacement); err != nil {
			t.Fatal(err)
		}
		if err := <-result; err != nil {
			if e, ok := errors.AsType[*inbox.Error](err); !ok || e.Kind != inbox.KindNotFound {
				t.Fatal(err)
			}
		}
		got, err := ib.Webhook(t.Context(), old.Name)
		if err != nil || got.Type != replacement.Type || got.URL != replacement.URL || got.Secret != "" || len(got.Headers) != 1 {
			t.Fatal("concurrent patch corrupted a recreated destination's credentials")
		}
		if err = ib.DeleteWebhook(t.Context(), old.Name); err != nil {
			t.Fatal(err)
		}
	}
}
