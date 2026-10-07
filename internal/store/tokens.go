package store

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"time"
)

// AgentToken is an agent token's record. Only a hash of the token is kept.
type AgentToken struct {
	Name string
	Hash string
	// Peers limits the token to threads with these peers; empty means all.
	Peers     []string
	CreatedAt time.Time
	// ExpiresAt and UsedAt are zero when unset.
	ExpiresAt time.Time
	UsedAt    time.Time
}

const agentColumns = `name, hash, peers, created_at, expires_at, used_at`

func scanAgentToken(row interface{ Scan(...any) error }) (AgentToken, error) {
	var t AgentToken
	var peers, created, expires, used string
	if err := row.Scan(&t.Name, &t.Hash, &peers, &created, &expires, &used); err != nil {
		return t, err
	}
	err := json.Unmarshal([]byte(peers), &t.Peers)
	if err != nil {
		return t, err
	}
	if t.CreatedAt, err = parseTime(created); err != nil {
		return t, err
	}
	if t.ExpiresAt, err = optionalTime(expires); err != nil {
		return t, err
	}
	t.UsedAt, err = optionalTime(used)
	return t, err
}

func optionalTime(s string) (time.Time, error) {
	if s == "" {
		return time.Time{}, nil
	}
	return parseTime(s)
}

func formatOptional(t time.Time) string {
	if t.IsZero() {
		return ""
	}
	return formatTime(t)
}

// AddAgentToken stores a new agent token, refusing a name in use. A token
// that expired by t.CreatedAt gives up its name.
func (s *Store) AddAgentToken(ctx context.Context, t AgentToken) error {
	return s.tx(ctx, func(tx *sql.Tx) error {
		if _, err := tx.ExecContext(ctx, `DELETE FROM agent_tokens WHERE name = ? AND expires_at != '' AND expires_at <= ?`, t.Name, formatTime(t.CreatedAt)); err != nil {
			return fmt.Errorf("add agent token: %w", err)
		}
		_, err := tx.ExecContext(ctx, `INSERT INTO agent_tokens (`+agentColumns+`) VALUES (?, ?, ?, ?, ?, '')`,
			t.Name, t.Hash, peerList(t.Peers), formatTime(t.CreatedAt), formatOptional(t.ExpiresAt))
		if isConstraint(err) {
			return ErrExists
		}
		if err != nil {
			return fmt.Errorf("add agent token: %w", err)
		}
		return nil
	})
}

// AgentTokens lists agent tokens by name.
func (s *Store) AgentTokens(ctx context.Context) ([]AgentToken, error) {
	rows, err := s.db.QueryContext(ctx, `SELECT `+agentColumns+` FROM agent_tokens ORDER BY name`)
	if err != nil {
		return nil, fmt.Errorf("list agent tokens: %w", err)
	}
	defer rows.Close()
	out := []AgentToken{}
	for rows.Next() {
		t, scanErr := scanAgentToken(rows)
		if scanErr != nil {
			return nil, fmt.Errorf("list agent tokens: %w", scanErr)
		}
		out = append(out, t)
	}
	return out, rows.Err()
}

// AgentTokenByHash returns the agent token with this hash.
func (s *Store) AgentTokenByHash(ctx context.Context, hash string) (AgentToken, error) {
	t, err := scanAgentToken(s.db.QueryRowContext(ctx, `SELECT `+agentColumns+` FROM agent_tokens WHERE hash = ?`, hash))
	if errors.Is(err, sql.ErrNoRows) {
		return t, ErrNotFound
	}
	return t, err
}

// TouchAgentToken records a use.
func (s *Store) TouchAgentToken(ctx context.Context, name string, now time.Time) error {
	_, err := s.db.ExecContext(ctx, `UPDATE agent_tokens SET used_at = ? WHERE name = ?`, formatTime(now), name)
	if err != nil {
		return fmt.Errorf("touch agent token: %w", err)
	}
	return nil
}

// DeleteAgentToken revokes an agent token.
func (s *Store) DeleteAgentToken(ctx context.Context, name string) error {
	res, err := s.db.ExecContext(ctx, `DELETE FROM agent_tokens WHERE name = ?`, name)
	if err != nil {
		return fmt.Errorf("delete agent token: %w", err)
	}
	if n, _ := res.RowsAffected(); n == 0 {
		return ErrNotFound
	}
	return nil
}
