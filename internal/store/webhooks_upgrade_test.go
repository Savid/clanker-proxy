package store

import (
	"database/sql"
	"encoding/base64"
	"path/filepath"
	"testing"
	"time"

	"github.com/savid/clanker-proxy/pkg/webhook"
)

// v012Webhooks is the webhook schema released in v0.1.2.
const v012Webhooks = `
CREATE TABLE webhooks (
	name        TEXT PRIMARY KEY,
	url         TEXT NOT NULL,
	events      TEXT NOT NULL,
	origin      TEXT NOT NULL,
	enabled     INTEGER NOT NULL,
	secret      TEXT NOT NULL,
	retry_after TEXT NOT NULL DEFAULT '',
	failures    INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE webhook_deliveries (
	seq             INTEGER PRIMARY KEY AUTOINCREMENT,
	id              TEXT NOT NULL UNIQUE,
	webhook         TEXT NOT NULL REFERENCES webhooks(name) ON DELETE CASCADE,
	event           TEXT NOT NULL,
	origin          TEXT NOT NULL,
	subject         TEXT NOT NULL,
	payload         BLOB NOT NULL,
	status          TEXT NOT NULL CHECK (status IN ('pending', 'delivered', 'failed')),
	attempts        INTEGER NOT NULL DEFAULT 0,
	created_at      TEXT NOT NULL,
	next_attempt_at TEXT NOT NULL,
	retry_until     TEXT NOT NULL,
	finished_at     TEXT,
	last_error      TEXT NOT NULL DEFAULT ''
);`

func TestOpenKeepsWebhooksFromV012(t *testing.T) {
	t.Parallel()
	path := filepath.Join(t.TempDir(), "cp.db")
	key := base64.StdEncoding.EncodeToString(make([]byte, 32))
	now := time.Date(2026, 10, 7, 0, 0, 0, 0, time.UTC)
	writeV012(t, path, key, now)
	checkUpgraded(t, path, key, now)
	// Opening again must keep the upgraded table and the owner's changes.
	st, err := Open(t.Context(), path)
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()
	got, err := st.Webhook(t.Context(), "agent")
	if err != nil || len(got.Headers) != 1 {
		t.Fatalf("reopened webhook %+v: %v", got, err)
	}
}

// writeV012 stores a signed webhook with one queued delivery, as v0.1.2 did.
func writeV012(t *testing.T, path, key string, now time.Time) {
	t.Helper()
	ctx := t.Context()
	db, err := sql.Open("sqlite", "file:"+path)
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	if _, err = db.ExecContext(ctx, v012Webhooks); err != nil {
		t.Fatal(err)
	}
	if _, err = db.ExecContext(ctx, `INSERT INTO webhooks (name, url, events, origin, enabled, secret) VALUES ('agent', 'https://runner.example/hook', '["*"]', 'both', 1, ?)`, key); err != nil {
		t.Fatal(err)
	}
	if _, err = db.ExecContext(ctx, `INSERT INTO webhook_deliveries (id, webhook, event, origin, subject, payload, status, created_at, next_attempt_at, retry_until)
 VALUES ('aaaa0000-0000-4000-8000-000000000001', 'agent', 'thread.open', 'incoming', 'aaaa0000-0000-4000-8000-000000000002', '{}', 'pending', ?, ?, ?)`,
		formatTime(now), formatTime(now), formatTime(now.Add(WebhookTTL))); err != nil {
		t.Fatal(err)
	}
}

// checkUpgraded opens the database and uses the webhook as delivery and the owner would.
func checkUpgraded(t *testing.T, path, key string, now time.Time) {
	t.Helper()
	ctx := t.Context()
	st, err := Open(ctx, path)
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()
	got, err := st.Webhook(ctx, "agent")
	if err != nil || got.Type != "generic" || got.Secret != key || len(got.Headers) != 0 || got.URL != "https://runner.example/hook" {
		t.Fatalf("upgraded webhook %+v: %v", got, err)
	}
	if err = got.Validate(); err != nil {
		t.Fatalf("upgraded webhook is invalid: %v", err)
	}
	due, err := st.DueWebhooks(ctx, now)
	if err != nil || len(due) != 1 {
		t.Fatalf("queued delivery lost: %d, %v", len(due), err)
	}
	dest, err := st.WebhookDestination(ctx, due[0].ID)
	if err != nil {
		t.Fatal(err)
	}
	// The added default must match what scanning and re-encoding produce,
	// or a failure could never pause the endpoint.
	if err = st.DeferWebhook(ctx, due[0].ID, dest, now.Add(time.Minute)); err != nil {
		t.Fatal(err)
	}
	if got, _ = st.Webhook(ctx, "agent"); got.PausedUntil.IsZero() {
		t.Fatal("upgraded webhook could not be paused")
	}
	headers := []webhook.Header{{Name: "Authorization", Value: "Bearer x"}}
	if _, err = st.UpdateWebhook(ctx, "agent", webhook.Update{Headers: &headers}); err != nil {
		t.Fatal(err)
	}
}
