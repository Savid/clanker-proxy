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

func TestReopenedThreadsFromPeerAreCapped(t *testing.T) {
	t.Parallel()

	ib := inboxtest.New(t)
	ib.ActivePeer(t, "bob")
	ids := closedPeerThreads(t, ib, inbox.MaxOpenThreadsFromPeer+1)
	for _, id := range ids[:inbox.MaxOpenThreadsFromPeer] {
		if _, err := ib.Receive(t.Context(), "bob", peerAction(id, 3, thread.ActionReopen)); err != nil {
			t.Fatal(err)
		}
	}

	id := ids[inbox.MaxOpenThreadsFromPeer]
	e := peerAction(id, 3, thread.ActionReopen)
	if _, err := ib.Receive(t.Context(), "bob", e); status(t, err) != http.StatusUnprocessableEntity {
		t.Errorf("peer reopened past the cap: %v", err)
	}
	if _, err := ib.Act(t.Context(), id, []string{"bob"}, thread.ActionReopen, "Still needed"); status(t, err) != http.StatusConflict {
		t.Errorf("owner reopened incoming thread past the cap: %v", err)
	}
	if v, err := ib.Thread(t.Context(), id, nil); err != nil || v.Summary.State != thread.StateClosed || len(v.Events) != 2 {
		t.Fatalf("refused reopen changed the thread: %+v, %v", v, err)
	}

	checkActionsAtPeerLimit(t, ib, ids[0])
	checkLocalThreadsAtPeerLimit(t, ib)

	if _, err := ib.Receive(t.Context(), "bob", peerAction(ids[1], 4, thread.ActionClose)); err != nil {
		t.Fatal(err)
	}
	if _, err := ib.Receive(t.Context(), "bob", e); err != nil {
		t.Fatalf("reopen after a slot was freed: %v", err)
	}
}

func closedPeerThreads(t *testing.T, ib inboxtest.Inbox, n int) []string {
	t.Helper()

	ids := make([]string, n)
	for i := range ids {
		ids[i] = uuid.NewString()
		if _, err := ib.Receive(t.Context(), "bob", openFrom(ids[i], time.Now().UTC().Truncate(time.Second))); err != nil {
			t.Fatal(err)
		}
		if _, err := ib.Receive(t.Context(), "bob", peerAction(ids[i], 2, thread.ActionClose)); err != nil {
			t.Fatal(err)
		}
	}

	return ids
}

func peerAction(id string, clock int64, action thread.Action) thread.Event {
	return thread.Event{
		ID: uuid.NewString(), Thread: id, At: time.Now().UTC().Truncate(time.Second), Clock: clock,
		Action: action, Body: "Still needed",
	}
}

func checkActionsAtPeerLimit(t *testing.T, ib inboxtest.Inbox, id string) {
	t.Helper()

	if _, err := ib.Receive(t.Context(), "bob", peerAction(id, 4, thread.ActionComment)); err != nil {
		t.Fatalf("reply at the cap: %v", err)
	}
	if _, err := ib.Act(t.Context(), id, nil, thread.ActionResolve, "Done"); err != nil {
		t.Fatalf("resolve at the cap: %v", err)
	}
	if _, err := ib.Receive(t.Context(), "bob", peerAction(id, 6, thread.ActionReopen)); err != nil {
		t.Fatalf("reopen already-active thread at the cap: %v", err)
	}
	if _, err := ib.Act(t.Context(), id, nil, thread.ActionClose, ""); err != nil {
		t.Fatalf("owner closing incoming thread at the cap: %v", err)
	}
	if _, err := ib.Act(t.Context(), id, []string{"bob"}, thread.ActionReopen, "Still needed"); err != nil {
		t.Fatalf("owner reopening after freeing a slot: %v", err)
	}
}

func checkLocalThreadsAtPeerLimit(t *testing.T, ib inboxtest.Inbox) {
	t.Helper()

	v, err := ib.Open(t.Context(), inbox.NewThread{To: "bob", Title: "Owner's request"})
	if err != nil {
		t.Fatal(err)
	}
	id := v.Summary.ID
	if _, err = ib.Act(t.Context(), id, nil, thread.ActionClose, ""); err != nil {
		t.Fatal(err)
	}
	if _, err = ib.Act(t.Context(), id, nil, thread.ActionReopen, "Still needed"); err != nil {
		t.Fatalf("owner reopening outgoing thread at the cap: %v", err)
	}
	if _, err = ib.Receive(t.Context(), "bob", peerAction(id, 4, thread.ActionClose)); err != nil {
		t.Fatal(err)
	}
	if _, err = ib.Receive(t.Context(), "bob", peerAction(id, 5, thread.ActionReopen)); err != nil {
		t.Fatalf("peer reopening outgoing thread at the cap: %v", err)
	}
}
