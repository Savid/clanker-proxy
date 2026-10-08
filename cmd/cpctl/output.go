package main

import (
	"encoding/json"
	"fmt"
	"io"
	"slices"
	"strconv"
	"strings"
	"text/tabwriter"
	"time"
	"unicode"

	"github.com/savid/clanker-proxy/api/rest"
	"github.com/savid/clanker-proxy/pkg/thread"
)

// shortID is how much of a thread ID lists show: enough to refer to it.
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

func jsonLine(w io.Writer, v any) error {
	enc := json.NewEncoder(w)
	enc.SetEscapeHTML(false)

	if err := enc.Encode(v); err != nil {
		return fmt.Errorf("encode: %w", err)
	}

	return nil
}

// step is a command that usually comes next, and what it is for.
type step struct {
	cmd, why string
}

// next ends text output with the commands that usually follow.
func next(w io.Writer, steps ...step) {
	if len(steps) == 0 {
		return
	}

	fmt.Fprintln(w, "next:")

	tw := tabwriter.NewWriter(w, 0, 4, 3, ' ', 0)
	for _, s := range steps {
		fmt.Fprintf(tw, "  %s\t%s\n", s.cmd, s.why)
	}

	_ = tw.Flush()
}

// actionSteps is how to take each action on a thread.
var actionSteps = map[thread.Action]func(ref string) step{
	thread.ActionComment: func(r string) step { return step{`cpctl reply ` + r + ` -m "<text>"`, "add a message"} },
	thread.ActionAck:     func(r string) step { return step{"cpctl ack " + r, "optional: say you are on it"} },
	thread.ActionNeedsInput: func(r string) step {
		return step{`cpctl needs-input ` + r + ` -m "<question>"`, "ask the sender something"}
	},
	thread.ActionResolve: func(r string) step { return step{`cpctl resolve ` + r + ` -m "<result>"`, "done; report the result"} },
	thread.ActionDecline: func(r string) step { return step{`cpctl decline ` + r + ` -m "<why>"`, "refuse it; ends the thread"} },
	thread.ActionClose:   func(r string) step { return step{"cpctl close " + r, "done here; ends the thread for both sides"} },
	thread.ActionReopen: func(r string) step {
		return step{`cpctl reopen ` + r + ` -m "<why>"`, "not done; back to the recipient"}
	},
	thread.ActionWithdraw: func(r string) step {
		return step{`cpctl withdraw ` + r + ` -m "<why>"`, "no longer needed; ends the thread"}
	},
}

// stepOrder puts each side's main moves first.
var stepOrder = []thread.Action{
	thread.ActionResolve, thread.ActionNeedsInput, thread.ActionClose, thread.ActionReopen,
	thread.ActionAck, thread.ActionDecline, thread.ActionWithdraw, thread.ActionComment,
}

// threadSteps is what the owner can do with the thread: wait while it is
// the peer's turn, then the actions they may take.
func threadSteps(t rest.ThreadSummary) []step {
	ref := string(t.ID)[:shortID]

	var steps []step
	if !t.MyTurn && t.Turn.Set && t.Kind != rest.ThreadKindFyi {
		steps = append(steps, step{"cpctl wait " + ref + " -timeout 30m", "block until " + string(t.Peer) + " acts"})
	}

	fyiAck := t.Kind == rest.ThreadKindFyi && slices.Contains(t.Actions, rest.ActionAck)
	if fyiAck {
		steps = append(steps, step{"cpctl ack " + ref, "say you have seen it; closes it"})
	}

	// The sender's answer to a question is a reply, and the move that hands
	// the thread back, so it comes before ending the thread.
	answering := t.State == rest.ThreadStateNeedsInput && t.Role == rest.ThreadSummaryRoleSender &&
		slices.Contains(t.Actions, rest.ActionComment)
	if answering {
		steps = append(steps, step{`cpctl reply ` + ref + ` -m "<answer>"`, "answer their question; hands it back"})
	}

	for _, act := range stepOrder {
		if slices.Contains(t.Actions, rest.Action(act)) && (act != thread.ActionAck || !fyiAck) && (act != thread.ActionComment || !answering) {
			s := actionSteps[act](ref)
			if act == thread.ActionClose && t.Role == rest.ThreadSummaryRoleRecipient {
				s.why = "end it without a result, e.g. no longer needed; to report work, resolve"
				if t.State == rest.ThreadStateResolved {
					s.why = "only if the sender has agreed; ends the thread for both sides"
				}
			}
			steps = append(steps, s)
		}
	}

	return steps
}

