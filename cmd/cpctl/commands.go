package main

import (
	"errors"
	"flag"
	"fmt"
	"io"
	"net/url"
	"slices"
	"strings"

	"github.com/savid/clanker-proxy/api/rest"
	"github.com/savid/clanker-proxy/pkg/thread"
)

func cmdMe(a *app, args []string) error {
	if _, err := parseArgs(flag.NewFlagSet("me", flag.ContinueOnError), args, 0, ""); err != nil {
		return err
	}

	me, err := a.client.GetMe(a.ctx)
	if err != nil {
		return err
	}

	return a.print(me, func(w io.Writer) { printMe(w, me) })
}

func cmdPeer(a *app, args []string) error {
	if len(args) == 0 {
		return errors.New("usage: cpctl peer add|ls|show|rm")
	}

	fs := flag.NewFlagSet("peer "+args[0], flag.ContinueOnError)

	switch args[0] {
	case "add":
		return peerAdd(a, fs, args[1:])
	case "ls":
		if _, err := parseArgs(fs, args[1:], 0, ""); err != nil {
			return err
		}

		ps, err := a.client.ListPeers(a.ctx)
		if err != nil {
			return err
		}

		return a.print(ps, func(w io.Writer) { printPeers(w, ps.Peers) })
	case "show":
		pos, err := parseArgs(fs, args[1:], 1, "<name>")
		if err != nil {
			return err
		}

		p, err := a.client.GetPeer(a.ctx, rest.GetPeerParams{Name: name(pos[0])})
		if err != nil {
			return err
		}

		return a.print(p, func(w io.Writer) { printPeers(w, []rest.Peer{*p}) })
	case "rm":
		pos, err := parseArgs(fs, args[1:], 1, "<name>")
		if err != nil {
			return err
		}

		if err = a.client.RemovePeer(a.ctx, rest.RemovePeerParams{Name: name(pos[0])}); err != nil {
			return err
		}

		a.say("removed %s\n", pos[0])

		return nil
	default:
		return fmt.Errorf("unknown peer command %q; want add, ls, show or rm", args[0])
	}
}

func peerAdd(a *app, fs *flag.FlagSet, args []string) error {
	note := fs.String("m", "", "a note for their owner")

	pos, err := parseArgs(fs, args, 2, "<name> <url> [-m note]")
	if err != nil {
		return err
	}

	u, err := url.Parse(pos[1])
	if err != nil {
		return fmt.Errorf("url: %w", err)
	}

	in := &rest.PeerInput{Name: name(pos[0]), URL: rest.DaemonURL(*u)}
	if *note != "" {
		in.Note = rest.NewOptNote(rest.Note(*note))
	}

	p, err := a.client.AddPeer(a.ctx, in)
	if err != nil {
		return err
	}

	return a.print(p, func(w io.Writer) {
		fmt.Fprintf(w, "asked %s to peer; code %s\n", p.Name, p.Code)
		fmt.Fprintf(w, "they run: cpctl requests, check the code is %s, then cpctl approve <id>\n", p.Code)
		fmt.Fprintln(w, "anything you send them meanwhile is delivered once they approve")
	})
}

func cmdRequests(a *app, args []string) error {
	if _, err := parseArgs(flag.NewFlagSet("requests", flag.ContinueOnError), args, 0, ""); err != nil {
		return err
	}

	l, err := a.client.ListRequests(a.ctx)
	if err != nil {
		return err
	}

	return a.print(l, func(w io.Writer) { printRequests(w, l.Requests, a.now()) })
}

func cmdApprove(a *app, args []string) error {
	fs := flag.NewFlagSet("approve", flag.ContinueOnError)
	as := fs.String("as", "", "what to call them (default: the name they asked for)")

	pos, err := parseArgs(fs, args, 1, "<id> [-as name]")
	if err != nil {
		return err
	}

	in := &rest.Approval{}
	if *as != "" {
		in.Name = rest.NewOptName(name(*as))
	}

	p, err := a.client.ApproveRequest(a.ctx, in, rest.ApproveRequestParams{ID: rest.RequestRef(pos[0])})
	if err != nil {
		return err
	}

	return a.print(p, func(w io.Writer) { fmt.Fprintf(w, "%s is now a peer (%s)\n", p.Name, urlString(p.URL)) })
}

func cmdDeny(a *app, args []string) error {
	pos, err := parseArgs(flag.NewFlagSet("deny", flag.ContinueOnError), args, 1, "<id>")
	if err != nil {
		return err
	}

	if err = a.client.DenyRequest(a.ctx, rest.DenyRequestParams{ID: rest.RequestRef(pos[0])}); err != nil {
		return err
	}

	a.say("denied %s\n", pos[0])

	return nil
}

func cmdInbox(a *app, args []string) error {
	if _, err := parseArgs(flag.NewFlagSet("inbox", flag.ContinueOnError), args, 0, ""); err != nil {
		return err
	}

	return a.list(rest.ListThreadsParams{Turn: rest.NewOptListThreadsTurn(rest.ListThreadsTurnMine)})
}

