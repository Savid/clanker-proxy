// Package store keeps the daemon's state in SQLite: the owner, peers and
// their secrets, pending peering requests, every event, the outbox of events
// to deliver, a projection of each thread for listing, and the URLs of
// removed peers whose threads remain.
package store

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"os"
	"strings"
	"time"

	_ "modernc.org/sqlite" // registers the pure-Go "sqlite" driver
)

// ErrNotFound is returned when a row does not exist.
var ErrNotFound = errors.New("not found")

const schema = `
CREATE TABLE IF NOT EXISTS meta (
	key   TEXT PRIMARY KEY,
	value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS peers (
	name     TEXT PRIMARY KEY,
	url      TEXT NOT NULL,
	secret   TEXT NOT NULL UNIQUE,
	status   TEXT NOT NULL CHECK (status IN ('requested', 'active')),
	added_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS former_peers (
	name TEXT PRIMARY KEY,
	url  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS requests (
	id     TEXT PRIMARY KEY,
	name   TEXT NOT NULL,
	url    TEXT NOT NULL,
	secret TEXT NOT NULL,
	note   TEXT NOT NULL,
	at     TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS events (
	id        TEXT PRIMARY KEY,
	thread    TEXT NOT NULL,
	data      BLOB NOT NULL,
	stored_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS events_thread ON events(thread);
CREATE TABLE IF NOT EXISTS outbox (
	seq             INTEGER PRIMARY KEY AUTOINCREMENT,
	event_id        TEXT NOT NULL UNIQUE REFERENCES events(id),
	peer            TEXT NOT NULL,
	status          TEXT NOT NULL CHECK (status IN ('pending', 'delivered', 'failed')),
	attempts        INTEGER NOT NULL DEFAULT 0,
	next_attempt_at TEXT,
	last_error      TEXT,
	delivered_at    TEXT
);
CREATE INDEX IF NOT EXISTS outbox_due ON outbox(status, next_attempt_at);
CREATE TABLE IF NOT EXISTS threads (
	id         TEXT PRIMARY KEY,
	title      TEXT NOT NULL,
	kind       TEXT NOT NULL,
	labels     TEXT NOT NULL,
	sender     TEXT NOT NULL,
	recipient  TEXT NOT NULL,
	peer       TEXT NOT NULL,
	state      TEXT NOT NULL,
	turn       TEXT NOT NULL,
	opened_at  TEXT NOT NULL,
	updated_at TEXT NOT NULL,
	events     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS threads_updated ON threads(updated_at);
CREATE TABLE IF NOT EXISTS webhooks (
	name        TEXT PRIMARY KEY,
	type        TEXT NOT NULL,
	url         TEXT NOT NULL,
	events      TEXT NOT NULL,
	origin      TEXT NOT NULL,
	enabled     INTEGER NOT NULL,
	secret      TEXT NOT NULL,
	headers     TEXT NOT NULL,
	retry_after TEXT NOT NULL DEFAULT '',
	failures    INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS webhook_deliveries (
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
);
CREATE INDEX IF NOT EXISTS webhook_due ON webhook_deliveries(webhook, status, next_attempt_at, seq);
CREATE INDEX IF NOT EXISTS webhook_expiry ON webhook_deliveries(status, retry_until);
CREATE INDEX IF NOT EXISTS webhook_finished ON webhook_deliveries(finished_at) WHERE status != 'pending';
CREATE INDEX IF NOT EXISTS webhook_peering ON webhook_deliveries(webhook, event, seq);
CREATE INDEX IF NOT EXISTS webhook_history ON webhook_deliveries(webhook, seq);
`

// Store is the daemon's database.
type Store struct {
	db *sql.DB
}

