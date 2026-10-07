// Package delivery is how this daemon calls other daemons: it drains the
// outbox, sending each peer its events in order and retrying with backoff
// until each is stored, refused, or a week old, and it carries peering
// requests and approvals. Webhooks separately sends the owner's signed
// notifications to their configured endpoints.
package delivery

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/url"
	"strings"
	"time"
	"unicode"

	"github.com/ogen-go/ogen/ogenerrors"

	"github.com/savid/clanker-proxy/api/rest"
	"github.com/savid/clanker-proxy/internal/inbox"
	"github.com/savid/clanker-proxy/internal/store"
	"github.com/savid/clanker-proxy/pkg/thread"
)

const (
	batch       = 100
	httpTimeout = 15 * time.Second
	minBackoff  = 15 * time.Second
	maxBackoff  = time.Hour
	// idle is the longest the deliverer sleeps with nothing due, in case a
	// wake-up was missed.
	idle = time.Minute
	// giveUp is how long an event is retried: as long as a peering request
	// waits, so a peer who never approves, or who removed this daemon,
	// does not hold its queue forever.
	giveUp = inbox.RequestTTL
)

// Notifier is told when a thread's delivery state changes.
type Notifier interface {
	Notify(ctx context.Context, threadID string)
}

// Config holds what the deliverer needs.
type Config struct {
	// Now returns the current time; nil takes time.Now.
	Now func() time.Time
}

// Deliverer drains the outbox and implements inbox.Federation.
type Deliverer struct {
	log    *slog.Logger
	store  *store.Store
	notify Notifier
	wake   <-chan struct{}
	now    func() time.Time
	http   *http.Client
}

var _ inbox.Federation = (*Deliverer)(nil)

var errRedirect = errors.New("peer redirected the request; use its direct URL")

// New returns a deliverer. The inbox it serves needs it first, so Attach
// connects the two afterwards.
func New(log *slog.Logger, st *store.Store, cfg Config) *Deliverer {
	if cfg.Now == nil {
		cfg.Now = time.Now
	}

	return &Deliverer{log: log, store: st, now: cfg.Now, http: &http.Client{
		Timeout: httpTimeout,
		// The shared secret may be in a header or a replayable request body.
		CheckRedirect: func(*http.Request, []*http.Request) error { return errRedirect },
	}}
}

// Attach connects the deliverer to its inbox: notify hears of delivery
// changes, and a value on wake starts a drain.
func (d *Deliverer) Attach(notify Notifier, wake <-chan struct{}) {
	d.notify, d.wake = notify, wake
}

// secret is a client security source that presents one peer's secret, and
// nothing for operations that need none.
type secret string

func (secret) OwnerToken(context.Context, rest.OperationName) (rest.OwnerToken, error) {
	return rest.OwnerToken{}, ogenerrors.ErrSkipClientSecurity
}

func (secret) AgentToken(context.Context, rest.OperationName) (rest.AgentToken, error) {
	return rest.AgentToken{}, ogenerrors.ErrSkipClientSecurity
}

func (s secret) PeerSecret(context.Context, rest.OperationName) (rest.PeerSecret, error) {
	if s == "" {
		return rest.PeerSecret{}, ogenerrors.ErrSkipClientSecurity
	}

	return rest.PeerSecret{Token: string(s)}, nil
}

type responseClient struct {
	client *http.Client
	status int
}

// maxPeerResponse bounds what is read from a peer's daemon. Its answers are
// small; a larger one is cut off and fails to decode.
const maxPeerResponse = 64 << 10

// maxPeerError bounds the peer-supplied text kept with a failed delivery,
// which cpctl shows to the owner's agent.
const maxPeerError = 300

func (c *responseClient) Do(req *http.Request) (*http.Response, error) {
	resp, err := c.client.Do(req) //nolint:gosec // peer URLs are added or approved by the owner; redirects are refused
	if resp != nil {
		c.status = resp.StatusCode
		resp.Body = limitedBody{Reader: io.LimitReader(resp.Body, maxPeerResponse), Closer: resp.Body}
	}

	return resp, err
}

type limitedBody struct {
	io.Reader
	io.Closer
}

// clip keeps a peer-influenced message short and printable.
func clip(s string) string {
	s = strings.Map(func(r rune) rune {
		if unicode.IsControl(r) {
			return ' '
		}

		return r
	}, s)

	if r := []rune(s); len(r) > maxPeerError {
		return string(r[:maxPeerError]) + "…"
	}

	return s
}

func (d *Deliverer) client(baseURL, peerSecret string) (*rest.Client, *responseClient, error) {
	response := &responseClient{client: d.http}
	c, err := rest.NewClient(baseURL, secret(peerSecret), rest.WithClient(response))
	if err != nil {
		return nil, nil, fmt.Errorf("peer url %q: %w", baseURL, err)
	}

	return c, response, nil
}

// RequestPeering asks the daemon at baseURL to become peers.
func (d *Deliverer) RequestPeering(ctx context.Context, baseURL string, req inbox.PeeringRequest) error {
	c, _, err := d.client(baseURL, "")
	if err != nil {
		return err
	}

	self, err := url.Parse(req.URL)
	if err != nil {
		return fmt.Errorf("own url: %w", err)
	}

	in := &rest.PeeringRequest{Name: rest.Name(req.Name), URL: rest.DaemonURL(*self), Secret: req.Secret}
	if req.Note != "" {
		in.Note = rest.NewOptNote(rest.Note(req.Note))
	}

	if _, err = c.RequestPeering(ctx, in); err != nil {
		return describe(err)
	}

	return nil
}

