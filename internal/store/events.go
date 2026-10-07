package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/savid/clanker-proxy/pkg/thread"
	"github.com/savid/clanker-proxy/pkg/webhook"
)

// ErrAmbiguous is returned when a thread reference matches more than one
// thread.
var ErrAmbiguous = errors.New("ambiguous thread reference")

// DeliveryFailed is the status of an event that will never be delivered.
const DeliveryFailed = "failed"

// Event is a stored event and, for the owner's, its delivery.
type Event struct {
	thread.Event
	// StoredAt is when this daemon stored it.
	StoredAt time.Time
	// Delivery is nil for the peer's events.
	Delivery *Delivery
}

// Delivery is where an event is in reaching the peer.
type Delivery struct {
	Status        string
	Attempts      int
	LastError     string
	NextAttemptAt time.Time
	DeliveredAt   time.Time
}

// Summary is a thread's listing row: its replayed state and its outbox
// counts.
type Summary struct {
	ID        string
	Title     string
	Kind      thread.Kind
	Labels    []string
	Sender    string
	Recipient string
	Peer      string
	State     thread.State
	Turn      string
	OpenedAt  time.Time
	UpdatedAt time.Time
	Events    int
	// LastFrom wrote the thread's last event in replay order.
	LastFrom    string
	Undelivered int
	Failed      int
}

// Projection builds the listing row for t, seen by self.
func Projection(t thread.Thread, self string) Summary {
	return Summary{
		ID: t.ID, Title: t.Title, Kind: t.Kind, Labels: t.Labels,
		Sender: t.Sender, Recipient: t.Recipient, Peer: t.Peer(self),
		State: t.State, Turn: t.Turn(), OpenedAt: t.OpenedAt, UpdatedAt: t.UpdatedAt,
		Events: len(t.Events), LastFrom: t.Events[len(t.Events)-1].From,
	}
}

// StoredEvent returns an event by ID, or ErrNotFound.
func (s *Store) StoredEvent(ctx context.Context, id string) (thread.Event, error) {
	var data []byte

	err := s.db.QueryRowContext(ctx, `SELECT data FROM events WHERE id = ?`, id).Scan(&data)
	if errors.Is(err, sql.ErrNoRows) {
		return thread.Event{}, ErrNotFound
	}

	if err != nil {
		return thread.Event{}, fmt.Errorf("read event: %w", err)
	}

	return thread.Parse(data)
}

// AddEvent stores an event and the thread's new projection together. An
// outbound event (outbox true) is queued for delivery to its recipient; if
// the recipient is not a peer, nothing is stored and it returns ErrNotFound.
func (s *Store) AddEvent(ctx context.Context, e Event, outbox bool, projection Summary) error {
	data, encodeErr := e.Marshal()
	if encodeErr != nil {
		return encodeErr
	}

	return s.tx(ctx, func(tx *sql.Tx) error {
		if _, err := tx.ExecContext(ctx, `INSERT INTO events (id, thread, data, stored_at) VALUES (?, ?, ?, ?)`,
			e.ID, e.Thread, data, formatTime(e.StoredAt)); err != nil {
			return fmt.Errorf("insert event: %w", err)
		}

		if outbox {
			res, err := tx.ExecContext(ctx, `INSERT INTO outbox (event_id, peer, status, next_attempt_at)
				SELECT ?, ?, 'pending', ? WHERE EXISTS (SELECT 1 FROM peers WHERE name = ?)`,
				e.ID, e.To, formatTime(e.StoredAt), e.To)
			if err != nil {
				return fmt.Errorf("queue event: %w", err)
			}

			if n, _ := res.RowsAffected(); n == 0 {
				return ErrNotFound
			}
		}

		labels, err := json.Marshal(nonNil(projection.Labels))
		if err != nil {
			return fmt.Errorf("encode labels: %w", err)
		}

		p := projection
		if _, err = tx.ExecContext(ctx, `INSERT INTO threads
			(id, title, kind, labels, sender, recipient, peer, state, turn, opened_at, updated_at, events, last_from)
			VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
			ON CONFLICT (id) DO UPDATE SET state = excluded.state, turn = excluded.turn,
				updated_at = excluded.updated_at, events = excluded.events, last_from = excluded.last_from`,
			p.ID, p.Title, p.Kind, string(labels), p.Sender, p.Recipient, p.Peer, p.State, p.Turn,
			formatTime(p.OpenedAt), formatTime(p.UpdatedAt), p.Events, p.LastFrom); err != nil {
			return fmt.Errorf("write thread: %w", err)
		}

		origin, self := "incoming", e.To
		if outbox {
			origin, self = "outgoing", e.From
		}
		mine := projection.Turn == self
		return queueWebhooks(ctx, tx, webhook.Payload{Type: webhook.ThreadType(e.Action), Origin: origin, At: e.StoredAt, Subject: e.Thread, EventID: e.ID, Peer: projection.Peer, State: projection.State, MyTurn: &mine})
	})
}

