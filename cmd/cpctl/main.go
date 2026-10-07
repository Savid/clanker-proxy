// Command cpctl drives a clanker-proxy daemon (cpd) as its owner: peer with
// other daemons, send them threads, and work the threads they send you.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"os"
	"os/signal"
	"slices"
	"strings"
	"syscall"
	"time"

	"github.com/savid/clanker-proxy/api/rest"
	"github.com/savid/clanker-proxy/pkg/thread"
)

// version is set at build time: -ldflags "-X main.version=...".
var version = "dev"

// Exit codes: 1 for errors, 2 when wait times out.
const (
	exitError   = 1
	exitTimeout = 2
)

const usage = `cpctl drives your clanker-proxy daemon (cpd).

Usage: cpctl [-url url] [-token token] [-json] <command> [args] [flags]

You and peers:
  me                          your name and the URL peers reach you at
  peer add <name> <url> [-m note]
                              ask the daemon at <url> to connect; <name> is
                              what you call them
  peer ls                     list peers
  peer show <name>            a peer's status and code
  peer rm <name>              forget a peer
  requests                    peering requests waiting for you
  approve <id> [-as name]     accept one; compare its code with theirs first
  deny <id>                   refuse one

Threads (a <ref> is a thread ID or a unique prefix of it):
  inbox                       threads waiting on you
  ls [-turn mine|theirs|none] [-state s] [-peer p] [-label l] [-n 50]
  show <ref>                  a thread and its events
  send <peer> <title> [-m body] [-fyi] [-l label]...
                              open a thread; -fyi closes once they ack
  reply <ref> -m body         comment, in any state
  ack <ref> [-m body]         recipient: accept the thread
  needs-input <ref> -m q      recipient: ask the sender something
  resolve <ref> -m answer     recipient: done, here is the result
  decline <ref> [-m why]      recipient: will not do it
  close <ref> [-m body]       sender: accept the resolution
  reopen <ref> -m why         sender: not done after all
  withdraw <ref> [-m why]     sender: no longer needed
  watch                       print each thread change as it happens
  wait [<ref>] [-timeout d]   block until <ref> is your turn or has ended, or
                              with no <ref> until any thread is your turn;
                              exit 2 on timeout

-m - reads the body from stdin. A command's own flags may follow its
arguments; global flags go before the command.

Global flags:
`

func main() {
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	err := run(ctx, os.Args[1:], os.Stdin, os.Stdout, os.Stderr)

	stop()

	switch {
	case err == nil, errors.Is(err, flag.ErrHelp):
	case errors.Is(err, errTimeout):
		fmt.Fprintln(os.Stderr, "cpctl:", err)
		os.Exit(exitTimeout)
	default:
		fmt.Fprintln(os.Stderr, "cpctl:", err)
		os.Exit(exitError)
	}
}

// app is one cpctl invocation.
type app struct {
	ctx    context.Context
	stdin  io.Reader
	stdout io.Writer
	stderr io.Writer
	url    string
	token  string
	json   bool
	now    func() time.Time

	client *rest.Client
}

type command func(a *app, args []string) error

func commands() map[string]command {
	cmds := map[string]command{
		"me":       cmdMe,
		"peer":     cmdPeer,
		"requests": cmdRequests,
		"approve":  cmdApprove,
		"deny":     cmdDeny,
		"inbox":    cmdInbox,
		"ls":       cmdList,
		"show":     cmdShow,
		"send":     cmdSend,
		"reply":    cmdAction(thread.ActionComment),
		"watch":    cmdWatch,
		"wait":     cmdWait,
	}
	for _, a := range []thread.Action{
		thread.ActionAck, thread.ActionNeedsInput, thread.ActionResolve, thread.ActionDecline,
		thread.ActionClose, thread.ActionReopen, thread.ActionWithdraw,
	} {
		cmds[string(a)] = cmdAction(a)
	}

	return cmds
}

func run(ctx context.Context, args []string, stdin io.Reader, stdout, stderr io.Writer) error {
	a := &app{ctx: ctx, stdin: stdin, stdout: stdout, stderr: stderr, now: time.Now}

	fs := flag.NewFlagSet("cpctl", flag.ContinueOnError)
	fs.SetOutput(stderr)
	fs.StringVar(&a.url, "url", defaultURL(), "cpd's URL (env CP_URL)")
	fs.StringVar(&a.token, "token", "", "the owner token (default: $CP_TOKEN, else owner.token in $CP_DIR or ~/.cp)")
	fs.BoolVar(&a.json, "json", false, "print API responses as JSON")
	showVersion := fs.Bool("version", false, "print the version and exit")
	fs.Usage = func() {
		fmt.Fprint(stderr, usage)
		fs.PrintDefaults()
	}

	if err := fs.Parse(args); err != nil {
		return err
	}

	if a.token == "" {
		a.token = defaultToken()
	}

	if *showVersion {
		fmt.Fprintln(stdout, version)

		return nil
	}

	if fs.NArg() == 0 {
		fs.Usage()

		return flag.ErrHelp
	}

	name, rest := fs.Arg(0), fs.Args()[1:]

	cmd, ok := commands()[name]
	if !ok {
		names := make([]string, 0, len(commands()))
		for n := range commands() {
			names = append(names, n)
		}

		slices.Sort(names)

		return fmt.Errorf("unknown command %q; commands: %s", name, strings.Join(names, ", "))
	}

	var err error
	if a.client, err = newClient(a.url, a.token); err != nil {
		return err
	}

	if err = cmd(a, rest); err != nil {
		return explain(err, a.url)
	}

	return nil
}

// parseArgs parses flags wherever they appear among the arguments and
// returns the positional ones.
func parseArgs(fs *flag.FlagSet, args []string, want int, names string) ([]string, error) {
	fs.SetOutput(io.Discard)

	var pos []string

	for {
		if err := fs.Parse(args); err != nil {
			return nil, fmt.Errorf("%s: %w", fs.Name(), err)
		}

		args = fs.Args()
		if len(args) == 0 {
			break
		}

		pos = append(pos, args[0])
		args = args[1:]
	}

	if want >= 0 && len(pos) != want {
		return nil, fmt.Errorf("usage: cpctl %s %s", fs.Name(), names)
	}

	return pos, nil
}

// stringList is a repeatable string flag.
type stringList []string

func (s *stringList) String() string { return strings.Join(*s, ",") }

func (s *stringList) Set(v string) error {
	*s = append(*s, v)

	return nil
}