// Accepted tells the daemon at baseURL its request was approved.
func (d *Deliverer) Accepted(ctx context.Context, baseURL, peerSecret string) error {
	c, _, err := d.client(baseURL, peerSecret)
	if err != nil {
		return err
	}

	return describe(c.NotifyPeeringAccepted(ctx))
}

// Run delivers until ctx is cancelled.
func (d *Deliverer) Run(ctx context.Context) error {
	timer := time.NewTimer(0)
	defer timer.Stop()

	for {
		select {
		case <-ctx.Done():
			return nil
		case <-d.wake:
		case <-timer.C:
		}

		if err := d.Drain(ctx); err != nil && ctx.Err() == nil {
			d.log.ErrorContext(ctx, "deliver", "error", err)
		}

		timer.Reset(d.sleep(ctx))
	}
}

// sleep is how long until the next event is due, at most idle.
func (d *Deliverer) sleep(ctx context.Context) time.Duration {
	next, err := d.store.NextDue(ctx)
	if err != nil || next.IsZero() {
		return idle
	}

	return min(max(next.Sub(d.now()), 0), idle)
}

// Drain attempts every due event once. A peer whose event fails to arrive
// gets nothing more this round, so its events stay in order.
func (d *Deliverer) Drain(ctx context.Context) error {
	due, err := d.store.Due(ctx, d.now(), batch)
	if err != nil {
		return err
	}

	blocked := map[string]bool{}

	for _, o := range due {
		if ctx.Err() != nil {
			return ctx.Err()
		}

		if blocked[o.Peer] {
			continue
		}

		pending, checkErr := d.store.Pending(ctx, o)
		if checkErr != nil {
			return checkErr
		}

		if !pending {
			continue
		}

		retry, sendErr := d.deliver(ctx, o)
		if err = d.record(ctx, o, retry, sendErr); err != nil {
			return err
		}

		if sendErr != nil && retry {
			blocked[o.Peer] = true
		}

		if d.notify != nil {
			d.notify.Notify(ctx, o.Event.Thread)
		}
	}

	return nil
}

// deliver posts one event. It reports whether a failure is worth retrying.
func (d *Deliverer) deliver(ctx context.Context, o store.Outgoing) (bool, error) {
	c, response, err := d.client(o.URL, o.Secret)
	if err != nil {
		return false, err
	}

	_, err = c.DeliverEvent(ctx, wire(o.Event))
	if err == nil {
		return false, nil
	}

	// A proxy can refuse with HTML or an empty body, so finality cannot
	// depend on decoding a Problem. A 401 may pass once peering is approved.
	switch response.status {
	case http.StatusBadRequest, http.StatusConflict, http.StatusUnprocessableEntity, http.StatusRequestEntityTooLarge:
		return false, describe(err)
	}

	return true, describe(err)
}

// wire is an event as daemons exchange it: without From and To, which the
// receiver knows from the secret.
func wire(e thread.Event) *rest.Event {
	w := &rest.Event{ID: rest.ID(e.ID), Thread: rest.ID(e.Thread), At: e.At, Clock: e.Clock, Action: rest.Action(e.Action)}
	for _, l := range e.Labels {
		w.Labels = append(w.Labels, rest.Label(l))
	}

	if e.Title != "" {
		w.Title = rest.NewOptTitle(rest.Title(e.Title))
	}

	if e.Kind != "" {
		w.Kind = rest.NewOptThreadKind(rest.ThreadKind(e.Kind))
	}

	if e.Body != "" {
		w.Body = rest.NewOptBody(rest.Body(e.Body))
	}

	return w
}

// describe turns a peer's problem into "status: detail".
func describe(err error) error {
	p, ok := errors.AsType[*rest.ProblemStatusCode](err)
	if !ok {
		return err
	}

	detail := p.Response.Title
	if p.Response.Detail.Set {
		detail += ": " + p.Response.Detail.Value
	}

	return fmt.Errorf("%d %s", p.StatusCode, clip(detail))
}

func (d *Deliverer) record(ctx context.Context, o store.Outgoing, retry bool, err error) error {
	now := d.now().UTC()
	id := o.Event.ID

	if err != nil {
		err = errors.New(clip(err.Error()))
	}

	switch {
	case err == nil:
		d.log.InfoContext(ctx, "event delivered", "peer", o.Peer, "event", id)

		return d.store.Delivered(ctx, id, now)
	case retry && now.Sub(o.StoredAt) >= giveUp:
		d.log.ErrorContext(ctx, "delivery abandoned", "peer", o.Peer, "event", id, "after", giveUp, "error", err)

		return d.store.Failed(ctx, id, fmt.Sprintf("gave up after %s: %v", giveUp, err))
	case retry:
		wait := Backoff(o.Attempts)
		d.log.WarnContext(ctx, "delivery failed; will retry", "peer", o.Peer, "event", id,
			"attempt", o.Attempts+1, "retry_in", wait, "error", err)

		return d.store.Retry(ctx, id, err.Error(), now.Add(wait))
	default:
		d.log.ErrorContext(ctx, "delivery refused", "peer", o.Peer, "event", id, "error", err)

		return d.store.Failed(ctx, id, err.Error())
	}
}

// Backoff is the wait after a failed attempt, given the attempts before it:
// 15 s, doubling, at most an hour.
func Backoff(attempts int) time.Duration {
	wait := minBackoff
	for range attempts {
		if wait >= maxBackoff/2 {
			return maxBackoff
		}

		wait *= 2
	}

	return wait
}