// ThreadEvents returns a thread's stored events, in storage order, with
// their delivery. A thread that does not exist has none.
func (s *Store) ThreadEvents(ctx context.Context, threadID string) ([]Event, error) {
	rows, err := s.db.QueryContext(ctx, `SELECT e.data,
			o.status, o.attempts, o.last_error, o.next_attempt_at, o.delivered_at
		FROM events e LEFT JOIN outbox o ON o.event_id = e.id
		WHERE e.thread = ? ORDER BY e.rowid`, threadID)
	if err != nil {
		return nil, fmt.Errorf("read thread events: %w", err)
	}
	defer rows.Close()

	var events []Event

	for rows.Next() {
		var (
			e                      Event
			data                   []byte
			status, lastErr        sql.NullString
			attempts               sql.NullInt64
			nextAttempt, delivered sql.NullString
		)

		if err = rows.Scan(&data, &status, &attempts, &lastErr, &nextAttempt, &delivered); err != nil {
			return nil, fmt.Errorf("read event: %w", err)
		}

		if e.Event, err = thread.Parse(data); err != nil {
			return nil, fmt.Errorf("stored event: %w", err)
		}

		if status.Valid {
			e.Delivery = &Delivery{Status: status.String, Attempts: int(attempts.Int64), LastError: lastErr.String}
			if e.Delivery.NextAttemptAt, err = parseNullTime(nextAttempt); err != nil {
				return nil, err
			}

			if e.Delivery.DeliveredAt, err = parseNullTime(delivered); err != nil {
				return nil, err
			}
		}

		events = append(events, e)
	}

	if err = rows.Err(); err != nil {
		return nil, fmt.Errorf("read thread events: %w", err)
	}

	return events, nil
}

