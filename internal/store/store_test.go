package store_test

import (
	"errors"
	"path/filepath"
	"slices"
	"testing"
	"time"

	"github.com/savid/clanker-proxy/internal/store"
	"github.com/savid/clanker-proxy/pkg/thread"
)

var t0 = time.Date(2026, 10, 7, 0, 0, 0, 0, time.UTC)

func open(t *testing.T) *store.Store {
	t.Helper()

	st, err := store.Open(t.Context(), filepath.Join(t.TempDir(), "cp.db"))
	if err != nil {
		t.Fatal(err)
	}

	t.Cleanup(func() { _ = st.Close() })

	return st
}

// addThread stores an open event from -> to, outbound when from is "me".
func addThread(t *testing.T, st *store.Store, id, from, to string, labels []string, at time.Time) {
	t.Helper()

	e := thread.Event{
		ID: id, Thread: id, From: from, To: to, At: at, Clock: 1,
		Action: thread.ActionOpen, Title: "t " + id[:4], Kind: thread.KindRequest, Labels: labels,
	}

	th, err := thread.Replay([]thread.Event{e})
	if err != nil {
		t.Fatal(err)
	}

	if err = st.AddEvent(t.Context(), store.Event{Event: e, StoredAt: at}, from == "me", store.Projection(th, "me")); err != nil {
		t.Fatal(err)
	}
}

func TestThreads(t *testing.T) {
	t.Parallel()

	st := open(t)
	ctx := t.Context()

	if err := st.AddPeer(ctx, store.Peer{Name: "bob", URL: "http://bob", Secret: "cpp_bob", Status: store.PeerActive, AddedAt: t0}); err != nil {
		t.Fatal(err)
	}

	addThread(t, st, "aaaa0000-0000-4000-8000-000000000001", "bob", "me", []string{"infra"}, t0)
	addThread(t, st, "aaaa1111-0000-4000-8000-000000000002", "me", "bob", nil, t0.Add(time.Minute))
	addThread(t, st, "bbbb0000-0000-4000-8000-000000000003", "bob", "me", []string{"web"}, t0.Add(2*time.Minute))

	ids := func(f store.Filter) []string {
		f.Self = "me"

		sums, err := st.Threads(ctx, f)
		if err != nil {
			t.Fatal(err)
		}

		var out []string
		for _, s := range sums {
			out = append(out, s.ID[:4])
		}

		return out
	}

	for name, tc := range map[string]struct {
		f    store.Filter
		want []string
	}{
		"all, newest first": {store.Filter{}, []string{"bbbb", "aaaa", "aaaa"}},
		"mine":              {store.Filter{Turn: "mine"}, []string{"bbbb", "aaaa"}},
		"theirs":            {store.Filter{Turn: "theirs"}, []string{"aaaa"}},
		"none":              {store.Filter{Turn: "none"}, nil},
		"label":             {store.Filter{Label: "infra"}, []string{"aaaa"}},
		"limit":             {store.Filter{Limit: 1}, []string{"bbbb"}},
	} {
		if got := ids(tc.f); !slices.Equal(got, tc.want) {
			t.Errorf("%s: %v, want %v", name, got, tc.want)
		}
	}

	if id, err := st.ResolveThread(ctx, "BBBB"); err != nil || id[:4] != "bbbb" {
		t.Errorf("resolve unique prefix = %q, %v", id, err)
	}

	if _, err := st.ResolveThread(ctx, "aaaa"); !errors.Is(err, store.ErrAmbiguous) {
		t.Errorf("resolve ambiguous: %v", err)
	}

	if _, err := st.ResolveThread(ctx, "aa%"); !errors.Is(err, store.ErrNotFound) {
		t.Errorf("resolve with a wildcard: %v", err)
	}
}

