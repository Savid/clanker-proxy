// Package inbox is the daemon's core: it stores events from peers and from
// the owner with their thread's replayed state, runs peering requests and
// approvals, and tells subscribers when a thread changes.
package inbox

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/google/uuid"

	"github.com/savid/clanker-proxy/internal/store"
	"github.com/savid/clanker-proxy/pkg/thread"
)

// maxFutureSkew is how far ahead of this daemon's clock a peer's event may be
// dated.
const maxFutureSkew = 10 * time.Minute

// Federation is how the inbox reaches other daemons.
type Federation interface {
	// RequestPeering asks the daemon at url to become peers.
	RequestPeering(ctx context.Context, url string, req PeeringRequest) error
	// Accepted tells the daemon at url that its request was approved.
	Accepted(ctx context.Context, url, secret string) error
}

// PeeringRequest is what one daemon sends another to become peers.
type PeeringRequest struct {
	Name, URL, Secret, Note string
}

// Config holds what the inbox needs.
type Config struct {
	// Self is the owner's name.
	Self string
	// URL is where peers reach this daemon; needed to send requests.
	URL string
}

// Inbox is the daemon's core.
type Inbox struct {
	log   *slog.Logger
	store *store.Store
	fed   Federation
	self  string
	url   string
	now   func() time.Time

	// mu serialises writes, so each event replays on its thread's latest
	// state.
	mu sync.Mutex

	// Owner peering changes must keep their checked names, URLs and rollback
	// snapshots until the callback finishes. Peer callbacks do not take this
	// lock, so they can confirm while an owner operation is waiting on them.
	peerMu sync.Mutex

	subMu sync.Mutex
	subs  map[chan store.Summary]struct{}

	// wake tells the deliverer there is something due in the outbox.
	wake chan struct{}
}

// New returns an inbox over st.
func New(log *slog.Logger, st *store.Store, fed Federation, cfg Config) *Inbox {
	return &Inbox{
		log: log, store: st, fed: fed, self: cfg.Self, url: cfg.URL, now: time.Now,
		subs: map[chan store.Summary]struct{}{},
		wake: make(chan struct{}, 1),
	}
}

// Self is the owner's name.
func (b *Inbox) Self() string {
	return b.self
}

// URL is where peers reach this daemon, or "".
func (b *Inbox) URL() string {
	return b.url
}

// Wake signals when the outbox has something due.
func (b *Inbox) Wake() <-chan struct{} {
	return b.wake
}

func (b *Inbox) poke() {
	select {
	case b.wake <- struct{}{}:
	default:
	}
}

// Receipt is the result of receiving an event.
type Receipt struct {
	ID        string
	Duplicate bool
}

// Receive stores an event peer delivered. e's From and To are ignored: the
// peer sent it, to the owner.
func (b *Inbox) Receive(ctx context.Context, peer string, e thread.Event) (Receipt, error) {
	e.From, e.To = peer, b.self
	if err := e.Validate(); err != nil {
		return Receipt{}, errorf(KindInvalid, "%v", err)
	}

	if e.At.After(b.now().Add(maxFutureSkew)) {
		return Receipt{}, errorf(KindUnavailable, "event time %s is ahead of this daemon's clock; retry later",
			e.At.Format(time.RFC3339))
	}

	b.mu.Lock()
	defer b.mu.Unlock()

	// RemovePeer may have run since the secret was checked.
	if _, err := b.store.Peer(ctx, peer); errors.Is(err, store.ErrNotFound) {
		return Receipt{}, errorf(KindUnauthorized, "unknown peer secret")
	} else if err != nil {
		return Receipt{}, err
	}

	dup, err := b.duplicate(ctx, e)
	if err != nil || dup {
		return Receipt{ID: e.ID, Duplicate: dup}, err
	}

	t, err := b.next(ctx, &e, false)
	if err != nil {
		return Receipt{}, err
	}

	if err = b.store.AddEvent(ctx, store.Event{Event: e, StoredAt: b.now().UTC()}, false, store.Projection(t, b.self)); err != nil {
		return Receipt{}, err
	}

	b.log.InfoContext(ctx, "event received", "peer", peer, "thread", e.Thread, "action", e.Action)
	b.Notify(ctx, e.Thread)

	return Receipt{ID: e.ID}, nil
}

// NewThread is what the owner opens a thread with.
type NewThread struct {
	To     string
	Title  string
	Kind   thread.Kind
	Labels []string
	Body   string
}