func cmdList(a *app, args []string) error {
	fs := flag.NewFlagSet("ls", flag.ContinueOnError)
	turn := fs.String("turn", "", "mine, theirs or none")
	state := fs.String("state", "", "a thread state")
	peer := fs.String("peer", "", "a peer's name")
	label := fs.String("label", "", "a label")
	limit := fs.Uint("n", 50, "at most this many (at most 500)")

	if _, err := parseArgs(fs, args, 0, "[-turn t] [-state s] [-peer p] [-label l] [-n 50]"); err != nil {
		return err
	}

	p := rest.ListThreadsParams{Limit: rest.NewOptInt32(int32(min(*limit, 500)))}
	if *turn != "" {
		p.Turn = rest.NewOptListThreadsTurn(rest.ListThreadsTurn(*turn))
	}

	if *state != "" {
		p.State = rest.NewOptThreadState(rest.ThreadState(*state))
	}

	if *peer != "" {
		p.Peer = rest.NewOptName(name(*peer))
	}

	if *label != "" {
		p.Label = rest.NewOptLabel(rest.Label(strings.ToLower(*label)))
	}

	return a.list(p)
}

func (a *app) list(p rest.ListThreadsParams) error {
	l, err := a.client.ListThreads(a.ctx, p)
	if err != nil {
		return err
	}

	return a.print(l, func(w io.Writer) { printThreads(w, l.Threads, a.now()) })
}

func cmdShow(a *app, args []string) error {
	pos, err := parseArgs(flag.NewFlagSet("show", flag.ContinueOnError), args, 1, "<ref>")
	if err != nil {
		return err
	}

	t, err := a.client.GetThread(a.ctx, rest.GetThreadParams{Ref: pos[0]})
	if err != nil {
		return err
	}

	return a.print(t, func(w io.Writer) { printThread(w, t) })
}

func cmdSend(a *app, args []string) error {
	fs := flag.NewFlagSet("send", flag.ContinueOnError)
	body := fs.String("m", "", "body (markdown); - reads stdin")
	fyi := fs.Bool("fyi", false, "no response expected: the thread closes when acknowledged")

	var labels stringList
	fs.Var(&labels, "l", "a label; repeat for more")

	pos, err := parseArgs(fs, args, 2, "<peer> <title> [-m body] [-fyi] [-l label]...")
	if err != nil {
		return err
	}

	text, err := a.body(*body)
	if err != nil {
		return err
	}

	in := &rest.NewThread{To: name(pos[0]), Title: rest.Title(strings.TrimSpace(pos[1]))}
	for _, l := range labels {
		in.Labels = append(in.Labels, rest.Label(strings.ToLower(strings.TrimSpace(l))))
	}

	if text != "" {
		in.Body = rest.NewOptBody(rest.Body(text))
	}

	if *fyi {
		in.Kind = rest.NewOptThreadKind(rest.ThreadKindFyi)
	}

	t, err := a.client.OpenThread(a.ctx, in)
	if err != nil {
		return err
	}

	return a.print(t, func(w io.Writer) { printThread(w, t) })
}

// cmdAction returns the command for an action on an existing thread.
func cmdAction(action thread.Action) command {
	return func(a *app, args []string) error {
		name := string(action)
		if action == thread.ActionComment {
			name = "reply"
		}

		fs := flag.NewFlagSet(name, flag.ContinueOnError)
		body := fs.String("m", "", "body (markdown); - reads stdin")

		pos, err := parseArgs(fs, args, 1, "<ref> [-m body]")
		if err != nil {
			return err
		}

		text, err := a.body(*body)
		if err != nil {
			return err
		}

		needsBody := []thread.Action{thread.ActionComment, thread.ActionNeedsInput, thread.ActionResolve, thread.ActionReopen}
		if text == "" && slices.Contains(needsBody, action) {
			return fmt.Errorf("%s needs a body: -m text, or -m - for stdin", name)
		}

		in := &rest.ThreadAction{Action: rest.Action(action)}
		if text != "" {
			in.Body = rest.NewOptBody(rest.Body(text))
		}

		t, err := a.client.ActOnThread(a.ctx, in, rest.ActOnThreadParams{Ref: pos[0]})
		if err != nil {
			return err
		}

		return a.print(t, func(w io.Writer) { printThread(w, t) })
	}
}

// body returns -m's value, reading stdin for "-".
func (a *app) body(m string) (string, error) {
	if m != "-" {
		return m, nil
	}

	b, err := io.ReadAll(io.LimitReader(a.stdin, thread.MaxBody+1))
	if err != nil {
		return "", fmt.Errorf("read body: %w", err)
	}

	if len(b) > thread.MaxBody {
		return "", fmt.Errorf("body is over %d bytes", thread.MaxBody)
	}

	return strings.TrimRight(string(b), "\n"), nil
}

// name lower-cases a name as typed: names are lower-case on the wire.
func name(s string) rest.Name {
	return rest.Name(strings.ToLower(strings.TrimSpace(s)))
}

func urlString(u rest.DaemonURL) string {
	plain := url.URL(u)

	return plain.String()
}

// say prints a confirmation, except with -json.
func (a *app) say(format string, args ...any) {
	if !a.json {
		fmt.Fprintf(a.stdout, format, args...)
	}
}
