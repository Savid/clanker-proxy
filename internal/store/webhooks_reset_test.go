package store

import (
	"database/sql"
	"path/filepath"
	"testing"

	"github.com/savid/clanker-proxy/pkg/webhook"
)

func TestOpenResetsUntypedWebhooks(t *testing.T) {
	t.Parallel()
	ctx := t.Context()
	path := filepath.Join(t.TempDir(), "cp.db")
	db, err := sql.Open("sqlite", "file:"+path)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = db.ExecContext(ctx, `CREATE TABLE webhooks (
	name TEXT PRIMARY KEY, url TEXT NOT NULL, events TEXT NOT NULL, origin TEXT NOT NULL,
	enabled INTEGER NOT NULL, secret TEXT NOT NULL,
	retry_after TEXT NOT NULL DEFAULT '', failures INTEGER NOT NULL DEFAULT 0);
CREATE TABLE webhook_deliveries (seq INTEGER PRIMARY KEY AUTOINCREMENT, webhook TEXT NOT NULL REFERENCES webhooks(name));
INSERT INTO webhooks (name, url, events, origin, enabled, secret) VALUES ('old', 'https://runner.example', '["*"]', 'both', 1, '');
INSERT INTO webhook_deliveries (webhook) VALUES ('old');`); err != nil {
		t.Fatal(err)
	}
	if err = db.Close(); err != nil {
		t.Fatal(err)
	}
	st, err := Open(ctx, path)
	if err != nil {
		t.Fatal(err)
	}
	hooks, err := st.Webhooks(ctx)
	if err != nil || len(hooks) != 0 {
		t.Fatalf("hooks=%v err=%v", hooks, err)
	}
	c := webhook.Config{Name: "new", Type: "ntfy", URL: "https://ntfy.example/topic", Events: []string{"*"}, Origin: "both", Enabled: true}
	if err = st.CreateWebhook(ctx, c); err != nil {
		t.Fatal(err)
	}
	if _, err = st.WebhookDeliveries(ctx, "new", WebhookFilter{}); err != nil {
		t.Fatal(err)
	}
	if err = st.Close(); err != nil {
		t.Fatal(err)
	}
	if st, err = Open(ctx, path); err != nil {
		t.Fatal(err)
	}
	defer st.Close()
	if _, err = st.Webhook(ctx, "new"); err != nil {
		t.Fatalf("current webhooks were reset again: %v", err)
	}
}
