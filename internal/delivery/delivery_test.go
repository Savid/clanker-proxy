package delivery_test

import (
	"encoding/json"
	"fmt"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"github.com/savid/clanker-proxy/internal/delivery"
	"github.com/savid/clanker-proxy/internal/store"
	"github.com/savid/clanker-proxy/pkg/thread"
)

func TestBackoff(t *testing.T) {
	t.Parallel()

	for attempts, want := range map[int]time.Duration{
		0: 15 * time.Second, 1: 30 * time.Second, 2: time.Minute, 6: 16 * time.Minute,
		8: time.Hour, 100: time.Hour,
	} {
		if got := delivery.Backoff(attempts); got != want {
			t.Errorf("Backoff(%d) = %s, want %s", attempts, got, want)
		}
	}
}

// queue stores n outbound open events to bob, oldest first.
func queue(t *testing.T, st *store.Store, n int) []string {
	t.Helper()

	var ids []string

	for i := range n {
		id := fmt.Sprintf("dddd0000-0000-4000-8000-%012d", i)
		e := thread.Event{
			ID: id, Thread: id, From: "me", To: "bob", At: t0, Clock: 1,
			Action: thread.ActionOpen, Title: "t", Kind: thread.KindRequest,
		}

		th, err := thread.Replay([]thread.Event{e})
		if err != nil {
			t.Fatal(err)
		}

		if err = st.AddEvent(t.Context(), store.Event{Event: e, StoredAt: t0}, true, store.Projection(th, "me")); err != nil {
			t.Fatal(err)
		}

		ids = append(ids, id)
	}

	return ids
}

var t0 = time.Date(2026, 10, 7, 0, 0, 0, 0, time.UTC)

// A peer that is down gets nothing more that round, so events stay in
// order; a peer that refuses an event fails it and moves on.
func TestDrainKeepsOrder(t *testing.T) {
	t.Parallel()

	var (
		mu       sync.Mutex
		received []string
		status   = http.StatusServiceUnavailable
	)

	peer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var body struct{ ID string }
		_ = json.NewDecoder(r.Body).Decode(&body)

		mu.Lock()
		defer mu.Unlock()

		if r.Header.Get("Authorization") != "Bearer cpp_bob" {
			t.Errorf("delivered with %q", r.Header.Get("Authorization"))
		}

		received = append(received, body.ID)

		w.Header().Set("Content-Type", "application/problem+json")
		w.WriteHeader(status)
		_, _ = fmt.Fprintf(w, `{"title":"x","status":%d}`, status)
	}))
	defer peer.Close()

	st, err := store.Open(t.Context(), filepath.Join(t.TempDir(), "cp.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()

	if err = st.AddPeer(t.Context(), store.Peer{Name: "bob", URL: peer.URL, Secret: "cpp_bob", Status: store.PeerActive, AddedAt: t0}); err != nil {
		t.Fatal(err)
	}

	ids := queue(t, st, 3)
	d := delivery.New(slog.New(slog.DiscardHandler), st, delivery.Config{Now: func() time.Time { return t0 }})

	if err = d.Drain(t.Context()); err != nil {
		t.Fatal(err)
	}

	if len(received) != 1 || received[0] != ids[0] {
		t.Fatalf("while down, peer received %v; want only the first event", received)
	}

	// Refusals are permanent and do not hold up the rest.
	mu.Lock()
	received, status = nil, http.StatusUnprocessableEntity
	mu.Unlock()

	d = delivery.New(slog.New(slog.DiscardHandler), st, delivery.Config{Now: func() time.Time { return t0.Add(time.Hour) }})
	if err = d.Drain(t.Context()); err != nil {
		t.Fatal(err)
	}

	if len(received) != 3 {
		t.Fatalf("after refusals, peer received %v; want all three", received)
	}

	for _, id := range ids {
		events, evErr := st.ThreadEvents(t.Context(), id)
		if evErr != nil || events[0].Delivery.Status != store.DeliveryFailed {
			t.Errorf("%s: %+v, %v", id, events, evErr)
		}
	}
}
