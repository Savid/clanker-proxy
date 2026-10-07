package store

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"time"
)

// ErrExists is returned when a peer of that name already exists.
var ErrExists = errors.New("already exists")

// Peer statuses.
const (
	// PeerRequested: the owner asked them; their owner has not approved.
	PeerRequested = "requested"
	// PeerActive: both daemons accept the secret.
	PeerActive = "active"
)

// Peer is a daemon the owner exchanges events with.
type Peer struct {
	Name    string
	URL     string
	Secret  string
	Status  string
	AddedAt time.Time
}

// Request is a pending peering request from another daemon.
type Request struct {
	ID     string
	Name   string
	URL    string
	Secret string
	Note   string
	At     time.Time
}

const peerColumns = `name, url, secret, status, added_at`

func scanPeer(row interface{ Scan(dest ...any) error }) (Peer, error) {
	var (
		p     Peer
		added string
	)

	if err := row.Scan(&p.Name, &p.URL, &p.Secret, &p.Status, &added); err != nil {
		return p, err
	}

	var err error

	p.AddedAt, err = parseTime(added)

	return p, err
}

// AddPeer adds a peer, or returns ErrExists.
func (s *Store) AddPeer(ctx context.Context, p Peer) error {
	_, err := s.db.ExecContext(ctx, `INSERT INTO peers (`+peerColumns+`) VALUES (?, ?, ?, ?, ?)`,
		p.Name, p.URL, p.Secret, p.Status, formatTime(p.AddedAt))
	if isConstraint(err) {
		return ErrExists
	}

	if err != nil {
		return fmt.Errorf("insert peer: %w", err)
	}

	return nil
}

// Peer returns one peer, or ErrNotFound.
func (s *Store) Peer(ctx context.Context, name string) (Peer, error) {
	p, err := scanPeer(s.db.QueryRowContext(ctx, `SELECT `+peerColumns+` FROM peers WHERE name = ?`, name))
	if errors.Is(err, sql.ErrNoRows) {
		return p, ErrNotFound
	}

	if err != nil {
		return p, fmt.Errorf("read peer: %w", err)
	}

	return p, nil
}

// Peers returns every peer, by name.
func (s *Store) Peers(ctx context.Context) ([]Peer, error) {
	rows, err := s.db.QueryContext(ctx, `SELECT `+peerColumns+` FROM peers ORDER BY name`)
	if err != nil {
		return nil, fmt.Errorf("list peers: %w", err)
	}
	defer rows.Close()

	var peers []Peer

	for rows.Next() {
		p, scanErr := scanPeer(rows)
		if scanErr != nil {
			return nil, fmt.Errorf("list peers: %w", scanErr)
		}

		peers = append(peers, p)
	}

	if err = rows.Err(); err != nil {
		return nil, fmt.Errorf("list peers: %w", err)
	}

	return peers, nil
}

// Activate marks a requested peer active and makes its pending events due
// at at. It reports whether the peer was requested; an active peer is left
// alone.
func (s *Store) Activate(ctx context.Context, name string, at time.Time) (bool, error) {
	var changed bool

	err := s.tx(ctx, func(tx *sql.Tx) error {
		res, err := tx.ExecContext(ctx, `UPDATE peers SET status = 'active' WHERE name = ? AND status = 'requested'`, name)
		if err != nil {
			return fmt.Errorf("activate peer: %w", err)
		}

		if n, _ := res.RowsAffected(); n == 0 {
			return nil
		}

		changed = true

		if _, err = tx.ExecContext(ctx, `UPDATE outbox SET next_attempt_at = ? WHERE peer = ? AND status = 'pending'`,
			formatTime(at), name); err != nil {
			return fmt.Errorf("reschedule outbox: %w", err)
		}

		return nil
	})

	return changed, err
}

// RemovePeer forgets a peer and fails their undelivered events. Threads
// stay.
func (s *Store) RemovePeer(ctx context.Context, name string) error {
	return s.tx(ctx, func(tx *sql.Tx) error {
		res, err := tx.ExecContext(ctx, `DELETE FROM peers WHERE name = ?`, name)
		if err != nil {
			return fmt.Errorf("delete peer: %w", err)
		}

		if n, _ := res.RowsAffected(); n == 0 {
			return ErrNotFound
		}

		if _, err = tx.ExecContext(ctx, `UPDATE outbox SET status = 'failed', last_error = 'peer removed',
			next_attempt_at = NULL WHERE peer = ? AND status = 'pending'`, name); err != nil {
			return fmt.Errorf("fail outbox: %w", err)
		}

		return nil
	})
}

// AddRequest stores a peering request, first dropping requests made before
// expired. It refuses with ErrFull once max requests are pending.
func (s *Store) AddRequest(ctx context.Context, r Request, maxPending int, expired time.Time) error {
	return s.tx(ctx, func(tx *sql.Tx) error {
		if _, err := tx.ExecContext(ctx, `DELETE FROM requests WHERE at < ?`, formatTime(expired)); err != nil {
			return fmt.Errorf("expire requests: %w", err)
		}

		var n int
		if err := tx.QueryRowContext(ctx, `SELECT count(*) FROM requests`).Scan(&n); err != nil {
			return fmt.Errorf("count requests: %w", err)
		}

		if n >= maxPending {
			return ErrFull
		}

		if _, err := tx.ExecContext(ctx, `INSERT INTO requests (id, name, url, secret, note, at) VALUES (?, ?, ?, ?, ?, ?)`,
			r.ID, r.Name, r.URL, r.Secret, r.Note, formatTime(r.At)); err != nil {
			return fmt.Errorf("insert request: %w", err)
		}

		return nil
	})
}

