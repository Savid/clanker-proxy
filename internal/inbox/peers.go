package inbox

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"fmt"
	"net/url"
	"regexp"
	"strings"
	"time"

	"github.com/google/uuid"

	"github.com/savid/clanker-proxy/internal/store"
	"github.com/savid/clanker-proxy/pkg/thread"
)

// Secret prefixes, so a token's kind is visible and the two are never
// confused.
const (
	OwnerPrefix = "cpo_"
	PeerPrefix  = "cpp_"
)

// Peering limits.
const (
	MaxPendingRequests = 20
	RequestTTL         = 7 * 24 * time.Hour
	maxURL             = 512
	rollbackTimeout    = 5 * time.Second
)

var secretPattern = regexp.MustCompile(`^cpp_[A-Za-z0-9_-]{43}$`)

// NewSecret returns a random secret with prefix: 32 bytes, base64url.
func NewSecret(prefix string) string {
	var b [32]byte

	rand.Read(b[:]) // never fails; it panics if the OS entropy source is unusable

	return prefix + base64.RawURLEncoding.EncodeToString(b[:])
}

// Code is a secret's short code, for two owners to compare: the first 8 hex
// digits of its SHA-256.
func Code(secret string) string {
	sum := sha256.Sum256([]byte(secret))
	h := hex.EncodeToString(sum[:4])

	return h[:4] + "-" + h[4:]
}

// Equal compares secrets in constant time.
func Equal(a, b string) bool {
	return subtle.ConstantTimeCompare([]byte(a), []byte(b)) == 1
}

// PeerBySecret returns the peer whose secret this is. Every peer is compared,
// in constant time, so the answer's timing says nothing about the secret.
func (b *Inbox) PeerBySecret(ctx context.Context, secret string) (store.Peer, error) {
	peers, err := b.store.Peers(ctx)
	if err != nil {
		return store.Peer{}, err
	}

	var (
		found store.Peer
		ok    bool
	)

	for _, p := range peers {
		if Equal(p.Secret, secret) {
			found, ok = p, true
		}
	}

	if !ok {
		return store.Peer{}, errorf(KindUnauthorized, "unknown peer secret")
	}

	return found, nil
}

// AddPeer asks the daemon at url to become peers, under a new secret, and
// saves them as requested. Nothing is saved if they cannot be reached.
func (b *Inbox) AddPeer(ctx context.Context, name, peerURL, note string) (store.Peer, error) {
	b.peerMu.Lock()
	defer b.peerMu.Unlock()

	name, err := b.newPeerName(ctx, name)
	if err != nil {
		return store.Peer{}, err
	}

	if err = ValidURL(peerURL); err != nil {
		return store.Peer{}, errorf(KindInvalid, "%v", err)
	}

	peerURL = strings.TrimRight(peerURL, "/")

	if _, err = b.approvable(ctx, name, peerURL); err != nil {
		return store.Peer{}, err
	}

	if err = b.notAskedBy(ctx, peerURL); err != nil {
		return store.Peer{}, err
	}

	if b.url == "" {
		return store.Peer{}, errorf(KindUnprocessable,
			"this daemon has no public URL to give them; restart cpd with -url https://<where peers reach you>")
	}

	p := store.Peer{Name: name, URL: peerURL, Secret: NewSecret(PeerPrefix), Status: store.PeerRequested, AddedAt: b.now().UTC()}

	req := PeeringRequest{Name: b.self, URL: b.url, Secret: p.Secret, Note: note}
	if err = b.fed.RequestPeering(ctx, peerURL, req); err != nil {
		return store.Peer{}, errorf(KindUpstream, "ask %s: %v", peerURL, err)
	}

	if err = b.store.AddPeer(ctx, p); errors.Is(err, store.ErrExists) {
		return store.Peer{}, errorf(KindConflict, "%s is already a peer", name)
	} else if err != nil {
		return store.Peer{}, err
	}

	b.log.InfoContext(ctx, "peering requested", "peer", name, "url", peerURL, "code", Code(p.Secret))

	return p, nil
}

// notAskedBy refuses to ask the daemon at url when it has already asked
// this one: approving its request peers them, and asking back would only
// cross the two requests.
func (b *Inbox) notAskedBy(ctx context.Context, url string) error {
	reqs, err := b.Requests(ctx)
	if err != nil {
		return err
	}

	for _, r := range reqs {
		if r.URL == url {
			return errorf(KindConflict, "%s already asked to peer as %s (request %s, code %s); approve that request instead",
				url, r.Name, r.ID, Code(r.Secret))
		}
	}

	return nil
}

