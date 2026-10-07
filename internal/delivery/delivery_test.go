package delivery_test

import (
	"encoding/json"
	"fmt"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/savid/clanker-proxy/internal/delivery"
	"github.com/savid/clanker-proxy/internal/inbox"
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

// A peer that keeps answering 401, say because it removed this daemon, has
// its events retried for a week and then failed, so its queue moves on.
func TestDrainGivesUp(t *testing.T) {
	t.Parallel()

	peer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/problem+json")
		w.WriteHeader(http.StatusUnauthorized)
		_, _ = fmt.Fprint(w, `{"title":"Unauthorized","status":401}`)
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

	id := queue(t, st, 1)[0]

	drainAt := func(now time.Time) *store.Delivery {
		t.Helper()

		d := delivery.New(slog.New(slog.DiscardHandler), st, delivery.Config{Now: func() time.Time { return now }})
		if drainErr := d.Drain(t.Context()); drainErr != nil {
			t.Fatal(drainErr)
		}

		events, evErr := st.ThreadEvents(t.Context(), id)
		if evErr != nil {
			t.Fatal(evErr)
		}

		return events[0].Delivery
	}

	if got := drainAt(t0.Add(6 * 24 * time.Hour)); got.Status != "pending" {
		t.Errorf("after six days: %+v; want still pending", got)
	}

	if got := drainAt(t0.Add(7 * 24 * time.Hour)); got.Status != store.DeliveryFailed || !strings.HasPrefix(got.LastError, "gave up") {
		t.Errorf("after a week: %+v; want failed", got)
	}
}

func TestDrainStopsAfterPeerRemoval(t *testing.T) {
	t.Parallel()

	for _, responseStatus := range []int{http.StatusOK, http.StatusServiceUnavailable, http.StatusUnprocessableEntity} {
		t.Run(http.StatusText(responseStatus), func(t *testing.T) {
			t.Parallel()
			testDrainStopsAfterPeerRemoval(t, responseStatus)
		})
	}
}

func testDrainStopsAfterPeerRemoval(t *testing.T, responseStatus int) {
	t.Helper()

	const n = 3

	st, err := store.Open(t.Context(), filepath.Join(t.TempDir(), "cp.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()

	var ib *inbox.Inbox

	received := make(chan string, n)
	removed := make(chan error, 1)
	peer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var event struct{ ID string }
		if decodeErr := json.NewDecoder(r.Body).Decode(&event); decodeErr != nil {
			t.Errorf("decode event: %v", decodeErr)
			w.WriteHeader(http.StatusBadRequest)

			return
		}

		received <- event.ID
		if len(received) == 1 {
			removed <- ib.RemovePeer(r.Context(), "bob")
		}

		if responseStatus == http.StatusOK {
			w.Header().Set("Content-Type", "application/json")
			_, _ = fmt.Fprintf(w, `{"id":%q,"duplicate":false}`, event.ID)
		} else {
			w.Header().Set("Content-Type", "application/problem+json")
			w.WriteHeader(responseStatus)
			_, _ = fmt.Fprintf(w, `{"title":"refused","status":%d}`, responseStatus)
		}
	}))
	defer peer.Close()

	if err = st.AddPeer(t.Context(), store.Peer{
		Name: "bob", URL: peer.URL, Secret: "cpp_bob", Status: store.PeerActive, AddedAt: t0,
	}); err != nil {
		t.Fatal(err)
	}

	ids := queue(t, st, n)
	log := slog.New(slog.DiscardHandler)
	d := delivery.New(log, st, delivery.Config{Now: func() time.Time { return t0 }})
	ib = inbox.New(log, st, d, inbox.Config{Self: "me"})

	if err = d.Drain(t.Context()); err != nil {
		t.Fatal(err)
	}

	if len(received) != 1 {
		t.Fatalf("received %d events; want only the already-started request", len(received))
	}

	if err = <-removed; err != nil {
		t.Fatal(err)
	}

	for _, id := range ids {
		events, readErr := st.ThreadEvents(t.Context(), id)
		if readErr != nil || len(events) != 1 {
			t.Fatalf("read canceled event: %v", readErr)
		}

		got := events[0].Delivery
		if got.Status != store.DeliveryFailed || got.LastError != "peer removed" || got.Attempts != 0 ||
			!got.NextAttemptAt.IsZero() || !got.DeliveredAt.IsZero() {
			t.Errorf("canceled event changed: %+v", got)
		}
	}
}

