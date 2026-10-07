// Package inboxtest builds an inbox over a temporary database, for API
// tests.
package inboxtest

import (
	"context"
	"errors"
	"log/slog"
	"path/filepath"
	"testing"
	"time"

	"github.com/savid/clanker-proxy/internal/inbox"
	"github.com/savid/clanker-proxy/internal/store"
)

// self is the test inbox's owner.
const self = "owner"

// offline is a federation that reaches nobody.
type offline struct{}

var errOffline = errors.New("offline")

func (offline) RequestPeering(context.Context, string, inbox.PeeringRequest) error { return errOffline }

func (offline) Accepted(context.Context, string, string) error { return errOffline }

// Inbox is a test inbox and the means to set it up.
type Inbox struct {
	*inbox.Inbox

	store *store.Store
}

// New returns an inbox owned by Self, with no peers, that reaches nobody.
func New(t *testing.T) Inbox {
	t.Helper()

	st, err := store.Open(t.Context(), filepath.Join(t.TempDir(), "cp.db"))
	if err != nil {
		t.Fatal(err)
	}

	t.Cleanup(func() { _ = st.Close() })

	ib := inbox.New(slog.New(slog.DiscardHandler), st, offline{}, inbox.Config{Self: self, URL: "http://owner.test"})

	return Inbox{Inbox: ib, store: st}
}

// ActivePeer adds an active peer and returns its secret.
func (i Inbox) ActivePeer(t *testing.T, name string) string {
	t.Helper()

	secret := inbox.NewSecret(inbox.PeerPrefix)

	p := store.Peer{Name: name, URL: "http://" + name + ".test", Secret: secret, Status: store.PeerActive, AddedAt: time.Now()}
	if err := i.store.AddPeer(t.Context(), p); err != nil {
		t.Fatal(err)
	}

	return secret
}