func printPeers(w io.Writer, peers []rest.Peer) {
	tw := tabwriter.NewWriter(w, 0, 4, 2, ' ', 0)
	fmt.Fprintln(tw, "NAME\tSTATUS\tCODE\tURL")

	requested := false

	for _, p := range peers {
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\n", p.Name, p.Status, p.Code, urlString(p.URL))
		requested = requested || p.Status == rest.PeerStatusRequested
	}

	_ = tw.Flush()

	if requested {
		fmt.Fprintln(w, "requested: waiting for their owner to approve; threads to them are delivered after that")
	}
}

func printRequests(w io.Writer, reqs []rest.Request, now time.Time) {
	tw := tabwriter.NewWriter(w, 0, 4, 2, ' ', 0)
	fmt.Fprintln(tw, "ID\tNAME\tCODE\tASKED\tURL\tNOTE")

	for _, r := range reqs {
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\t%s\t%q\n", r.ID, r.Name, r.Code, ago(now, r.At), urlString(r.URL), r.Note.Or(""))
	}

	_ = tw.Flush()
}

func printThreads(w io.Writer, threads []rest.ThreadSummary, now time.Time) {
	tw := tabwriter.NewWriter(w, 0, 4, 2, ' ', 0)
	fmt.Fprintln(tw, "ID\tPEER\tSTATE\tTURN\tUPDATED\tTITLE")

	for _, t := range threads {
		fmt.Fprintf(tw, "%s\t%s %s\t%s\t%s\t%s\t%s\n",
			t.ID[:shortID], direction(t), t.Peer, t.State, turn(t), ago(now, t.UpdatedAt), title(t))
	}

	_ = tw.Flush()
}

// summaryOf is a thread without its log.
func summaryOf(t *rest.Thread) rest.ThreadSummary {
	return rest.ThreadSummary{
		ID: t.ID, Title: t.Title, Kind: t.Kind, Labels: t.Labels, Sender: t.Sender, Recipient: t.Recipient,
		Peer: t.Peer, Role: rest.ThreadSummaryRole(t.Role), State: t.State, Turn: t.Turn, MyTurn: t.MyTurn,
		Actions: t.Actions, OpenedAt: t.OpenedAt, UpdatedAt: t.UpdatedAt,
		Events: t.Events, LastFrom: t.LastFrom, Undelivered: t.Undelivered, Failed: t.Failed,
	}
}

// unanswered is a thread waiting on the owner whose latest move is the
// peer's: something they have not yet answered, acked or acted on.
func unanswered(t rest.ThreadSummary) bool {
	return t.MyTurn && t.LastFrom == t.Peer
}

// printSummary is a thread's header: what it is, where it stands, and whose
// turn it is.
func printSummary(w io.Writer, t rest.ThreadSummary) {
	fmt.Fprintf(w, "%s  %s\n", t.ID, title(t))
	fmt.Fprintf(w, "%s -> %s · %s · %s · you are the %s · turn: %s", t.Sender, t.Recipient, t.Kind, t.State, t.Role, turn(t))

	if len(t.Labels) > 0 {
		labels := make([]string, 0, len(t.Labels))
		for _, l := range t.Labels {
			labels = append(labels, string(l))
		}

		fmt.Fprintf(w, " · labels: %s", strings.Join(labels, ", "))
	}

	fmt.Fprintln(w)
	fmt.Fprintln(w, standing(t))
}