// ResolveThread turns a thread ID or unique ID prefix into the ID.
func (s *Store) ResolveThread(ctx context.Context, ref string) (string, error) {
	ref = strings.ToLower(ref)
	pattern := strings.NewReplacer(`\`, `\\`, `%`, `\%`, `_`, `\_`).Replace(ref) + "%"

	rows, err := s.db.QueryContext(ctx, `SELECT id FROM threads WHERE id LIKE ? ESCAPE '\' LIMIT 2`, pattern)
	if err != nil {
		return "", fmt.Errorf("resolve thread: %w", err)
	}
	defer rows.Close()

	var ids []string

	for rows.Next() {
		var id string
		if err = rows.Scan(&id); err != nil {
			return "", fmt.Errorf("resolve thread: %w", err)
		}

		ids = append(ids, id)
	}

	if err = rows.Err(); err != nil {
		return "", fmt.Errorf("resolve thread: %w", err)
	}

	switch len(ids) {
	case 0:
		return "", ErrNotFound
	case 1:
		return ids[0], nil
	default:
		return "", ErrAmbiguous
	}
}

// Filter narrows a thread listing. Zero fields match everything.
type Filter struct {
	// Turn is "mine", "theirs" or "none", relative to Self.
	Turn  string
	Self  string
	State thread.State
	Peer  string
	Label string
	Limit int
}

const summaryColumns = `t.id, t.title, t.kind, t.labels, t.sender, t.recipient, t.peer, t.state, t.turn,
	t.opened_at, t.updated_at, t.events, t.last_from,
	(SELECT count(*) FROM outbox o JOIN events e ON e.id = o.event_id WHERE e.thread = t.id AND o.status = 'pending'),
	(SELECT count(*) FROM outbox o JOIN events e ON e.id = o.event_id WHERE e.thread = t.id AND o.status = 'failed')`

// Threads lists thread summaries, newest activity first.
func (s *Store) Threads(ctx context.Context, f Filter) ([]Summary, error) {
	var (
		where []string
		args  []any
	)

	switch f.Turn {
	case "mine":
		where, args = append(where, "t.turn = ?"), append(args, f.Self)
	case "theirs":
		where, args = append(where, "t.turn != '' AND t.turn != ?"), append(args, f.Self)
	case "none":
		where = append(where, "t.turn = ''")
	}

	if f.State != "" {
		where, args = append(where, "t.state = ?"), append(args, f.State)
	}

	if f.Peer != "" {
		where, args = append(where, "t.peer = ?"), append(args, f.Peer)
	}

	if f.Label != "" {
		where, args = append(where, "EXISTS (SELECT 1 FROM json_each(t.labels) WHERE value = ?)"), append(args, f.Label)
	}

	query := `SELECT ` + summaryColumns + ` FROM threads t`
	if len(where) > 0 {
		// Only the fixed fragments above are joined; values are bound.
		query += " WHERE " + strings.Join(where, " AND ") //nolint:gosec // fixed fragments, bound values
	}

	limit := f.Limit
	if limit <= 0 {
		limit = 100
	}

	query += " ORDER BY t.updated_at DESC, t.id LIMIT ?"

	args = append(args, limit)

	rows, err := s.db.QueryContext(ctx, query, args...)
	if err != nil {
		return nil, fmt.Errorf("list threads: %w", err)
	}
	defer rows.Close()

	var out []Summary

	for rows.Next() {
		sum, scanErr := scanSummary(rows)
		if scanErr != nil {
			return nil, scanErr
		}

		out = append(out, sum)
	}

	if err = rows.Err(); err != nil {
		return nil, fmt.Errorf("list threads: %w", err)
	}

	return out, nil
}

// Summary returns one thread's summary, or ErrNotFound.
func (s *Store) Summary(ctx context.Context, id string) (Summary, error) {
	rows, err := s.db.QueryContext(ctx, `SELECT `+summaryColumns+` FROM threads t WHERE t.id = ?`, id)
	if err != nil {
		return Summary{}, fmt.Errorf("read thread: %w", err)
	}
	defer rows.Close()

	if !rows.Next() {
		if err = rows.Err(); err != nil {
			return Summary{}, fmt.Errorf("read thread: %w", err)
		}

		return Summary{}, ErrNotFound
	}

	return scanSummary(rows)
}

func scanSummary(rows *sql.Rows) (Summary, error) {
	var (
		s               Summary
		labels          string
		opened, updated string
	)

	if err := rows.Scan(&s.ID, &s.Title, &s.Kind, &labels, &s.Sender, &s.Recipient, &s.Peer, &s.State, &s.Turn,
		&opened, &updated, &s.Events, &s.LastFrom, &s.Undelivered, &s.Failed); err != nil {
		return s, fmt.Errorf("read thread: %w", err)
	}

	if err := json.Unmarshal([]byte(labels), &s.Labels); err != nil {
		return s, fmt.Errorf("stored labels: %w", err)
	}

	var err error
	if s.OpenedAt, err = parseTime(opened); err != nil {
		return s, err
	}

	if s.UpdatedAt, err = parseTime(updated); err != nil {
		return s, err
	}

	return s, nil
}

func nonNil(s []string) []string {
	if s == nil {
		return []string{}
	}

	return s
}
