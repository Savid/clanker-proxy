package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"time"

	"github.com/google/uuid"

	"github.com/savid/clanker-proxy/pkg/webhook"
)

// MaxPeeringNotifications bounds records originating at the public request endpoint.
const MaxPeeringNotifications = 100

// WebhookTTL bounds automatic retries and retention of completed deliveries.
const WebhookTTL = 7 * 24 * time.Hour

// ErrWebhookRetry means a delivery is not failed, or its webhook is disabled.
var ErrWebhookRetry = errors.New("only failed deliveries of enabled webhooks can be retried")

// InvalidWebhookError is a validation failure inside an atomic settings update.
type InvalidWebhookError struct{ Err error }

func (e *InvalidWebhookError) Error() string { return e.Err.Error() }

func (e *InvalidWebhookError) Unwrap() error { return e.Err }

const webhookColumns = `name, type, url, events, origin, peers, enabled, secret, headers, retry_after, failures`

func scanWebhook(row interface{ Scan(...any) error }) (webhook.Config, error) {
	var c webhook.Config
	var events, peers, headers, paused string
	if err := row.Scan(&c.Name, &c.Type, &c.URL, &events, &c.Origin, &peers, &c.Enabled, &c.Secret, &headers, &paused, &c.Failures); err != nil {
		return c, err
	}
	if paused != "" {
		var err error
		if c.PausedUntil, err = parseTime(paused); err != nil {
			return c, err
		}
	}
	if err := json.Unmarshal([]byte(headers), &c.Headers); err != nil {
		return c, err
	}
	if err := json.Unmarshal([]byte(peers), &c.Peers); err != nil {
		return c, err
	}
	return c, json.Unmarshal([]byte(events), &c.Events)
}

// peerList encodes a peer list, always as an array, so SQL can count it.
func peerList(peers []string) string {
	if len(peers) == 0 {
		return "[]"
	}
	data, _ := json.Marshal(peers) //nolint:errchkjson // a []string always encodes
	return string(data)
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

// CreateWebhook stores a new destination without replacing an existing name.
func (s *Store) CreateWebhook(ctx context.Context, c webhook.Config) error {
	events, err := json.Marshal(c.Events)
	if err != nil {
		return err
	}
	headers, err := json.Marshal(c.Headers)
	if err != nil {
		return err
	}
	_, err = s.db.ExecContext(ctx, `INSERT INTO webhooks (name, type, url, events, origin, peers, enabled, secret, headers) VALUES (?,?,?,?,?,?,?,?,?)`, c.Name, c.Type, c.URL, string(events), c.Origin, peerList(c.Peers), c.Enabled, c.Secret, string(headers))
	if isConstraint(err) {
		return ErrExists
	}
	return err
}

// UpdateWebhook merges and validates under the write lock, so unrelated edits
// cannot undo credential rotation or overwrite a deleted and recreated name.
// An owner update also clears backoff to resume a repaired destination.
func (s *Store) UpdateWebhook(ctx context.Context, name string, change webhook.Update) (webhook.Config, error) {
	var out webhook.Config
	err := s.tx(ctx, func(tx *sql.Tx) error {
		c, err := scanWebhook(tx.QueryRowContext(ctx, `SELECT `+webhookColumns+` FROM webhooks WHERE name=?`, name))
		if errors.Is(err, sql.ErrNoRows) {
			return ErrNotFound
		}
		if err != nil {
			return err
		}
		c = change.Apply(c)
		if err = c.Validate(); err != nil {
			return &InvalidWebhookError{Err: err}
		}
		events, err := json.Marshal(c.Events)
		if err != nil {
			return err
		}
		headers, err := json.Marshal(c.Headers)
		if err != nil {
			return err
		}
		if _, err = tx.ExecContext(ctx, `UPDATE webhooks SET retry_after='', failures=0, url=?, events=?, origin=?, peers=?, enabled=?, secret=?, headers=? WHERE name=?`, c.URL, string(events), c.Origin, peerList(c.Peers), c.Enabled, c.Secret, string(headers), name); err != nil {
			return err
		}
		c.PausedUntil = time.Time{}
		c.Failures = 0
		out = c
		return nil
	})
	return out, err
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
	// A webhook with peers takes only those peers' thread events: a peering
	// request carries a name its sender chose.
	rows, err := tx.QueryContext(ctx, `SELECT name FROM webhooks WHERE enabled=1 AND (origin='both' OR origin=?)
 AND EXISTS (SELECT 1 FROM json_each(events) WHERE value='*' OR value=?)
 AND (json_array_length(peers)=0 OR (?<>'peering.requested' AND EXISTS (SELECT 1 FROM json_each(peers) WHERE value=?)))`,
		p.Origin, p.Type, p.Type, p.Peer)
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
		// A notification not yet delivered is replaced by a newer one of the
		// same kind about the same thread, and peering notifications by the
		// newest one, so a flood of events queues one notification, not one
		// each. Payloads are metadata; receivers read current state.
		replaced := `DELETE FROM webhook_deliveries WHERE webhook=? AND status='pending' AND event=? AND subject=?`
		args := []any{name, p.Type, p.Subject}
		if p.Type == "peering.requested" {
			replaced = `DELETE FROM webhook_deliveries WHERE webhook=? AND status='pending' AND event=?`
			args = args[:2]
		}
		if _, err = tx.ExecContext(ctx, replaced, args...); err != nil {
			return fmt.Errorf("replace webhook: %w", err)
		}
		p.ID = uuid.NewString()
		payload, encodeErr := json.Marshal(p)
		if encodeErr != nil {
			return encodeErr
		}
		_, err = tx.ExecContext(ctx, `INSERT INTO webhook_deliveries (id,webhook,event,origin,subject,payload,status,created_at,next_attempt_at,retry_until) VALUES (?,?,?,?,?,?,'pending',?,?,?)`, p.ID, name, p.Type, p.Origin, p.Subject, payload, formatTime(p.At), formatTime(p.At), formatTime(p.At.Add(WebhookTTL)))
		if err != nil {
			return fmt.Errorf("queue webhook: %w", err)
		}
		if p.Type == "peering.requested" {
			// Public requests cannot turn a bounded inbox into an unbounded notification queue.
			if _, err = tx.ExecContext(ctx, `DELETE FROM webhook_deliveries WHERE seq IN
                (SELECT seq FROM webhook_deliveries WHERE webhook=? AND event='peering.requested' ORDER BY seq DESC LIMIT -1 OFFSET ?)`, name, MaxPeeringNotifications); err != nil {
				return err
			}
		}
	}
	return nil
}