// Open opens a thread with a peer and queues it for delivery.
func (b *Inbox) Open(ctx context.Context, n NewThread) (View, error) {
	p, err := b.Peer(ctx, n.To)
	if err != nil {
		return View{}, err
	}

	if n.Kind == "" {
		n.Kind = thread.KindRequest
	}

	id := uuid.NewString()

	return b.submit(ctx, thread.Event{
		ID: id, Thread: id, To: p.Name, Action: thread.ActionOpen,
		Title: strings.TrimSpace(n.Title), Kind: n.Kind, Labels: slices.Sorted(slices.Values(n.Labels)), Body: n.Body,
	})
}

// Act takes an action on the thread ref names, as the owner.
func (b *Inbox) Act(ctx context.Context, ref string, action thread.Action, body string) (View, error) {
	if action == thread.ActionOpen {
		return View{}, errorf(KindInvalid, "open a thread by opening it, not as an action")
	}

	id, err := b.resolve(ctx, ref)
	if err != nil {
		return View{}, err
	}

	sum, err := b.store.Summary(ctx, id)
	if err != nil {
		return View{}, err
	}

	if _, err = b.Peer(ctx, sum.Peer); err != nil {
		return View{}, err
	}

	if err = b.delivered(ctx, id); err != nil {
		return View{}, err
	}

	return b.submit(ctx, thread.Event{ID: uuid.NewString(), Thread: id, To: sum.Peer, Action: action, Body: body})
}

// delivered refuses a thread whose open the peer will never get: anything
// more sent on it would be refused as a thread they do not hold.
func (b *Inbox) delivered(ctx context.Context, id string) error {
	events, err := b.store.ThreadEvents(ctx, id)
	if err != nil {
		return err
	}

	for _, e := range events {
		if e.ID == id && e.Delivery != nil && e.Delivery.Status == store.DeliveryFailed {
			return errorf(KindConflict, "%s never received this thread (%s); send a new one", e.To, e.Delivery.LastError)
		}
	}

	return nil
}

// submit completes, checks and stores an owner's event, and queues it.
func (b *Inbox) submit(ctx context.Context, e thread.Event) (View, error) {
	e.From, e.At = b.self, b.now().UTC().Truncate(time.Second)

	b.mu.Lock()
	defer b.mu.Unlock()

	t, err := b.next(ctx, &e, true)
	if err != nil {
		return View{}, err
	}

	if err = e.Validate(); err != nil {
		return View{}, errorf(KindInvalid, "%v", err)
	}

	err = b.store.AddEvent(ctx, store.Event{Event: e, StoredAt: b.now().UTC()}, true, store.Projection(t, b.self))
	if errors.Is(err, store.ErrNotFound) {
		return View{}, errorf(KindNotFound, "%s is not a peer", e.To)
	}

	if err != nil {
		return View{}, err
	}

	b.log.InfoContext(ctx, "event queued", "peer", e.To, "thread", e.Thread, "action", e.Action)
	b.Notify(ctx, e.Thread)
	b.poke()

	return b.view(ctx, e.Thread)
}

// duplicate reports whether the event is already stored. The same ID with
// different content is a conflict.
func (b *Inbox) duplicate(ctx context.Context, e thread.Event) (bool, error) {
	stored, err := b.store.StoredEvent(ctx, e.ID)
	if errors.Is(err, store.ErrNotFound) {
		return false, nil
	}

	if err != nil {
		return false, err
	}

	was, errWas := stored.Marshal()
	now, errNow := e.Marshal()

	if errWas != nil || errNow != nil || string(was) != string(now) {
		return false, errorf(KindConflict, "event %s already exists with different content", e.ID)
	}

	return true, nil
}

// next replays e onto its thread. The event must be between the thread's
// participants. own marks the owner's event: it must apply, and it is given
// the thread's next clock, so it replays after everything the owner saw. A
// peer's event may fail to apply, but its clock must pass CheckClock.
func (b *Inbox) next(ctx context.Context, e *thread.Event, own bool) (thread.Thread, error) {
	stored, err := b.store.ThreadEvents(ctx, e.Thread)
	if err != nil {
		return thread.Thread{}, err
	}

	if e.Action == thread.ActionOpen {
		if own {
			e.Clock = 1
		}

		return opened(*e, len(stored) > 0)
	}

	if len(stored) == 0 {
		return thread.Thread{}, errorf(KindUnprocessable, "unknown thread %s", e.Thread)
	}

	events := make([]thread.Event, 0, len(stored)+1)
	for _, s := range stored {
		events = append(events, s.Event)
	}

	t, err := thread.Replay(events)
	if err != nil {
		return thread.Thread{}, fmt.Errorf("replay thread %s: %w", e.Thread, err)
	}

	if _, ok := t.RoleOf(e.From); !ok || t.Peer(e.From) != e.To {
		return thread.Thread{}, errorf(KindUnprocessable, "thread %s is not between %s and %s", e.Thread, e.From, e.To)
	}

	if own {
		e.Clock = t.NextClock()

		if err = t.Check(*e); err != nil {
			return thread.Thread{}, errorf(KindConflict, "%v; you can now %s", err, actionList(t.Allowed(e.From)))
		}
	} else if err = t.CheckClock(*e); err != nil {
		return thread.Thread{}, errorf(KindUnprocessable, "%v", err)
	}

	t, err = thread.Replay(append(events, *e))
	if err != nil {
		return thread.Thread{}, fmt.Errorf("replay thread %s: %w", e.Thread, err)
	}

	return t, nil
}

