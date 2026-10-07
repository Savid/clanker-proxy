package store

import (
	"context"
	"database/sql"
	"fmt"
	"time"

	"github.com/savid/clanker-proxy/pkg/thread"
)

// Outgoing is a queued event to deliver, with where and how.
type Outgoing struct {
	Event    thread.Event
	Peer     string
	URL      string
	Secret   string
	Attempts int
	StoredAt time.Time
}

// Due returns pending events whose next attempt is at or before now, oldest
// first. An event behind one of its peer's that is still waiting is not due:
// each peer receives its events in the order they were sent.
func (s *Store) Due(ctx context.Context, now time.Time, limit int) ([]Outgoing, error) {
	at := formatTime(now)

	rows, err := s.db.QueryContext(ctx, `SELECT e.data, e.stored_at, o.peer, p.url, p.secret, o.attempts
		FROM outbox o JOIN events e ON e.id = o.event_id JOIN peers p ON p.name = o.peer
		WHERE o.status = 'pending' AND o.next_attempt_at <= ?
			AND NOT EXISTS (SELECT 1 FROM outbox w WHERE w.peer = o.peer AND w.status = 'pending'
				AND w.seq < o.seq AND w.next_attempt_at > ?)
		ORDER BY o.seq LIMIT ?`, at, at, limit)
	if err != nil {
		return nil, fmt.Errorf("read outbox: %w", err)
	}
	defer rows.Close()

	var out []Outgoing

	for rows.Next() {
		var (
			o      Outgoing
			data   []byte
			stored string
		)

		if err = rows.Scan(&data, &stored, &o.Peer, &o.URL, &o.Secret, &o.Attempts); err != nil {
			return nil, fmt.Errorf("read outbox: %w", err)
		}

		if o.StoredAt, err = parseTime(stored); err != nil {
			return nil, err
		}

		if o.Event, err = thread.Parse(data); err != nil {
			return nil, fmt.Errorf("stored event: %w", err)
		}

		out = append(out, o)
	}

	if err = rows.Err(); err != nil {
		return nil, fmt.Errorf("read outbox: %w", err)
	}

	return out, nil
}

// Pending rechecks a due snapshot before sending it. Peer removal, secret
// replacement or a completed attempt must invalidate the snapshot.
func (s *Store) Pending(ctx context.Context, o Outgoing) (bool, error) {
	var pending bool
	if err := s.db.QueryRowContext(ctx, `SELECT EXISTS (
		SELECT 1 FROM outbox o JOIN peers p ON p.name = o.peer
		WHERE o.event_id = ? AND o.peer = ? AND o.status = 'pending' AND o.attempts = ?
			AND p.url = ? AND p.secret = ?)`, o.Event.ID, o.Peer, o.Attempts, o.URL, o.Secret).
		Scan(&pending); err != nil {
		return false, fmt.Errorf("check pending delivery: %w", err)
	}

	return pending, nil
}

// NextDue returns when the next event is due, or the zero time when nothing
// is pending. Only each peer's oldest pending event counts: the rest follow
// it.
func (s *Store) NextDue(ctx context.Context) (time.Time, error) {
	var next sql.NullString
	if err := s.db.QueryRowContext(ctx, `SELECT min(o.next_attempt_at) FROM outbox o JOIN peers p ON p.name = o.peer
		WHERE o.status = 'pending'
			AND NOT EXISTS (SELECT 1 FROM outbox w WHERE w.peer = o.peer AND w.status = 'pending' AND w.seq < o.seq)`).
		Scan(&next); err != nil {
		return time.Time{}, fmt.Errorf("read outbox: %w", err)
	}

	return parseNullTime(next)
}

// Delivered marks an event delivered.
func (s *Store) Delivered(ctx context.Context, eventID string, at time.Time) error {
	return s.markOutbox(ctx, `UPDATE outbox SET status = 'delivered', attempts = attempts + 1, delivered_at = ?,
		next_attempt_at = NULL, last_error = NULL WHERE event_id = ? AND status = 'pending'`, formatTime(at), eventID)
}

// Retry records a failed attempt and when to try again.
func (s *Store) Retry(ctx context.Context, eventID, reason string, next time.Time) error {
	return s.markOutbox(ctx, `UPDATE outbox SET attempts = attempts + 1, last_error = ?, next_attempt_at = ?
		WHERE event_id = ? AND status = 'pending'`, reason, formatTime(next), eventID)
}

// Failed marks an event the peer refused for good.
func (s *Store) Failed(ctx context.Context, eventID, reason string) error {
	return s.markOutbox(ctx, `UPDATE outbox SET status = 'failed', attempts = attempts + 1, last_error = ?,
		next_attempt_at = NULL WHERE event_id = ? AND status = 'pending'`, reason, eventID)
}

func (s *Store) markOutbox(ctx context.Context, query string, args ...any) error {
	if _, err := s.db.ExecContext(ctx, query, args...); err != nil {
		return fmt.Errorf("update outbox: %w", err)
	}

	return nil
}