// Open opens or creates the database at path. A new database is readable by
// the owner only, since it holds peer secrets; SQLite gives its journal
// files the same mode.
func Open(ctx context.Context, path string) (*Store, error) {
	f, err := os.OpenFile(path, os.O_RDWR|os.O_CREATE, 0o600)
	if err != nil {
		return nil, fmt.Errorf("open %s: %w", path, err)
	}

	if err = f.Close(); err != nil {
		return nil, fmt.Errorf("open %s: %w", path, err)
	}

	dsn := "file:" + path + "?_pragma=journal_mode(WAL)&_pragma=foreign_keys(1)&_pragma=busy_timeout(5000)&_txlock=immediate"

	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		return nil, fmt.Errorf("open %s: %w", path, err)
	}

	if err = upgradeWebhooks(ctx, db); err != nil {
		_ = db.Close()

		return nil, fmt.Errorf("upgrade webhooks %s: %w", path, err)
	}

	if _, err = db.ExecContext(ctx, schema); err != nil {
		_ = db.Close()

		return nil, fmt.Errorf("create schema %s: %w", path, err)
	}

	return &Store{db: db}, nil
}

// upgradeWebhooks adds destination types and headers to a webhooks table
// created before them, so configured webhooks survive an upgrade. CREATE
// TABLE IF NOT EXISTS keeps an existing table as it is, and every webhook
// query needs these columns. Those webhooks were all signed generic ones.
func upgradeWebhooks(ctx context.Context, db *sql.DB) error {
	var tables, typed int

	err := db.QueryRowContext(ctx, `SELECT
 (SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='webhooks'),
 (SELECT count(*) FROM pragma_table_info('webhooks') WHERE name='type')`).Scan(&tables, &typed)
	if err != nil || tables == 0 || typed == 1 {
		return err
	}

	return (&Store{db: db}).tx(ctx, func(tx *sql.Tx) error {
		_, alterErr := tx.ExecContext(ctx, `ALTER TABLE webhooks ADD COLUMN type TEXT NOT NULL DEFAULT 'generic';
ALTER TABLE webhooks ADD COLUMN headers TEXT NOT NULL DEFAULT '[]'`)

		return alterErr
	})
}

// Close closes the database.
func (s *Store) Close() error {
	return s.db.Close()
}

// Meta returns a stored setting, or "" when unset.
func (s *Store) Meta(ctx context.Context, key string) (string, error) {
	var v string

	err := s.db.QueryRowContext(ctx, `SELECT value FROM meta WHERE key = ?`, key).Scan(&v)
	if errors.Is(err, sql.ErrNoRows) {
		return "", nil
	}

	if err != nil {
		return "", fmt.Errorf("read %s: %w", key, err)
	}

	return v, nil
}

// SetMeta stores a setting.
func (s *Store) SetMeta(ctx context.Context, key, value string) error {
	if _, err := s.db.ExecContext(ctx, `INSERT OR REPLACE INTO meta (key, value) VALUES (?, ?)`, key, value); err != nil {
		return fmt.Errorf("write %s: %w", key, err)
	}

	return nil
}

// tx runs fn in a transaction, committing if it returns nil.
func (s *Store) tx(ctx context.Context, fn func(*sql.Tx) error) error {
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("begin: %w", err)
	}

	if err = fn(tx); err != nil {
		_ = tx.Rollback()

		return err
	}

	if err = tx.Commit(); err != nil {
		return fmt.Errorf("commit: %w", err)
	}

	return nil
}

// timeLayout is fixed-width, so stored times compare correctly as text.
const timeLayout = "2006-01-02T15:04:05.000000000Z"

func formatTime(t time.Time) string {
	return t.UTC().Format(timeLayout)
}

func parseTime(s string) (time.Time, error) {
	t, err := time.Parse(time.RFC3339Nano, s)
	if err != nil {
		return time.Time{}, fmt.Errorf("stored time %q: %w", s, err)
	}

	return t.UTC(), nil
}

func parseNullTime(s sql.NullString) (time.Time, error) {
	if !s.Valid {
		return time.Time{}, nil
	}

	return parseTime(s.String)
}

// isConstraint reports whether err is a SQLite constraint violation.
func isConstraint(err error) bool {
	return err != nil && strings.Contains(err.Error(), "constraint failed")
}
