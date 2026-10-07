package server

import (
	"context"
	"errors"
	"log/slog"
	"net/http"
	"time"

	"github.com/ogen-go/ogen/ogenerrors"

	"github.com/savid/clanker-proxy/api/rest"
	"github.com/savid/clanker-proxy/internal/inbox"
	"github.com/savid/clanker-proxy/internal/store"
	"github.com/savid/clanker-proxy/pkg/thread"
)

// operations implements the generated handler. Adding an operation to the
// spec and running `make generate` fails the build until it is added here.
type operations struct {
	log     *slog.Logger
	inbox   Inbox
	version string
	now     func() time.Time
}

var _ rest.Handler = (*operations)(nil)

// Public.

func (o *operations) GetHealth(context.Context) (*rest.Health, error) {
	return &rest.Health{Status: rest.HealthStatusOk, At: o.now().UTC()}, nil
}

func (o *operations) RequestPeering(ctx context.Context, req *rest.PeeringRequest) (*rest.RequestReceipt, error) {
	code, err := o.inbox.RequestReceived(ctx, inbox.PeeringRequest{
		Name: string(req.Name), URL: urlString(req.URL), Secret: req.Secret, Note: string(req.Note.Or("")),
	})
	if err != nil {
		return nil, err
	}

	return &rest.RequestReceipt{Status: rest.RequestReceiptStatusPending, Code: rest.Code(code)}, nil
}

// Peer.

func (o *operations) DeliverEvent(ctx context.Context, req *rest.Event) (*rest.Receipt, error) {
	peer := peerFrom(ctx)
	e := thread.Event{
		ID: string(req.ID), Thread: string(req.Thread), At: req.At.UTC(), Clock: req.Clock, Action: thread.Action(req.Action),
		Title: string(req.Title.Or("")), Kind: thread.Kind(req.Kind.Or("")), Labels: labels(req.Labels), Body: string(req.Body.Or("")),
	}

	r, err := o.inbox.Receive(ctx, peer, e)
	if err != nil {
		return nil, err
	}

	if err = o.inbox.Accepted(ctx, peer); err != nil {
		o.log.WarnContext(ctx, "activate peer", "peer", peer, "error", err)
	}

	return &rest.Receipt{ID: rest.ID(r.ID), Duplicate: r.Duplicate}, nil
}

func (o *operations) NotifyPeeringAccepted(ctx context.Context) error {
	return o.inbox.Accepted(ctx, peerFrom(ctx))
}

// Owner.

func (o *operations) GetMe(context.Context) (*rest.Me, error) {
	me := &rest.Me{Name: rest.Name(o.inbox.Self()), Version: o.version}
	if u, ok := parseURL(o.inbox.URL()); ok {
		me.URL = rest.NewOptDaemonURL(u)
	}

	return me, nil
}

func (o *operations) ListRequests(ctx context.Context) (*rest.RequestList, error) {
	reqs, err := o.inbox.Requests(ctx)
	if err != nil {
		return nil, err
	}

	out := make([]rest.Request, 0, len(reqs))
	for _, r := range reqs {
		out = append(out, request(r))
	}

	return &rest.RequestList{Requests: out}, nil
}

func (o *operations) ApproveRequest(ctx context.Context, req *rest.Approval, params rest.ApproveRequestParams) (*rest.Peer, error) {
	p, err := o.inbox.Approve(ctx, string(params.ID), string(req.Name.Or("")))
	if err != nil {
		return nil, err
	}

	return peer(p), nil
}

func (o *operations) DenyRequest(ctx context.Context, params rest.DenyRequestParams) error {
	return o.inbox.Deny(ctx, string(params.ID))
}

func (o *operations) ListPeers(ctx context.Context) (*rest.PeerList, error) {
	peers, err := o.inbox.Peers(ctx)
	if err != nil {
		return nil, err
	}

	out := make([]rest.Peer, 0, len(peers))
	for _, p := range peers {
		out = append(out, *peer(p))
	}

	return &rest.PeerList{Peers: out}, nil
}

func (o *operations) AddPeer(ctx context.Context, req *rest.PeerInput) (*rest.Peer, error) {
	p, err := o.inbox.AddPeer(ctx, string(req.Name), urlString(req.URL), string(req.Note.Or("")))
	if err != nil {
		return nil, err
	}

	return peer(p), nil
}

func (o *operations) GetPeer(ctx context.Context, params rest.GetPeerParams) (*rest.Peer, error) {
	p, err := o.inbox.Peer(ctx, string(params.Name))
	if err != nil {
		return nil, err
	}

	return peer(p), nil
}

func (o *operations) RemovePeer(ctx context.Context, params rest.RemovePeerParams) error {
	return o.inbox.RemovePeer(ctx, string(params.Name))
}

func (o *operations) ListThreads(ctx context.Context, params rest.ListThreadsParams) (*rest.ThreadList, error) {
	sums, err := o.inbox.Threads(ctx, store.Filter{
		Turn:  string(params.Turn.Or("")),
		State: thread.State(params.State.Or("")),
		Peer:  string(params.Peer.Or("")),
		Label: string(params.Label.Or("")),
		Limit: int(params.Limit.Or(100)),
	})
	if err != nil {
		return nil, err
	}

	out := make([]rest.ThreadSummary, 0, len(sums))
	for _, s := range sums {
		out = append(out, summary(s, o.inbox.Self()))
	}

	return &rest.ThreadList{Threads: out}, nil
}

func (o *operations) GetThread(ctx context.Context, params rest.GetThreadParams) (*rest.Thread, error) {
	v, err := o.inbox.Thread(ctx, params.Ref)
	if err != nil {
		return nil, err
	}

	return threadView(v, o.inbox.Self()), nil
}

func (o *operations) OpenThread(ctx context.Context, req *rest.NewThread) (*rest.ThreadSummary, error) {
	v, err := o.inbox.Open(ctx, inbox.NewThread{
		To: string(req.To), Title: string(req.Title), Kind: thread.Kind(req.Kind.Or("")), Labels: labels(req.Labels),
		Body: string(req.Body.Or("")),
	})
	if err != nil {
		return nil, err
	}

	sum := summary(v.Summary, o.inbox.Self())

	return &sum, nil
}

func (o *operations) ActOnThread(ctx context.Context, req *rest.ThreadAction, params rest.ActOnThreadParams) (*rest.ThreadSummary, error) {
	v, err := o.inbox.Act(ctx, params.Ref, thread.Action(req.Action), string(req.Body.Or("")))
	if err != nil {
		return nil, err
	}

	sum := summary(v.Summary, o.inbox.Self())

	return &sum, nil
}

// NewError answers every handler and security error: a missing or wrong
// credential as 401, the caller's fault as its status with the reason,
// anything else (a security check that failed internally included) as a
// bare 500.
func (o *operations) NewError(ctx context.Context, err error) *rest.ProblemStatusCode {
	if errors.Is(err, errUnauthorized) || errors.Is(err, ogenerrors.ErrSecurityRequirementIsNotSatisfied) {
		return problem(http.StatusUnauthorized, errUnauthorized.Error())
	}

	status, ok := inbox.HTTPStatus(err)
	if !ok {
		o.log.ErrorContext(ctx, "handler failed", "error", err)

		return problem(status, "")
	}

	return problem(status, err.Error())
}

func problem(status int, detail string) *rest.ProblemStatusCode {
	p := rest.Problem{Title: http.StatusText(status), Status: int32(status)} //nolint:gosec // HTTP statuses fit
	if detail != "" {
		p.Detail = rest.NewOptString(detail)
	}

	return &rest.ProblemStatusCode{StatusCode: status, Response: p}
}