// WebhookDelivery is persisted delivery state; payload and credentials stay internal.
type WebhookDelivery struct {
	Seq                                      int64
	ID, Name, Event, Origin, Subject, Status string
	Attempts                                 int
	CreatedAt, NextAttemptAt, RetryUntil     time.Time
	LastError                                string
	Payload                                  []byte
}

const webhookDeliveryColumns = `d.seq,d.id,d.webhook,d.event,d.origin,d.subject,d.status,d.attempts,d.created_at,d.next_attempt_at,d.retry_until,d.last_error,d.payload`

func scanWebhookDelivery(row interface{ Scan(...any) error }) (WebhookDelivery, error) {
	var d WebhookDelivery
	var created, next, until string
	if err := row.Scan(&d.Seq, &d.ID, &d.Name, &d.Event, &d.Origin, &d.Subject, &d.Status, &d.Attempts, &created, &next, &until, &d.LastError, &d.Payload); err != nil {
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

// WebhookFilter pages newest-first history with an optional status filter.
type WebhookFilter struct {
	Before int64
	Limit  int
	Status string
}

// WebhookDeliveries uses the last row's sequence as the next page's exclusive cursor.
func (s *Store) WebhookDeliveries(ctx context.Context, name string, f WebhookFilter) ([]WebhookDelivery, error) {
	if f.Limit == 0 {
		f.Limit = 100
	}
	if f.Before == 0 {
		f.Before = math.MaxInt64
	}
	query := `SELECT ` + webhookDeliveryColumns + ` FROM webhook_deliveries d WHERE webhook=? AND seq<?`
	args := []any{name, f.Before}
	if f.Status != "" {
		query += ` AND status=?`
		args = append(args, f.Status)
	}
	query += ` ORDER BY seq DESC LIMIT ?`
	args = append(args, f.Limit)
	return s.webhookDeliveries(ctx, query, args...)
}

// DueWebhooks selects at most one due delivery per enabled endpoint, so failures do not block other events.
// Expired deliveries are skipped even before MaintainWebhooks marks them failed.
func (s *Store) DueWebhooks(ctx context.Context, now time.Time) ([]WebhookDelivery, error) {
	at := formatTime(now)
	return s.webhookDeliveries(ctx, `SELECT `+webhookDeliveryColumns+` FROM webhooks w
 JOIN webhook_deliveries d ON d.seq=(SELECT x.seq FROM webhook_deliveries x
 WHERE x.webhook=w.name AND x.status='pending' AND x.next_attempt_at<=? AND x.retry_until>?
 ORDER BY x.next_attempt_at,x.seq LIMIT 1)
 WHERE w.enabled=1 AND w.retry_after<=? ORDER BY d.next_attempt_at,d.seq`, at, at, at)
}

// FinishWebhook records one attempt without overwriting a deleted delivery.
// A delivered attempt proves the endpoint works and resets its failure count.
func (s *Store) FinishWebhook(ctx context.Context, id, status, message string, next, now time.Time) error {
	var finished any
	if status != "pending" {
		finished = formatTime(now)
	}
	return s.tx(ctx, func(tx *sql.Tx) error {
		if _, err := tx.ExecContext(ctx, `UPDATE webhook_deliveries SET status=?,attempts=attempts+1,last_error=?,next_attempt_at=?,finished_at=? WHERE id=? AND status='pending'`, status, message, formatTime(next), finished, id); err != nil {
			return err
		}
		if status != "delivered" {
			return nil
		}
		_, err := tx.ExecContext(ctx, `UPDATE webhooks SET failures=0 WHERE name=(SELECT webhook FROM webhook_deliveries WHERE id=?)`, id)
		return err
	})
}

// RetryWebhook resets a failed delivery, preserving its ID and exact payload,
// and clears the endpoint's backoff so the retry is attempted now.
func (s *Store) RetryWebhook(ctx context.Context, name, id string, now time.Time) error {
	err := s.tx(ctx, func(tx *sql.Tx) error {
		res, err := tx.ExecContext(ctx, `UPDATE webhook_deliveries SET status='pending',attempts=0,last_error='',next_attempt_at=?,retry_until=?,finished_at=NULL
 WHERE webhook=? AND id=? AND status='failed' AND EXISTS (SELECT 1 FROM webhooks WHERE name=? AND enabled=1)`, formatTime(now), formatTime(now.Add(WebhookTTL)), name, id, name)
		if err = webhookChanged(res, err); err != nil {
			return err
		}
		_, err = tx.ExecContext(ctx, `UPDATE webhooks SET retry_after='', failures=0 WHERE name=?`, name)
		return err
	})
	if !errors.Is(err, ErrNotFound) {
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

// MaintainWebhooks expires waiting work and bounds history. Active attempts must
// finish themselves before a delivery can expire or be manually retried.
func (s *Store) MaintainWebhooks(ctx context.Context, now time.Time, active []string) error {
	activeJSON, encodeErr := json.Marshal(nonNil(active))
	if encodeErr != nil {
		return encodeErr
	}
	return s.tx(ctx, func(tx *sql.Tx) error {
		if _, err := tx.ExecContext(ctx, `UPDATE webhook_deliveries SET status='failed',finished_at=?,last_error='retry window expired' WHERE status='pending' AND retry_until<=? AND id NOT IN (SELECT value FROM json_each(?))`, formatTime(now), formatTime(now), string(activeJSON)); err != nil {
			return err
		}
		_, err := tx.ExecContext(ctx, `DELETE FROM webhook_deliveries WHERE status!='pending' AND finished_at<?`, formatTime(now.Add(-WebhookTTL)))
		return err
	})
}

// WebhookDestination resolves a still-queued delivery to its current destination.
// A deleted and recreated name must never receive the deleted hook's in-flight work.
func (s *Store) WebhookDestination(ctx context.Context, id string) (webhook.Config, error) {
	c, err := scanWebhook(s.db.QueryRowContext(ctx, `SELECT `+webhookColumns+` FROM webhooks WHERE name=(SELECT webhook FROM webhook_deliveries WHERE id=? AND status='pending')`, id))
	if errors.Is(err, sql.ErrNoRows) {
		err = ErrNotFound
	}
	return c, err
}

// DeferWebhook records a retryable endpoint failure and holds all pending work
// at that endpoint until the given time. It applies only while the endpoint
// still has the URL, signing key and headers the request was sent with, so a
// request that fails after an owner update cannot pause the repaired
// destination.
func (s *Store) DeferWebhook(ctx context.Context, id string, sent webhook.Config, until time.Time) error {
	headers, err := json.Marshal(sent.Headers)
	if err != nil {
		return err
	}
	_, err = s.db.ExecContext(ctx, `UPDATE webhooks SET retry_after=max(retry_after,?), failures=failures+1
 WHERE name=(SELECT webhook FROM webhook_deliveries WHERE id=?) AND url=? AND secret=? AND headers=?`,
		formatTime(until), id, sent.URL, sent.Secret, string(headers))
	return err
}
