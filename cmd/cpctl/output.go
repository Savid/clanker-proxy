package main

import (
	"fmt"
	"io"
	"strings"
	"text/tabwriter"
	"time"

	"github.com/savid/clanker-proxy/api/rest"
)

// shortID is how threads are shown: enough of the ID to refer to it.
const shortID = 8

// marshaler is a generated API type.
type marshaler interface {
	MarshalJSON() ([]byte, error)
}

// print writes v as one line of JSON with -json, else as text.
func (a *app) print(v marshaler, text func(io.Writer)) error {
	if !a.json {
		text(a.stdout)

		return nil
	}

	b, err := v.MarshalJSON()
	if err != nil {
		return fmt.Errorf("encode: %w", err)
	}

	_, err = fmt.Fprintf(a.stdout, "%s\n", b)

	return err
}

func printMe(w io.Writer, me *rest.Me) {
	fmt.Fprintln(w, me.Name)

	if me.URL.Set {
		fmt.Fprintf(w, "peers reach you at %s\n", urlString(me.URL.Value))
	} else {
		fmt.Fprintln(w, "no public URL: restart cpd with -url to ask others to peer")
	}
}

func printPeers(w io.Writer, peers []rest.Peer) {
	if len(peers) == 0 {
		fmt.Fprintln(w, "no peers; ask one with: cpctl peer add <name> <url>")

		return
	}

	tw := tabwriter.NewWriter(w, 0, 4, 2, ' ', 0)
	fmt.Fprintln(tw, "NAME\tSTATUS\tCODE\tURL")

	for _, p := range peers {
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\n", p.Name, p.Status, p.Code, urlString(p.URL))
	}

	_ = tw.Flush()
}

func printRequests(w io.Writer, reqs []rest.Request, now time.Time) {
	if len(reqs) == 0 {
		fmt.Fprintln(w, "no peering requests")

		return
	}

	tw := tabwriter.NewWriter(w, 0, 4, 2, ' ', 0)
	fmt.Fprintln(tw, "ID\tNAME\tCODE\tASKED\tURL\tNOTE")

	for _, r := range reqs {
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\t%s\t%s\n", r.ID, r.Name, r.Code, ago(now, r.At), urlString(r.URL), r.Note.Or(""))
	}

	_ = tw.Flush()
}

func printThreads(w io.Writer, threads []rest.ThreadSummary, now time.Time) {
	if len(threads) == 0 {
		fmt.Fprintln(w, "no threads")

		return
	}

	tw := tabwriter.NewWriter(w, 0, 4, 2, ' ', 0)
	fmt.Fprintln(tw, "ID\tPEER\tSTATE\tTURN\tUPDATED\tTITLE")

	for _, t := range threads {
		fmt.Fprintf(tw, "%s\t%s %s\t%s\t%s\t%s\t%s\n",
			t.ID[:shortID], direction(t), t.Peer, t.State, turn(t.MyTurn, t.Turn), ago(now, t.UpdatedAt), title(t))
	}

	_ = tw.Flush()
}

// summaryOf is a thread without its log.
func summaryOf(t *rest.Thread) rest.ThreadSummary {
	return rest.ThreadSummary{
		ID: t.ID, Title: t.Title, Kind: t.Kind, Labels: t.Labels, Sender: t.Sender, Recipient: t.Recipient,
		Peer: t.Peer, State: t.State, Turn: t.Turn, MyTurn: t.MyTurn, OpenedAt: t.OpenedAt, UpdatedAt: t.UpdatedAt,
		Events: t.Events, Undelivered: t.Undelivered, Failed: t.Failed,
	}
}

func printThread(w io.Writer, t *rest.Thread) {
	fmt.Fprintf(w, "%s  %s\n", t.ID, t.Title)
	fmt.Fprintf(w, "%s -> %s · %s · %s · turn: %s", t.Sender, t.Recipient, t.Kind, t.State, turn(t.MyTurn, t.Turn))

	if len(t.Labels) > 0 {
		labels := make([]string, 0, len(t.Labels))
		for _, l := range t.Labels {
			labels = append(labels, string(l))
		}

		fmt.Fprintf(w, " · labels: %s", strings.Join(labels, ", "))
	}

	fmt.Fprintln(w)

	for _, e := range t.Log {
		fmt.Fprintf(w, "\n-- %s · %s · %s%s\n", e.Action, e.From, e.At.Local().Format(time.DateTime), deliveryNote(e))

		if e.Ignored.Set {
			fmt.Fprintf(w, "   (no effect: %s)\n", e.Ignored.Value)
		}

		if e.Body.Set {
			fmt.Fprintln(w, e.Body.Value)
		}
	}
}

// direction is -> for threads the owner opened, <- for ones sent to them.
func direction(t rest.ThreadSummary) string {
	if t.Sender == t.Peer {
		return "<-"
	}

	return "->"
}

func turn(mine bool, who rest.OptName) string {
	switch {
	case mine:
		return "you"
	case who.Set:
		return string(who.Value)
	default:
		return "-"
	}
}

func title(t rest.ThreadSummary) string {
	s := string(t.Title)
	if t.Kind == rest.ThreadKindFyi {
		s = "[fyi] " + s
	}

	if t.Failed > 0 {
		s += fmt.Sprintf("  (%d undeliverable)", t.Failed)
	} else if t.Undelivered > 0 {
		s += fmt.Sprintf("  (%d sending)", t.Undelivered)
	}

	return s
}

func deliveryNote(e rest.ThreadEvent) string {
	switch e.Delivery.Status {
	case rest.DeliveryStatusPending:
		s := fmt.Sprintf(" · sending (attempt %d)", e.Delivery.Attempts+1)
		if e.Delivery.LastError.Set {
			s += ": " + e.Delivery.LastError.Value
		}

		return s
	case rest.DeliveryStatusFailed:
		return " · NOT DELIVERED: " + e.Delivery.LastError.Or("refused")
	case rest.DeliveryStatusReceived, rest.DeliveryStatusDelivered:
		return ""
	default:
		return ""
	}
}

func ago(now, t time.Time) string {
	d := now.Sub(t)

	switch {
	case d < time.Minute:
		return "just now"
	case d < time.Hour:
		return fmt.Sprintf("%dm ago", int(d.Minutes()))
	case d < 48*time.Hour:
		return fmt.Sprintf("%dh ago", int(d.Hours()))
	default:
		return t.Local().Format(time.DateOnly)
	}
}