func actionList(actions []thread.Action) string {
	names := make([]string, 0, len(actions))
	for _, a := range actions {
		names = append(names, string(a))
	}

	return strings.Join(names, ", ")
}

// opened is the thread an open event starts.
func opened(e thread.Event, exists bool) (thread.Thread, error) {
	if exists {
		return thread.Thread{}, errorf(KindConflict, "thread %s already exists", e.Thread)
	}

	t, err := thread.Replay([]thread.Event{e})
	if err != nil {
		return thread.Thread{}, errorf(KindInvalid, "%v", err)
	}

	return t, nil
}

// View is a thread's summary and its events.
type View struct {
	Summary store.Summary
	Events  []ViewEvent
}

// ViewEvent is a stored event in replay order, with its effect.
type ViewEvent struct {
	store.Event
	Ignored string
}

// Thread returns the thread ref names: an ID or a unique prefix.
func (b *Inbox) Thread(ctx context.Context, ref string) (View, error) {
	id, err := b.resolve(ctx, ref)
	if err != nil {
		return View{}, err
	}

	return b.view(ctx, id)
}

func (b *Inbox) resolve(ctx context.Context, ref string) (string, error) {
	id, err := b.store.ResolveThread(ctx, ref)

	switch {
	case errors.Is(err, store.ErrNotFound):
		return "", errorf(KindNotFound, "no thread matches %q", ref)
	case errors.Is(err, store.ErrAmbiguous):
		return "", errorf(KindConflict, "%q matches more than one thread; give more of its ID", ref)
	default:
		return id, err
	}
}

func (b *Inbox) view(ctx context.Context, id string) (View, error) {
	sum, err := b.store.Summary(ctx, id)
	if err != nil {
		return View{}, err
	}

	stored, err := b.store.ThreadEvents(ctx, id)
	if err != nil {
		return View{}, err
	}

	byID := make(map[string]store.Event, len(stored))
	events := make([]thread.Event, 0, len(stored))

	for _, s := range stored {
		byID[s.ID] = s
		events = append(events, s.Event)
	}

	t, err := thread.Replay(events)
	if err != nil {
		return View{}, fmt.Errorf("replay thread %s: %w", id, err)
	}

	v := View{Summary: sum, Events: make([]ViewEvent, 0, len(t.Events))}
	for _, a := range t.Events {
		v.Events = append(v.Events, ViewEvent{Event: byID[a.ID], Ignored: a.Ignored})
	}

	return v, nil
}

// Threads lists thread summaries.
func (b *Inbox) Threads(ctx context.Context, f store.Filter) ([]store.Summary, error) {
	f.Self = b.self

	return b.store.Threads(ctx, f)
}

// Subscribe returns a channel of thread summaries, one each time a thread
// changes, and a function that ends the subscription. A subscriber that
// falls behind misses summaries rather than slowing the daemon.
func (b *Inbox) Subscribe() (<-chan store.Summary, func()) {
	ch := make(chan store.Summary, 64)

	b.subMu.Lock()
	b.subs[ch] = struct{}{}
	b.subMu.Unlock()

	return ch, func() {
		b.subMu.Lock()
		delete(b.subs, ch)
		b.subMu.Unlock()
	}
}

// Notify tells subscribers that a thread changed.
func (b *Inbox) Notify(ctx context.Context, threadID string) {
	sum, err := b.store.Summary(ctx, threadID)
	if err != nil {
		b.log.WarnContext(ctx, "notify: read thread", "thread", threadID, "error", err)

		return
	}

	b.subMu.Lock()
	defer b.subMu.Unlock()

	for ch := range b.subs {
		select {
		case ch <- sum:
		default:
		}
	}
}
