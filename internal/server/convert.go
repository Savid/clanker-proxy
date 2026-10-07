package server

import (
	"net/url"

	"github.com/savid/clanker-proxy/api/rest"
	"github.com/savid/clanker-proxy/internal/inbox"
	"github.com/savid/clanker-proxy/internal/store"
	"github.com/savid/clanker-proxy/pkg/thread"
)

func parseURL(raw string) (rest.DaemonURL, bool) {
	u, err := url.Parse(raw)
	if err != nil || raw == "" {
		return rest.DaemonURL{}, false
	}

	return rest.DaemonURL(*u), true
}

func urlString(u rest.DaemonURL) string {
	plain := url.URL(u)

	return plain.String()
}

func labels(in rest.Labels) []string {
	out := make([]string, 0, len(in))
	for _, l := range in {
		out = append(out, string(l))
	}

	return out
}

func toLabels(in []string) rest.Labels {
	out := make(rest.Labels, 0, len(in))
	for _, l := range in {
		out = append(out, rest.Label(l))
	}

	return out
}

func count(n int) rest.Count {
	return rest.Count(min(n, 1_000_000)) //nolint:gosec // bounded above; counts are never negative
}

func peer(p store.Peer) *rest.Peer {
	u, _ := parseURL(p.URL) // validated when the peer was saved

	return &rest.Peer{
		Name: rest.Name(p.Name), URL: u, Status: rest.PeerStatus(p.Status), Code: rest.Code(inbox.Code(p.Secret)),
		AddedAt: p.AddedAt,
	}
}

func request(r store.Request) rest.Request {
	u, _ := parseURL(r.URL)

	out := rest.Request{ID: rest.RequestRef(r.ID), Name: rest.Name(r.Name), URL: u, Code: rest.Code(inbox.Code(r.Secret)), At: r.At}
	if r.Note != "" {
		out.Note = rest.NewOptNote(rest.Note(r.Note))
	}

	return out
}

func summary(s store.Summary, self string) rest.ThreadSummary {
	th := thread.Thread{Sender: s.Sender, Recipient: s.Recipient, State: s.State}
	role, _ := th.RoleOf(self)

	actions := make([]rest.Action, 0, len(thread.Actions()))
	for _, a := range th.Allowed(self) {
		actions = append(actions, rest.Action(a))
	}

	out := rest.ThreadSummary{
		ID: rest.ID(s.ID), Title: rest.Title(s.Title), Kind: rest.ThreadKind(s.Kind), Labels: toLabels(s.Labels),
		Sender: rest.Name(s.Sender), Recipient: rest.Name(s.Recipient), Peer: rest.Name(s.Peer),
		Role: rest.ThreadSummaryRole(role), State: rest.ThreadState(s.State), MyTurn: s.Turn != "" && s.Turn == self,
		Actions: actions, OpenedAt: s.OpenedAt, UpdatedAt: s.UpdatedAt,
		Events: count(s.Events), Undelivered: count(s.Undelivered), Failed: count(s.Failed),
	}
	if s.Turn != "" {
		out.Turn = rest.NewOptName(rest.Name(s.Turn))
	}

	return out
}

func threadView(v inbox.View, self string) *rest.Thread {
	s := summary(v.Summary, self)
	t := &rest.Thread{
		ID: s.ID, Title: s.Title, Kind: s.Kind, Labels: s.Labels,
		Sender: s.Sender, Recipient: s.Recipient, Peer: s.Peer, Role: rest.ThreadRole(s.Role),
		State: s.State, Turn: s.Turn, MyTurn: s.MyTurn, Actions: s.Actions,
		OpenedAt: s.OpenedAt, UpdatedAt: s.UpdatedAt,
		Events: s.Events, Undelivered: s.Undelivered, Failed: s.Failed,
		Log: make([]rest.ThreadEvent, 0, len(v.Events)),
	}

	for _, e := range v.Events {
		t.Log = append(t.Log, threadEvent(e))
	}

	return t
}

func threadEvent(e inbox.ViewEvent) rest.ThreadEvent {
	te := rest.ThreadEvent{ID: rest.ID(e.ID), From: rest.Name(e.From), At: e.At, Action: rest.Action(e.Action)}

	if e.Body != "" {
		te.Body = rest.NewOptBody(rest.Body(e.Body))
	}

	if e.Ignored != "" {
		te.Ignored = rest.NewOptString(e.Ignored)
	}

	if e.Delivery != nil {
		te.Delivery = rest.NewOptDelivery(delivery(*e.Delivery))
	}

	return te
}

func delivery(d store.Delivery) rest.Delivery {
	out := rest.Delivery{Status: rest.DeliveryStatus(d.Status), Attempts: count(d.Attempts)}

	if d.LastError != "" {
		out.LastError = rest.NewOptString(d.LastError)
	}

	if !d.NextAttemptAt.IsZero() {
		out.NextAttemptAt = rest.NewOptDateTime(d.NextAttemptAt)
	}

	if !d.DeliveredAt.IsZero() {
		out.DeliveredAt = rest.NewOptDateTime(d.DeliveredAt)
	}

	return out
}