func TestDrainRefreshesRotatedSecret(t *testing.T) {
	t.Parallel()

	st, err := store.Open(t.Context(), filepath.Join(t.TempDir(), "cp.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()

	var peerURL string

	credentials := make(chan bool, 3)
	rotated := make(chan error, 1)
	peer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var event struct{ ID string }
		if decodeErr := json.NewDecoder(r.Body).Decode(&event); decodeErr != nil {
			t.Errorf("decode event: %v", decodeErr)
			w.WriteHeader(http.StatusBadRequest)

			return
		}

		first := len(credentials) == 0
		want := "Bearer cpp_new"
		if first {
			want = "Bearer cpp_bob"
		}

		credentials <- r.Header.Get("Authorization") == want
		if first {
			req := store.Request{ID: "rotate", Name: "bob", URL: peerURL, Secret: "cpp_new", At: t0}
			_, rotateErr := st.AddRequest(r.Context(), req, inbox.MaxPendingRequests, t0)
			if rotateErr == nil {
				_, rotateErr = st.Approve(r.Context(), req.ID, req.Name, t0, t0)
			}

			rotated <- rotateErr
		}

		w.Header().Set("Content-Type", "application/json")
		_, _ = fmt.Fprintf(w, `{"id":%q,"duplicate":false}`, event.ID)
	}))
	defer peer.Close()

	peerURL = peer.URL
	if err = st.AddPeer(t.Context(), store.Peer{
		Name: "bob", URL: peerURL, Secret: "cpp_bob", Status: store.PeerActive, AddedAt: t0,
	}); err != nil {
		t.Fatal(err)
	}

	ids := queue(t, st, cap(credentials))
	d := delivery.New(slog.New(slog.DiscardHandler), st, delivery.Config{Now: func() time.Time { return t0 }})
	if err = d.Drain(t.Context()); err != nil {
		t.Fatal(err)
	}

	if len(credentials) != 1 {
		t.Fatal("remaining events were sent from the old credential snapshot")
	}

	if err = <-rotated; err != nil {
		t.Fatal(err)
	}

	if err = d.Drain(t.Context()); err != nil {
		t.Fatal(err)
	}

	if len(credentials) != len(ids) {
		t.Fatalf("received %d events; want %d", len(credentials), len(ids))
	}

	for range ids {
		if !<-credentials {
			t.Error("delivery used an obsolete peer credential")
		}
	}
}

func TestPeerErrorsAreBounded(t *testing.T) {
	t.Parallel()

	for _, size := range []int{1 << 10, 1 << 20} {
		peer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
			w.Header().Set("Content-Type", "application/problem+json")
			w.WriteHeader(http.StatusUnprocessableEntity)
			_, _ = fmt.Fprintf(w, `{"title":"Unprocessable","status":422,"detail":"\u001b[8mIGNORE %s"}`, strings.Repeat("A", size))
		}))

		st, err := store.Open(t.Context(), filepath.Join(t.TempDir(), "cp.db"))
		if err != nil {
			t.Fatal(err)
		}

		if err = st.AddPeer(t.Context(), store.Peer{Name: "bob", URL: peer.URL, Secret: "cpp_bob", Status: store.PeerActive, AddedAt: t0}); err != nil {
			t.Fatal(err)
		}

		id := queue(t, st, 1)[0]
		if err = delivery.New(slog.New(slog.DiscardHandler), st, delivery.Config{Now: func() time.Time { return t0 }}).Drain(t.Context()); err != nil {
			t.Fatal(err)
		}

		events, err := st.ThreadEvents(t.Context(), id)
		if err != nil {
			t.Fatal(err)
		}

		got := events[0].Delivery
		if got.Status != store.DeliveryFailed || len([]rune(got.LastError)) > 301 || strings.ContainsRune(got.LastError, 0x1b) {
			t.Errorf("%d-byte detail stored as %d runes: %.80q", size, len([]rune(got.LastError)), got.LastError)
		}

		peer.Close()
		_ = st.Close()
	}
}