func TestOutbox(t *testing.T) {
	t.Parallel()

	st := open(t)
	ctx := t.Context()

	if err := st.AddPeer(ctx, store.Peer{Name: "bob", URL: "http://bob", Secret: "cpp_bob", Status: store.PeerActive, AddedAt: t0}); err != nil {
		t.Fatal(err)
	}

	first, second := "cccc0000-0000-4000-8000-000000000001", "cccc0000-0000-4000-8000-000000000002"
	addThread(t, st, first, "me", "bob", nil, t0)
	addThread(t, st, second, "me", "bob", nil, t0)

	due, err := st.Due(ctx, t0, 10)
	if err != nil || len(due) != 2 || due[0].Event.ID != first || due[0].URL != "http://bob" || due[0].Secret != "cpp_bob" {
		t.Fatalf("Due = %+v, %v", due, err)
	}

	if err = st.Retry(ctx, first, "down", t0.Add(time.Minute)); err != nil {
		t.Fatal(err)
	}

	if err = st.Delivered(ctx, second, t0); err != nil {
		t.Fatal(err)
	}

	if later, _ := st.Due(ctx, t0, 10); len(later) != 0 {
		t.Errorf("Due before retry = %+v", later)
	}

	if next, _ := st.NextDue(ctx); !next.Equal(t0.Add(time.Minute)) {
		t.Errorf("NextDue = %s", next)
	}

	// Removing the peer fails what was still pending.
	if err = st.RemovePeer(ctx, "bob"); err != nil {
		t.Fatal(err)
	}

	events, err := st.ThreadEvents(ctx, first)
	if err != nil || events[0].Delivery.Status != store.DeliveryFailed || events[0].Delivery.LastError != "peer removed" {
		t.Errorf("after removal = %+v, %v", events, err)
	}

	sum, err := st.Summary(ctx, second)
	if err != nil || sum.Undelivered != 0 || sum.Failed != 0 {
		t.Errorf("delivered summary = %+v, %v", sum, err)
	}
}

func TestPeering(t *testing.T) {
	t.Parallel()

	st := open(t)
	ctx := t.Context()

	req := func(id, url string, at time.Time) store.Request {
		return store.Request{ID: id, Name: "alice", URL: url, Secret: "cpp_" + id, At: at}
	}

	if err := st.AddRequest(ctx, req("old", "http://a", t0), 2, t0.Add(-time.Hour)); err != nil {
		t.Fatal(err)
	}

	// A second request from the same URL does not replace the first.
	if err := st.AddRequest(ctx, req("new", "http://a", t0.Add(time.Minute)), 2, t0.Add(-time.Hour)); err != nil {
		t.Fatal(err)
	}

	if err := st.AddRequest(ctx, req("full", "http://b", t0), 2, t0.Add(-time.Hour)); !errors.Is(err, store.ErrFull) {
		t.Fatalf("third request: %v", err)
	}

	// Expired requests make room.
	if err := st.AddRequest(ctx, req("later", "http://b", t0.Add(48*time.Hour)), 2, t0.Add(time.Hour)); err != nil {
		t.Fatal(err)
	}

	if reqs, _ := st.Requests(ctx, t0.Add(time.Hour)); len(reqs) != 1 || reqs[0].ID != "later" {
		t.Fatalf("requests = %+v", reqs)
	}

	if _, err := st.Approve(ctx, "later", "al", t0, t0.Add(72*time.Hour)); !errors.Is(err, store.ErrNotFound) {
		t.Errorf("approve expired: %v", err)
	}

	p, err := st.Approve(ctx, "later", "al", t0, t0)
	if err != nil || p.Status != store.PeerActive || p.Secret != "cpp_later" {
		t.Fatalf("Approve = %+v, %v", p, err)
	}

	if _, err = st.Approve(ctx, "later", "al", t0, t0); !errors.Is(err, store.ErrNotFound) {
		t.Errorf("approve twice: %v", err)
	}

	if err = st.AddPeer(ctx, store.Peer{Name: "al", URL: "http://x", Secret: "cpp_x", Status: store.PeerRequested, AddedAt: t0}); !errors.Is(err, store.ErrExists) {
		t.Errorf("duplicate name: %v", err)
	}

	if err = st.AddPeer(ctx, store.Peer{Name: "bo", URL: "http://bo", Secret: "cpp_bo", Status: store.PeerRequested, AddedAt: t0}); err != nil {
		t.Fatal(err)
	}

	if changed, actErr := st.Activate(ctx, "bo", t0); actErr != nil || !changed {
		t.Errorf("Activate = %v, %v", changed, actErr)
	}

	if changed, _ := st.Activate(ctx, "bo", t0); changed {
		t.Error("Activate twice reported a change")
	}
}