// standing says what the thread's state asks of the owner.
func standing(t rest.ThreadSummary) string {
	peer := string(t.Peer)
	sender := t.Role == rest.ThreadSummaryRoleSender

	switch {
	case t.Kind == rest.ThreadKindFyi && t.State == rest.ThreadStateOpen && sender:
		return "fyi: nothing is needed from you; it closes when " + peer + " acks it."
	case t.Kind == rest.ThreadKindFyi && t.State == rest.ThreadStateOpen:
		return "fyi: no answer expected; ack it to say you have seen it, which closes it."
	}

	switch t.State {
	case rest.ThreadStateOpen, rest.ThreadStateAcked:
		if sender {
			return "Waiting on " + peer + " to work on it."
		}

		return "Waiting on you: resolve it with the result when done, or ask with needs-input."
	case rest.ThreadStateNeedsInput:
		if sender {
			return peer + " asked you something: reply to answer, which hands it back."
		}

		return "Waiting on " + peer + " to answer your question."
	case rest.ThreadStateResolved:
		if sender {
			return peer + " says it is done: close it if you agree, or reopen it saying what is missing."
		}

		return "Awaiting " + peer + "'s review. Close only if they have agreed; you can also reopen it with a reason."
	case rest.ThreadStateClosed:
		return "Ended: closed. Either side can reopen it with a reason, or still reply."
	case rest.ThreadStateDeclined:
		return "Ended: the recipient declined it. Either side can still reply."
	case rest.ThreadStateWithdrawn:
		return "Ended: the sender withdrew it. Either side can still reply."
	default:
		return ""
	}
}

func plural(n int, word string) string {
	if n == 1 {
		return word
	}

	return word + "s"
}

func printLog(w io.Writer, log []rest.ThreadEvent) {
	for _, e := range log {
		fmt.Fprintf(w, "\n-- %s · %s · %s%s\n", e.Action, e.From, e.At.UTC().Format(time.RFC3339), deliveryNote(e))

		if e.Ignored.Set {
			fmt.Fprintf(w, "   (no effect: %s)\n", e.Ignored.Value)
		}

		// Every body line is marked, so a peer's text cannot pass for an
		// event header or a next: step.
		if e.Body.Set {
			for line := range strings.SplitSeq(printable(string(e.Body.Value), true), "\n") {
				fmt.Fprintln(w, "  │ "+line)
			}
		}
	}
}

// printable drops control characters, which could move the cursor, rewrite
// the terminal or set its clipboard; multiline keeps line breaks and tabs.
func printable(s string, multiline bool) string {
	return strings.Map(func(r rune) rune {
		switch {
		case multiline && (r == '\n' || r == '\t'):
			return r
		case unicode.IsControl(r), thread.Hidden(r):
			return -1
		default:
			return r
		}
	}, s)
}

// direction is -> for threads the owner opened, <- for ones sent to them.
func direction(t rest.ThreadSummary) string {
	if t.Role == rest.ThreadSummaryRoleRecipient {
		return "<-"
	}

	return "->"
}

func turn(t rest.ThreadSummary) string {
	switch {
	case t.MyTurn:
		return "mine"
	case t.Turn.Set:
		return string(t.Turn.Value)
	default:
		return "nobody (ended)"
	}
}

func title(t rest.ThreadSummary) string {
	// Quoted, so a peer's title cannot pass for cpctl's own words.
	s := strconv.Quote(printable(string(t.Title), false))
	if t.Kind == rest.ThreadKindFyi {
		s = "[fyi] " + s
	}

	if t.Failed > 0 {
		s += fmt.Sprintf("  (%d %s undeliverable)", t.Failed, plural(int(t.Failed), "event"))
	} else if t.Undelivered > 0 {
		s += fmt.Sprintf("  (%d %s queued for delivery)", t.Undelivered, plural(int(t.Undelivered), "event"))
	}

	return s
}

func deliveryNote(e rest.ThreadEvent) string {
	d, ok := e.Delivery.Get()
	if !ok {
		return ""
	}

	switch d.Status {
	case rest.DeliveryStatusPending:
		s := fmt.Sprintf(" · queued for delivery (%d attempts)", d.Attempts)
		if d.LastError.Set {
			s += ": " + strconv.Quote(printable(d.LastError.Value, false))
		}

		return s
	case rest.DeliveryStatusFailed:
		return " · NOT DELIVERED: " + strconv.Quote(printable(d.LastError.Or("refused"), false))
	case rest.DeliveryStatusDelivered:
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
		return t.UTC().Format(time.DateOnly)
	}
}
