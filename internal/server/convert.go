package server

import (
	"net/url"

	"github.com/savid/clanker-proxy/api/rest"
	"github.com/savid/clanker-proxy/internal/inbox"
	"github.com/savid/clanker-proxy/internal/store"
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
	out := rest.ThreadSummary{
		ID: rest.ID(s.ID), Title: rest.Title(s.Title), Kind: rest.ThreadKind(s.Kind), Labels: toLabels(s.Labels),
		Sender: rest.Name(s.Sender), Recipient: rest.Name(s.Recipient), Peer: rest.Name(s.Peer),
		State: rest.ThreadState(s.State), MyTurn: s.Turn != "" && s.Turn == self,
		OpenedAt: s.OpenedAt, UpdatedAt: s.UpdatedAt,
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
		Sender: s.Sender, Recipient: s.Recipient, Peer: s.Peer,
		State: s.State, Turn: s.Turn, MyTurn: s.MyTurn,
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
	te := rest.ThreadEvent{
		ID: rest.ID(e.ID), From: rest.Name(e.From), To: rest.Name(e.To), At: e.At, Action: rest.Action(e.Action),
		Delivery: rest.Delivery{Status: rest.DeliveryStatus(e.Delivery.Status), Attempts: count(e.Delivery.Attempts)},
	}

	if e.Body != "" {
		te.Body = rest.NewOptBody(rest.Body(e.Body))
	}

	if e.Ignored != "" {
		te.Ignored = rest.NewOptString(e.Ignored)
	}

	if e.Delivery.LastError != "" {
		te.Delivery.LastError = rest.NewOptString(e.Delivery.LastError)
	}

	if !e.Delivery.NextAttemptAt.IsZero() {
		te.Delivery.NextAttemptAt = rest.NewOptDateTime(e.Delivery.NextAttemptAt)
	}

	if !e.Delivery.DeliveredAt.IsZero() {
		te.Delivery.DeliveredAt = rest.NewOptDateTime(e.Delivery.DeliveredAt)
	}

	return te
}