// ErrFull is returned when too many requests are pending.
var ErrFull = errors.New("too many pending requests")

const (
	selectRequests = `SELECT id, name, url, secret, note, at FROM requests `
	requestByID    = selectRequests + `WHERE id = ? AND at >= ?`
	requestsSince  = selectRequests + `WHERE at >= ? ORDER BY at, id`
)

// Request returns one request made since expired, or ErrNotFound.
func (s *Store) Request(ctx context.Context, id string, expired time.Time) (Request, error) {
	reqs, err := s.requests(ctx, requestByID, id, formatTime(expired))
	if err != nil {
		return Request{}, err
	}

	if len(reqs) == 0 {
		return Request{}, ErrNotFound
	}

	return reqs[0], nil
}

// Requests returns requests made since expired, oldest first.
func (s *Store) Requests(ctx context.Context, expired time.Time) ([]Request, error) {
	return s.requests(ctx, requestsSince, formatTime(expired))
}

func (s *Store) requests(ctx context.Context, query string, args ...any) ([]Request, error) {
	rows, err := s.db.QueryContext(ctx, query, args...)
	if err != nil {
		return nil, fmt.Errorf("list requests: %w", err)
	}
	defer rows.Close()

	var out []Request

	for rows.Next() {
		var (
			r  Request
			at string
		)

		if err = rows.Scan(&r.ID, &r.Name, &r.URL, &r.Secret, &r.Note, &at); err != nil {
			return nil, fmt.Errorf("list requests: %w", err)
		}

		if r.At, err = parseTime(at); err != nil {
			return nil, err
		}

		out = append(out, r)
	}

	if err = rows.Err(); err != nil {
		return nil, fmt.Errorf("list requests: %w", err)
	}

	return out, nil
}

// Approve turns request id, made since expired, into an active peer called
// name, replacing any peer of that name; events queued for name are due at
// at. It returns ErrNotFound for an unknown or expired request.
func (s *Store) Approve(ctx context.Context, id, name string, at, expired time.Time) (Peer, error) {
	var p Peer

	err := s.tx(ctx, func(tx *sql.Tx) error {
		var url, secret string

		err := tx.QueryRowContext(ctx, `SELECT url, secret FROM requests WHERE id = ? AND at >= ?`, id, formatTime(expired)).
			Scan(&url, &secret)
		if errors.Is(err, sql.ErrNoRows) {
			return ErrNotFound
		}

		if err != nil {
			return fmt.Errorf("read request: %w", err)
		}

		p = Peer{Name: name, URL: url, Secret: secret, Status: PeerActive, AddedAt: at}

		for _, q := range []struct {
			sql  string
			args []any
		}{
			{`DELETE FROM peers WHERE name = ?`, []any{name}},
			{`INSERT INTO peers (` + peerColumns + `) VALUES (?, ?, ?, ?, ?)`, []any{p.Name, p.URL, p.Secret, p.Status, formatTime(at)}},
			{`DELETE FROM requests WHERE id = ?`, []any{id}},
			{`UPDATE outbox SET next_attempt_at = ? WHERE peer = ? AND status = 'pending'`, []any{formatTime(at), name}},
		} {
			if _, err = tx.ExecContext(ctx, q.sql, q.args...); err != nil {
				return fmt.Errorf("approve: %w", err)
			}
		}

		return nil
	})

	return p, err
}

// Unapprove undoes Approve: it removes the peer approve made, puts back prev
// (the peer of that name before, if any) and the request r.
func (s *Store) Unapprove(ctx context.Context, approved Peer, prev *Peer, r Request) error {
	return s.tx(ctx, func(tx *sql.Tx) error {
		if _, err := tx.ExecContext(ctx, `DELETE FROM peers WHERE name = ? AND secret = ?`, approved.Name, approved.Secret); err != nil {
			return fmt.Errorf("unapprove: %w", err)
		}

		if prev != nil {
			if _, err := tx.ExecContext(ctx, `INSERT INTO peers (`+peerColumns+`) VALUES (?, ?, ?, ?, ?)`,
				prev.Name, prev.URL, prev.Secret, prev.Status, formatTime(prev.AddedAt)); err != nil {
				return fmt.Errorf("unapprove: %w", err)
			}
		}

		if _, err := tx.ExecContext(ctx, `INSERT INTO requests (id, name, url, secret, note, at) VALUES (?, ?, ?, ?, ?, ?)`,
			r.ID, r.Name, r.URL, r.Secret, r.Note, formatTime(r.At)); err != nil {
			return fmt.Errorf("unapprove: %w", err)
		}

		return nil
	})
}

// DeleteRequest drops a pending request, or returns ErrNotFound.
func (s *Store) DeleteRequest(ctx context.Context, id string) error {
	res, err := s.db.ExecContext(ctx, `DELETE FROM requests WHERE id = ?`, id)
	if err != nil {
		return fmt.Errorf("delete request: %w", err)
	}

	if n, _ := res.RowsAffected(); n == 0 {
		return ErrNotFound
	}

	return nil
}