// An event queued behind one that is waiting to retry waits too, so a later
// event never overtakes an earlier one.
func TestRetryHoldsThePeersQueue(t *testing.T) {
	t.Parallel()

	st := open(t)
	ctx := t.Context()

	if err := st.AddPeer(ctx, store.Peer{Name: "bob", URL: "http://bob", Secret: "cpp_bob", Status: store.PeerActive, AddedAt: t0}); err != nil {
		t.Fatal(err)
	}

	first, second := "eeee0000-0000-4000-8000-000000000001", "eeee0000-0000-4000-8000-000000000002"
	addThread(t, st, first, "me", "bob", nil, t0)

	if err := st.Retry(ctx, first, "down", t0.Add(time.Minute)); err != nil {
		t.Fatal(err)
	}

	addThread(t, st, second, "me", "bob", nil, t0.Add(10*time.Second))

	if due, _ := st.Due(ctx, t0.Add(30*time.Second), 10); len(due) != 0 {
		t.Fatalf("due while the first waits: %+v", due)
	}

	if next, _ := st.NextDue(ctx); !next.Equal(t0.Add(time.Minute)) {
		t.Fatalf("NextDue = %s; want the first's retry, not the waiting second", next)
	}

	due, _ := st.Due(ctx, t0.Add(time.Minute), 10)
	if len(due) != 2 || due[0].Event.ID != first {
		t.Fatalf("due after the wait: %+v", due)
	}
}

// Activating a peer reschedules its queue once, when it was requested; an
// active peer's backoff is left alone.
func TestActivateReschedulesOnce(t *testing.T) {
	t.Parallel()

	st := open(t)
	ctx := t.Context()

	if err := st.AddPeer(ctx, store.Peer{Name: "bob", URL: "http://bob", Secret: "cpp_bob", Status: store.PeerActive, AddedAt: t0}); err != nil {
		t.Fatal(err)
	}

	id := "ffff0000-0000-4000-8000-000000000001"
	addThread(t, st, id, "me", "bob", nil, t0)

	if err := st.Retry(ctx, id, "down", t0.Add(time.Hour)); err != nil {
		t.Fatal(err)
	}

	if changed, err := st.Activate(ctx, "bob", t0); err != nil || changed {
		t.Fatalf("Activate(active) = %v, %v", changed, err)
	}

	if due, _ := st.Due(ctx, t0, 10); len(due) != 0 {
		t.Fatalf("activating an active peer reset its backoff: %+v", due)
	}
}

// Unapprove puts back exactly what Approve replaced.
func TestUnapprove(t *testing.T) {
	t.Parallel()

	st := open(t)
	ctx := t.Context()

	old := store.Peer{Name: "bob", URL: "http://bob", Secret: "cpp_old", Status: store.PeerActive, AddedAt: t0}
	if err := st.AddPeer(ctx, old); err != nil {
		t.Fatal(err)
	}

	r := store.Request{ID: "r1", Name: "bob", URL: "http://bob", Secret: "cpp_new", Note: "hi", At: t0}
	if err := st.AddRequest(ctx, r, 5, t0.Add(-time.Hour)); err != nil {
		t.Fatal(err)
	}

	p, err := st.Approve(ctx, "r1", "bob", t0, t0.Add(-time.Hour))
	if err != nil || p.Secret != "cpp_new" {
		t.Fatalf("Approve = %+v, %v", p, err)
	}

	if err = st.Unapprove(ctx, p, &old, r); err != nil {
		t.Fatal(err)
	}

	got, err := st.Peer(ctx, "bob")
	if err != nil || got.Secret != "cpp_old" {
		t.Errorf("peer after undo = %+v, %v", got, err)
	}

	back, err := st.Request(ctx, "r1", t0.Add(-time.Hour))
	if err != nil || back.Secret != "cpp_new" || back.Note != "hi" {
		t.Errorf("request after undo = %+v, %v", back, err)
	}
}
