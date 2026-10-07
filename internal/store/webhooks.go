package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"github.com/google/uuid"

	"github.com/savid/clanker-proxy/pkg/webhook"
)

// MaxWebhooks bounds fanout per event.
const MaxWebhooks = 32

// WebhookTTL bounds automatic retries and retention of completed deliveries.
const WebhookTTL = 7 * 24 * time.Hour

// ErrWebhookLimit reports that the configured endpoint limit has been reached.
var ErrWebhookLimit = errors.New("webhook limit reached")

// ErrWebhookRetry means a delivery is not failed, or its webhook is disabled.
var ErrWebhookRetry = errors.New("only failed deliveries of enabled webhooks can be retried")

const webhookColumns = `name, url, events, origin, enabled, secret`

func scanWebhook(row interface{ Scan(...any) error }) (webhook.Config, error) {
	var c webhook.Config
	var events string
	if err := row.Scan(&c.Name, &c.URL, &events, &c.Origin, &c.Enabled, &c.Secret); err != nil {
		return c, err
	}
	return c, json.Unmarshal([]byte(events), &c.Events)
}

// Webhooks lists destinations; secrets are for internal delivery only.
func (s *Store) Webhooks(ctx context.Context) ([]webhook.Config, error) {
	rows, err := s.db.QueryContext(ctx, `SELECT `+webhookColumns+` FROM webhooks ORDER BY name`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []webhook.Config{}
	for rows.Next() {
		c, e := scanWebhook(rows)
		if e != nil {
			return nil, e
		}
		out = append(out, c)
	}
	return out, rows.Err()
}

// Webhook reads one destination.
func (s *Store) Webhook(ctx context.Context, name string) (webhook.Config, error) {
	c, err := scanWebhook(s.db.QueryRowContext(ctx, `SELECT `+webhookColumns+` FROM webhooks WHERE name=?`, name))
	if errors.Is(err, sql.ErrNoRows) {
		err = ErrNotFound
	}
	return c, err
}

// CreateWebhook atomically enforces the fanout limit.
func (s *Store) CreateWebhook(ctx context.Context, c webhook.Config) error {
	return s.tx(ctx, func(tx *sql.Tx) error {
		var count int
		if err := tx.QueryRowContext(ctx, `SELECT count(*) FROM webhooks`).Scan(&count); err != nil {
			return err
		}
		if count >= MaxWebhooks {
			return ErrWebhookLimit
		}
		events, err := json.Marshal(c.Events)
		if err != nil {
			return err
		}
		_, err = tx.ExecContext(ctx, `INSERT INTO webhooks (`+webhookColumns+`) VALUES (?,?,?,?,?,?)`, c.Name, c.URL, string(events), c.Origin, c.Enabled, c.Secret)
		if isConstraint(err) {
			return ErrExists
		}
		return err
	})
}

// UpdateWebhook replaces settings. An empty secret retains the existing key.
func (s *Store) UpdateWebhook(ctx context.Context, c webhook.Config) error {
	events, err := json.Marshal(c.Events)
	if err != nil {
		return err
	}
	res, err := s.db.ExecContext(ctx, `UPDATE webhooks SET url=?, events=?, origin=?, enabled=?, secret=CASE WHEN ?='' THEN secret ELSE ? END WHERE name=?`, c.URL, string(events), c.Origin, c.Enabled, c.Secret, c.Secret, c.Name)
	return webhookChanged(res, err)
}

// DeleteWebhook also deletes queued deliveries through the foreign key.
func (s *Store) DeleteWebhook(ctx context.Context, name string) error {
	res, err := s.db.ExecContext(ctx, `DELETE FROM webhooks WHERE name=?`, name)
	return webhookChanged(res, err)
}

func webhookChanged(res sql.Result, err error) error {
	if err != nil {
		return err
	}
	n, err := res.RowsAffected()
	if err != nil {
		return err
	}
	if n == 0 {
		return ErrNotFound
	}
	return nil
}

// queueWebhooks shares the source mutation's transaction: either both commit or neither does.
func queueWebhooks(ctx context.Context, tx *sql.Tx, p webhook.Payload) error {
	rows, err := tx.QueryContext(ctx, `SELECT name FROM webhooks WHERE enabled=1 AND (origin='both' OR origin=?)
 AND EXISTS (SELECT 1 FROM json_each(events) WHERE value='*' OR value=?)`, p.Origin, p.Type)
	if err != nil {
		return err
	}
	var names []string
	for rows.Next() {
		var name string
		if err = rows.Scan(&name); err != nil {
			break
		}
		names = append(names, name)
	}
	rowErr := rows.Err()
	closeErr := rows.Close()
	if err = errors.Join(err, rowErr, closeErr); err != nil {
		return err
	}
	for _, name := range names {
		p.ID = uuid.NewString()
		payload, encodeErr := json.Marshal(p)
		if encodeErr != nil {
			return encodeErr
		}
		_, err = tx.ExecContext(ctx, `INSERT INTO webhook_deliveries (id,webhook,event,origin,payload,status,created_at,next_attempt_at,retry_until) VALUES (?,?,?,?,?,'pending',?,?,?)`, p.ID, name, p.Type, p.Origin, payload, formatTime(p.At), formatTime(p.At), formatTime(p.At.Add(WebhookTTL)))
		if err != nil {
			return fmt.Errorf("queue webhook: %w", err)
		}
	}
	return nil
}

// WebhookDelivery is persisted delivery state; payload and credentials stay internal.
type WebhookDelivery struct {
	ID, Name, Event, Origin, Status      string
	Attempts                             int
	CreatedAt, NextAttemptAt, RetryUntil time.Time
	LastError                            string
	Payload                              []byte
}

const webhookDeliveryColumns = `d.id,d.webhook,d.event,d.origin,d.status,d.attempts,d.created_at,d.next_attempt_at,d.retry_until,d.last_error,d.payload`

func scanWebhookDelivery(row interface{ Scan(...any) error }) (WebhookDelivery, error) {
	var d WebhookDelivery
	var created, next, until string
	if err := row.Scan(&d.ID, &d.Name, &d.Event, &d.Origin, &d.Status, &d.Attempts, &created, &next, &until, &d.LastError, &d.Payload); err != nil {
		return d, err
	}
	var a, b, c error
	d.CreatedAt, a = parseTime(created)
	d.NextAttemptAt, b = parseTime(next)
	d.RetryUntil, c = parseTime(until)
	return d, errors.Join(a, b, c)
}

func (s *Store) webhookDeliveries(ctx context.Context, query string, args ...any) ([]WebhookDelivery, error) {
	rows, err := s.db.QueryContext(ctx, query, args...)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []WebhookDelivery{}
	for rows.Next() {
		d, e := scanWebhookDelivery(rows)
		if e != nil {
			return nil, e
		}
		out = append(out, d)
	}
	return out, rows.Err()
}

// WebhookDeliveries returns bounded recent history.
func (s *Store) WebhookDeliveries(ctx context.Context, name string) ([]WebhookDelivery, error) {
	return s.webhookDeliveries(ctx, `SELECT `+webhookDeliveryColumns+` FROM webhook_deliveries d WHERE webhook=? ORDER BY seq DESC LIMIT 100`, name)
}

// DueWebhooks selects at most one due delivery per enabled endpoint, so failures do not block other events.
func (s *Store) DueWebhooks(ctx context.Context, now time.Time) ([]WebhookDelivery, error) {
	return s.webhookDeliveries(ctx, `SELECT `+webhookDeliveryColumns+` FROM webhook_deliveries d
 JOIN webhooks w ON w.name=d.webhook WHERE w.enabled=1 AND d.status='pending' AND d.next_attempt_at<=?
 AND d.seq=(SELECT min(x.seq) FROM webhook_deliveries x WHERE x.webhook=d.webhook AND x.status='pending' AND x.next_attempt_at<=?) ORDER BY d.seq`, formatTime(now), formatTime(now))
}

// FinishWebhook records one attempt without overwriting a deleted delivery.
func (s *Store) FinishWebhook(ctx context.Context, id, status, message string, next, now time.Time) error {
	var finished any
	if status != "pending" {
		finished = formatTime(now)
	}
	_, err := s.db.ExecContext(ctx, `UPDATE webhook_deliveries SET status=?,attempts=attempts+1,last_error=?,next_attempt_at=?,finished_at=? WHERE id=? AND status='pending'`, status, message, formatTime(next), finished, id)
	return err
}

// RetryWebhook resets a failed delivery, preserving its ID and exact payload.
func (s *Store) RetryWebhook(ctx context.Context, name, id string, now time.Time) error {
	res, err := s.db.ExecContext(ctx, `UPDATE webhook_deliveries SET status='pending',attempts=0,last_error='',next_attempt_at=?,retry_until=?,finished_at=NULL
 WHERE webhook=? AND id=? AND status='failed' AND EXISTS (SELECT 1 FROM webhooks WHERE name=? AND enabled=1)`, formatTime(now), formatTime(now.Add(WebhookTTL)), name, id, name)
	if err = webhookChanged(res, err); !errors.Is(err, ErrNotFound) {
		return err
	}
	var found int
	err = s.db.QueryRowContext(ctx, `SELECT 1 FROM webhook_deliveries WHERE webhook=? AND id=?`, name, id).Scan(&found)
	if errors.Is(err, sql.ErrNoRows) {
		return ErrNotFound
	}
	if err != nil {
		return err
	}
	return ErrWebhookRetry
}

// MaintainWebhooks expires old work even while paused and bounds terminal history.
func (s *Store) MaintainWebhooks(ctx context.Context, now time.Time) error {
	return s.tx(ctx, func(tx *sql.Tx) error {
		if _, err := tx.ExecContext(ctx, `UPDATE webhook_deliveries SET status='failed',finished_at=?,last_error='retry window expired' WHERE status='pending' AND retry_until<=?`, formatTime(now), formatTime(now)); err != nil {
			return err
		}
		_, err := tx.ExecContext(ctx, `DELETE FROM webhook_deliveries WHERE status!='pending' AND finished_at<?`, formatTime(now.Add(-WebhookTTL)))
		return err
	})
}

// WebhookDestination resolves a still-queued delivery to its current destination.
// A deleted and recreated name must never receive the deleted hook's in-flight work.
func (s *Store) WebhookDestination(ctx context.Context, id string) (webhook.Config, error) {
	c, err := scanWebhook(s.db.QueryRowContext(ctx, `SELECT w.name,w.url,w.events,w.origin,w.enabled,w.secret FROM webhooks w JOIN webhook_deliveries d ON d.webhook=w.name WHERE d.id=? AND d.status='pending'`, id))
	if errors.Is(err, sql.ErrNoRows) {
		err = ErrNotFound
	}
	return c, err
}