// validName normalizes a participant's name, or says why it is invalid.
func validName(name string) (string, error) {
	n, ok := thread.NormalizeName(name)
	if !ok {
		return "", errorf(KindInvalid, "%q: a name is lower-case letters, digits and single hyphens", name)
	}

	return n, nil
}

// peerName is a valid name for a peer: not the owner's.
func (b *Inbox) peerName(name string) (string, error) {
	n, err := validName(name)
	if err != nil {
		return "", err
	}

	if n == b.self {
		return "", errorf(KindUnprocessable, "%s is this daemon's owner; choose another name", n)
	}

	return n, nil
}

func (b *Inbox) newPeerName(ctx context.Context, name string) (string, error) {
	n, err := b.peerName(name)
	if err != nil {
		return "", err
	}

	if _, err = b.store.Peer(ctx, n); err == nil {
		return "", errorf(KindConflict, "%s is already a peer", n)
	} else if !errors.Is(err, store.ErrNotFound) {
		return "", err
	}

	return n, nil
}

// RequestReceived stores a peering request from another daemon for the
// owner to approve, and returns its code.
func (b *Inbox) RequestReceived(ctx context.Context, req PeeringRequest) (string, error) {
	name, err := validName(req.Name)
	if err != nil {
		return "", err
	}

	if err = ValidURL(req.URL); err != nil {
		return "", errorf(KindInvalid, "%v", err)
	}

	req.URL = strings.TrimRight(req.URL, "/")

	if !secretPattern.MatchString(req.Secret) {
		return "", errorf(KindInvalid, "secret: want cpp_ and 43 base64url characters")
	}

	now := b.now().UTC()
	r := store.Request{ID: uuid.NewString()[:8], Name: name, URL: req.URL, Secret: req.Secret, Note: req.Note, At: now}

	dropped, err := b.store.AddRequest(ctx, r, MaxPendingRequests, now.Add(-RequestTTL))
	if err != nil {
		return "", err
	}

	b.log.InfoContext(ctx, "peering request received", "name", name, "url", req.URL, "code", Code(req.Secret))

	if dropped > 0 {
		b.log.WarnContext(ctx, "peering requests dropped to make room", "dropped", dropped, "max", MaxPendingRequests)
	}

	return Code(req.Secret), nil
}

// Requests returns pending peering requests.
func (b *Inbox) Requests(ctx context.Context) ([]store.Request, error) {
	return b.store.Requests(ctx, b.now().UTC().Add(-RequestTTL))
}

// Approve accepts a peering request, as name or the name they asked for.
// The peer is saved, then the requester's daemon is told at the URL it gave,
// with the secret it offered; if it does not confirm, the approval is undone.
// That proves the URL is the requester's, and nothing they send on hearing
// back can arrive before they are a peer here.
func (b *Inbox) Approve(ctx context.Context, id, name string) (store.Peer, error) {
	b.peerMu.Lock()
	defer b.peerMu.Unlock()

	now := b.now().UTC()

	r, err := b.store.Request(ctx, id, now.Add(-RequestTTL))
	if errors.Is(err, store.ErrNotFound) {
		return store.Peer{}, errorf(KindNotFound, "no pending request %q", id)
	} else if err != nil {
		return store.Peer{}, err
	}

	if name == "" {
		name = r.Name
	}

	n, err := b.peerName(name)
	if err != nil {
		return store.Peer{}, err
	}

	prev, err := b.approvable(ctx, n, r.URL)
	if err != nil {
		return store.Peer{}, err
	}

	p, err := b.store.Approve(ctx, id, n, now, now.Add(-RequestTTL))
	if errors.Is(err, store.ErrNotFound) {
		return p, errorf(KindNotFound, "no pending request %q", id)
	} else if err != nil {
		return p, err
	}

	if err = b.fed.Accepted(ctx, r.URL, r.Secret); err != nil {
		// A disconnected owner still needs the unconfirmed approval undone.
		undoCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), rollbackTimeout)
		defer cancel()

		if undoErr := b.store.Unapprove(undoCtx, p, prev, r); undoErr != nil {
			return store.Peer{}, fmt.Errorf("undo approval of %s: %w", n, undoErr)
		}

		return store.Peer{}, errorf(KindUpstream, "the daemon at %s did not confirm this request, so it is not approved: %v", r.URL, err)
	}

	if err = b.store.DeleteRequestsFrom(ctx, p.URL); err != nil {
		return p, err
	}

	b.log.InfoContext(ctx, "peering approved", "peer", p.Name, "url", p.URL)
	b.poke()

	return p, nil
}

