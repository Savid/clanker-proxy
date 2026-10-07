package inbox_test

import (
	"context"
	"log/slog"
	"net/http"
	"path/filepath"
	"testing"
	"time"

	"github.com/savid/clanker-proxy/internal/inbox"
	"github.com/savid/clanker-proxy/internal/store"
	"github.com/savid/clanker-proxy/internal/testutil/inboxtest"
)

type cancelConfirmation struct{ cancel context.CancelFunc }

func (cancelConfirmation) RequestPeering(context.Context, string, inbox.PeeringRequest) error {
	return nil
}

func (f cancelConfirmation) Accepted(ctx context.Context, _, _ string) error {
	f.cancel()

	return ctx.Err()
}

func TestApproveCanceledConfirmation(t *testing.T) {
	t.Parallel()

	for _, previousStatus := range []string{"", store.PeerRequested, store.PeerActive} {
		t.Run("previous="+previousStatus, func(t *testing.T) {
			t.Parallel()
			testApproveCanceledConfirmation(t, previousStatus)
		})
	}
}

func testApproveCanceledConfirmation(t *testing.T, previousStatus string) {
	t.Helper()

	st, err := store.Open(t.Context(), filepath.Join(t.TempDir(), "cp.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()

	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()

	ib := inbox.New(slog.New(slog.DiscardHandler), st, cancelConfirmation{cancel: cancel},
		inbox.Config{Self: "alice", URL: "http://alice.test"})
	previous := store.Peer{
		Name: "bob", URL: "http://bob.test", Secret: inbox.NewSecret(inbox.PeerPrefix),
		Status: previousStatus, AddedAt: time.Now().UTC(),
	}
	if previousStatus != "" {
		if err = st.AddPeer(t.Context(), previous); err != nil {
			t.Fatal(err)
		}
	}

	offered := inbox.NewSecret(inbox.PeerPrefix)
	if _, err = ib.RequestReceived(t.Context(), inbox.PeeringRequest{
		Name: "bob", URL: previous.URL, Secret: offered, Note: "please connect",
	}); err != nil {
		t.Fatal(err)
	}

	reqs, err := ib.Requests(t.Context())
	if err != nil || len(reqs) != 1 {
		t.Fatalf("pending requests: count %d, %v", len(reqs), err)
	}

	original := reqs[0]
	if _, err = ib.Approve(ctx, original.ID, ""); err == nil {
		t.Fatal("interrupted confirmation succeeded")
	}

	if _, err = ib.PeerBySecret(t.Context(), offered); status(t, err) != http.StatusUnauthorized {
		t.Error("unconfirmed credential is still accepted")
	}

	restored, err := ib.Peer(t.Context(), "bob")
	if previousStatus == "" {
		if status(t, err) != http.StatusNotFound {
			t.Error("unconfirmed peer was not removed")
		}
	} else if err != nil || restored != previous {
		t.Error("previous peer was not restored")
	}

	if reqs, err = ib.Requests(t.Context()); err != nil || len(reqs) != 1 || reqs[0] != original {
		t.Fatalf("request was not restored: count %d, %v", len(reqs), err)
	}

	if _, err = ib.Approve(t.Context(), original.ID, ""); err != nil {
		t.Fatalf("retrying approval: %v", err)
	}
}

func TestAddPeerDuplicateURL(t *testing.T) {
	t.Parallel()

	for _, peerStatus := range []string{store.PeerRequested, store.PeerActive} {
		t.Run(peerStatus, func(t *testing.T) {
			t.Parallel()

			ib := inboxtest.New(t)
			if peerStatus == store.PeerRequested {
				ib.RequestedPeer(t, "bob")
			} else {
				ib.ActivePeer(t, "bob")
			}

			for _, url := range []string{"http://bob.test", "http://bob.test/"} {
				if _, err := ib.AddPeer(t.Context(), "robert", url, ""); status(t, err) != http.StatusConflict {
					t.Errorf("duplicate URL %s: %v", url, err)
				}
			}
		})
	}
}
