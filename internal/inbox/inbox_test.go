package inbox_test

import (
	"net/http"
	"testing"
	"time"

	"github.com/google/uuid"

	"github.com/savid/clanker-proxy/internal/inbox"
	"github.com/savid/clanker-proxy/internal/testutil/inboxtest"
	"github.com/savid/clanker-proxy/pkg/thread"
)

func status(t *testing.T, err error) int {
	t.Helper()

	if err == nil {
		return http.StatusOK
	}

	code, _ := inbox.HTTPStatus(err)

	return code
}

func openFrom(id string, at time.Time) thread.Event {
	return thread.Event{
		ID: id, Thread: id, At: at, Clock: 1, Action: thread.ActionOpen, Title: "Hello", Kind: thread.KindRequest,
	}
}

func TestReceive(t *testing.T) {
	t.Parallel()

	ib := inboxtest.New(t)
	ib.ActivePeer(t, "bob")

	now := time.Now().UTC().Truncate(time.Second)
	id := uuid.NewString()

	r, err := ib.Receive(t.Context(), "bob", openFrom(id, now))
	if err != nil || r.Duplicate {
		t.Fatalf("first delivery = %+v, %v", r, err)
	}

	// The same event again is harmless.
	if r, err = ib.Receive(t.Context(), "bob", openFrom(id, now)); err != nil || !r.Duplicate {
		t.Fatalf("redelivery = %+v, %v", r, err)
	}

	// The same ID with different content is a conflict.
	changed := openFrom(id, now)
	changed.Title = "Changed"

	if _, err = ib.Receive(t.Context(), "bob", changed); status(t, err) != http.StatusConflict {
		t.Errorf("reused ID: %v", err)
	}

	// An event dated well into the future is refused.
	if _, err = ib.Receive(t.Context(), "bob", openFrom(uuid.NewString(), now.Add(time.Hour))); status(t, err) != http.StatusUnprocessableEntity {
		t.Errorf("future event: %v", err)
	}

	// A comment on a thread this daemon does not hold is refused.
	stray := thread.Event{ID: uuid.NewString(), Thread: uuid.NewString(), At: now, Clock: 2, Action: thread.ActionComment}
	if _, err = ib.Receive(t.Context(), "bob", stray); status(t, err) != http.StatusUnprocessableEntity {
		t.Errorf("unknown thread: %v", err)
	}
}

func TestOwnerActions(t *testing.T) {
	t.Parallel()

	ib := inboxtest.New(t)
	ib.ActivePeer(t, "bob")

	if _, err := ib.Open(t.Context(), inbox.NewThread{To: "carol", Title: "Hi"}); status(t, err) != http.StatusNotFound {
		t.Errorf("open to a stranger: %v", err)
	}

	v, err := ib.Open(t.Context(), inbox.NewThread{To: "bob", Title: "Hi", Labels: []string{"z", "a"}})
	if err != nil {
		t.Fatal(err)
	}

	if l := v.Summary.Labels; len(l) != 2 || l[0] != "a" {
		t.Errorf("labels = %v, want sorted", l)
	}

	// The sender may not do the recipient's part.
	if _, err = ib.Act(t.Context(), v.Summary.ID[:8], thread.ActionResolve, "done"); status(t, err) != http.StatusConflict {
		t.Errorf("sender resolve: %v", err)
	}

	v, err = ib.Act(t.Context(), v.Summary.ID, thread.ActionComment, "more")
	if err != nil || v.Events[1].Clock != 2 {
		t.Fatalf("comment = %+v, %v", v.Events, err)
	}

	if err = ib.RemovePeer(t.Context(), "bob"); err != nil {
		t.Fatal(err)
	}

	// Nothing is queued for a peer that is gone.
	if _, err = ib.Act(t.Context(), v.Summary.ID, thread.ActionComment, "anyone?"); status(t, err) != http.StatusNotFound {
		t.Errorf("act after the peer left: %v", err)
	}
}

func TestPeering(t *testing.T) {
	t.Parallel()

	ib := inboxtest.New(t)
	ib.ActivePeer(t, "bob")

	secret := inbox.NewSecret(inbox.PeerPrefix)

	code, err := ib.RequestReceived(t.Context(), inbox.PeeringRequest{Name: "Bob", URL: "http://elsewhere.test", Secret: secret})
	if err != nil || code != inbox.Code(secret) {
		t.Fatalf("RequestReceived = %q, %v", code, err)
	}

	reqs, err := ib.Requests(t.Context())
	if err != nil || len(reqs) != 1 || reqs[0].Name != "bob" {
		t.Fatalf("Requests = %+v, %v", reqs, err)
	}

	// "bob" is taken by a peer at another URL.
	if _, err = ib.Approve(t.Context(), reqs[0].ID, ""); status(t, err) != http.StatusConflict {
		t.Errorf("approve onto a taken name: %v", err)
	}

	// The owner's own name is not a peer's.
	if _, err = ib.Approve(t.Context(), reqs[0].ID, "owner"); status(t, err) != http.StatusUnprocessableEntity {
		t.Errorf("approve as the owner's name: %v", err)
	}

	// The requester's daemon cannot be reached to confirm, so the approval
	// is undone and the request waits again.
	if _, err = ib.Approve(t.Context(), reqs[0].ID, "robert"); status(t, err) != http.StatusBadGateway {
		t.Errorf("approve unconfirmed: %v", err)
	}

	if _, err = ib.Peer(t.Context(), "robert"); status(t, err) != http.StatusNotFound {
		t.Errorf("unconfirmed request became a peer: %v", err)
	}

	if reqs, _ = ib.Requests(t.Context()); len(reqs) != 1 {
		t.Errorf("request after a failed approval = %+v", reqs)
	}

	// A peer already at that URL under another name must be removed first.
	if _, err = ib.RequestReceived(t.Context(), inbox.PeeringRequest{Name: "b", URL: "http://bob.test", Secret: inbox.NewSecret(inbox.PeerPrefix)}); err != nil {
		t.Fatal(err)
	}

	reqs, _ = ib.Requests(t.Context())
	if _, err = ib.Approve(t.Context(), reqs[1].ID, "b"); status(t, err) != http.StatusConflict {
		t.Errorf("approve a second name for bob's URL: %v", err)
	}

	for _, bad := range []inbox.PeeringRequest{
		{Name: "not a name", URL: "http://x.test", Secret: secret},
		{Name: "x", URL: "ftp://x.test", Secret: secret},
		{Name: "x", URL: "http://x.test", Secret: "cpp_short"},
	} {
		if _, err = ib.RequestReceived(t.Context(), bad); status(t, err) != http.StatusBadRequest {
			t.Errorf("RequestReceived(%+v): %v", bad, err)
		}
	}
}