// boundElsewhere refuses name for the daemon at url when a removed peer of
// that name, at another URL, left threads here: threads name their
// participants, so the new daemon would take them over.
func (b *Inbox) boundElsewhere(ctx context.Context, name, url string) error {
	was, err := b.store.FormerURL(ctx, name)
	if err != nil {
		return err
	}

	if was != "" && was != url {
		return errorf(KindConflict, "%s was a peer at %s and threads with them are kept; give this one another name", name, was)
	}

	return nil
}

// approvable checks that a request from url can become the peer called name,
// and returns the peer it replaces: one of that name at the same URL, or
// nil.
func (b *Inbox) approvable(ctx context.Context, name, url string) (*store.Peer, error) {
	if err := b.boundElsewhere(ctx, name, url); err != nil {
		return nil, err
	}

	peers, err := b.store.Peers(ctx)
	if err != nil {
		return nil, err
	}

	var prev *store.Peer

	for _, p := range peers {
		switch {
		case p.Name == name && p.URL != url:
			return nil, errorf(KindConflict, "another peer is called %s; approve with another name", name)
		case p.Name != name && p.URL == url:
			return nil, errorf(KindConflict, "%s is already a peer at %s; use that name or remove them first", p.Name, url)
		case p.Name == name:
			prev = &p
		}
	}

	return prev, nil
}

// Deny drops a peering request.
func (b *Inbox) Deny(ctx context.Context, id string) error {
	if err := b.store.DeleteRequest(ctx, id); errors.Is(err, store.ErrNotFound) {
		return errorf(KindNotFound, "no pending request %q", id)
	} else if err != nil {
		return err
	}

	return nil
}

// Accepted marks peer active, because their owner approved: events queued
// for them are due now, and their own requests to this daemon are moot.
func (b *Inbox) Accepted(ctx context.Context, peer string) error {
	changed, err := b.store.Activate(ctx, peer, b.now().UTC())
	if err != nil || !changed {
		return err
	}

	p, err := b.store.Peer(ctx, peer)
	if err != nil {
		return err
	}

	if err = b.store.DeleteRequestsFrom(ctx, p.URL); err != nil {
		return err
	}

	b.log.InfoContext(ctx, "peering accepted", "peer", peer)
	b.poke()

	return nil
}

// Peer returns one peer.
func (b *Inbox) Peer(ctx context.Context, name string) (store.Peer, error) {
	n, _ := thread.NormalizeName(name)

	p, err := b.store.Peer(ctx, n)
	if errors.Is(err, store.ErrNotFound) {
		return p, errorf(KindNotFound, "%s is not a peer", name)
	}

	return p, err
}

// Peers returns every peer.
func (b *Inbox) Peers(ctx context.Context) ([]store.Peer, error) {
	return b.store.Peers(ctx)
}

// RemovePeer forgets a peer. Threads with them stay, and their name stays
// with their URL; see boundElsewhere.
func (b *Inbox) RemovePeer(ctx context.Context, name string) error {
	n, _ := thread.NormalizeName(name)

	b.peerMu.Lock()
	defer b.peerMu.Unlock()

	// Under mu, so no event of theirs is stored after they are gone.
	b.mu.Lock()
	defer b.mu.Unlock()

	err := b.store.RemovePeer(ctx, n)
	if errors.Is(err, store.ErrNotFound) {
		return errorf(KindNotFound, "%s is not a peer", name)
	}

	if err == nil {
		b.log.InfoContext(ctx, "peer removed", "peer", n)
	}

	return err
}

// ValidURL checks a daemon's base URL.
func ValidURL(raw string) error {
	if len(raw) > maxURL {
		return fmt.Errorf("url is %d characters, over %d", len(raw), maxURL)
	}

	u, err := url.Parse(raw)
	if err != nil {
		return fmt.Errorf("url %q: %w", raw, err)
	}

	if (u.Scheme != "https" && u.Scheme != "http") || u.Host == "" || u.User != nil || u.RawQuery != "" || u.Fragment != "" {
		return fmt.Errorf("url %q: want http(s)://host[:port][/path]", raw)
	}

	return nil
}
