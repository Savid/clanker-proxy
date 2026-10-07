package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/savid/clanker-proxy/internal/delivery"
	"github.com/savid/clanker-proxy/internal/inbox"
	"github.com/savid/clanker-proxy/internal/server"
	"github.com/savid/clanker-proxy/internal/store"
)

// person is one owner with their own daemon.
type person struct {
	name  string
	url   string
	token string
}

// newPerson runs a daemon for name: store, inbox, API and both deliverers.
func newPerson(t *testing.T, name string) *person {
	t.Helper()

	ctx, cancel := context.WithCancel(context.Background())
	t.Cleanup(cancel)

	log := slog.New(slog.DiscardHandler)

	st, err := store.Open(ctx, filepath.Join(t.TempDir(), "cp.db"))
	if err != nil {
		t.Fatal(err)
	}

	t.Cleanup(func() { _ = st.Close() })

	// The server's URL is the daemon's own public URL, so the handler is
	// set once the server exists.
	var handler http.Handler

	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { handler.ServeHTTP(w, r) }))

	p := &person{name: name, url: srv.URL, token: inbox.NewSecret(inbox.OwnerPrefix)}
	deliverer := delivery.New(log, st, delivery.Config{})
	ib := inbox.New(log, st, deliverer, inbox.Config{Self: name, URL: srv.URL})
	deliverer.Attach(ib, ib.Wake())

	api, err := server.New(log, ib, server.Config{OwnerToken: p.token, Version: "test"})
	if err != nil {
		t.Fatal(err)
	}

	handler = api

	t.Cleanup(func() {
		api.Shutdown()
		srv.Close()
	})

	go func() { _ = deliverer.Run(ctx) }()
	go func() { _ = delivery.NewWebhooks(log, st, delivery.Config{}).Run(ctx) }()

	return p
}

// cp runs cpctl as p and returns its output.
func (p *person) cp(t *testing.T, args ...string) string {
	t.Helper()

	out, err := p.try(t, args...)
	if err != nil {
		t.Fatalf("%s: cpctl %s: %v\n%s", p.name, strings.Join(args, " "), err, out)
	}

	return out
}

func (p *person) try(t *testing.T, args ...string) (string, error) {
	t.Helper()

	var stdout, stderr bytes.Buffer

	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()

	full := append([]string{"-url", p.url, "-token", p.token}, args...)
	err := run(ctx, full, strings.NewReader(""), &stdout, &stderr)

	return stdout.String() + stderr.String(), err
}

func (p *person) json(t *testing.T, v any, args ...string) {
	t.Helper()

	if err := json.Unmarshal([]byte(p.cp(t, append([]string{"-json"}, args...)...)), v); err != nil {
		t.Fatal(err)
	}
}

// threadJSON is enough of a thread to check.
type threadJSON struct {
	ID     string `json:"id"`
	State  string `json:"state"`
	MyTurn bool   `json:"myTurn"`
	Log    []struct {
		Action   string `json:"action"`
		From     string `json:"from"`
		Body     string `json:"body"`
		Delivery struct {
			Status string `json:"status"`
		} `json:"delivery"`
	} `json:"log"`
}

func (p *person) thread(t *testing.T, ref string) threadJSON {
	t.Helper()

	var th threadJSON
	p.json(t, &th, "show", ref)

	return th
}

// waitTurn blocks until ref (or any thread) is p's turn or ended.
func (p *person) waitTurn(t *testing.T, ref string) threadJSON {
	t.Helper()

	args := []string{"wait", "-timeout", "5s"}
	if ref != "" {
		args = append(args, ref)
	}

	out, err := p.try(t, append([]string{"-json"}, args...)...)
	if err != nil {
		t.Fatalf("%s: wait %s: %v\n%s\nthreads:\n%s", p.name, ref, err, out, p.cp(t, "ls"))
	}

	var th threadJSON
	if err = json.Unmarshal([]byte(out), &th); err != nil {
		t.Fatal(err)
	}

	return th
}

// peer makes from ask to, checks to sees the same code, and approves.
func peer(t *testing.T, from, to *person) {
	t.Helper()

	var asked struct {
		Code   string `json:"code"`
		Status string `json:"status"`
	}
	from.json(t, &asked, "peer", "add", to.name, to.url, "-m", "hi, it's "+from.name)

	var reqs struct {
		Requests []struct {
			ID, Name, Code, Note string
		} `json:"requests"`
	}
	to.json(t, &reqs, "requests")

	if len(reqs.Requests) != 1 || reqs.Requests[0].Code != asked.Code || reqs.Requests[0].Name != from.name ||
		asked.Status != "requested" {
		t.Fatalf("asked %+v; %s sees %+v", asked, to.name, reqs)
	}

	to.cp(t, "approve", reqs.Requests[0].ID)
}

func TestPeersWorkAThread(t *testing.T) {
	t.Parallel()

	alice, bob := newPerson(t, "alice"), newPerson(t, "bob")

	if out := alice.cp(t, "me"); !strings.Contains(out, "alice") || !strings.Contains(out, alice.url) {
		t.Fatalf("me = %s", out)
	}

	peer(t, alice, bob)

	if out := bob.cp(t, "peer", "ls"); !strings.Contains(out, "alice") || !strings.Contains(out, "active") {
		t.Fatalf("bob's peers = %s", out)
	}

	// Approval reaches alice, so her side is active too.
	deadline := time.Now().Add(5 * time.Second)
	for !strings.Contains(alice.cp(t, "peer", "ls"), "active") {
		if time.Now().After(deadline) {
			t.Fatalf("alice's peer never became active: %s", alice.cp(t, "peer", "ls"))
		}

		time.Sleep(20 * time.Millisecond)
	}

	var sent threadJSON
	alice.json(t, &sent, "send", "bob", "Review the plan", "-m", "Can you look?", "-l", "infra")

	ref := sent.ID[:8]

	if got := bob.waitTurn(t, ""); got.ID != sent.ID || !got.MyTurn {
		t.Fatalf("bob's wait = %+v", got)
	}

	if out := bob.cp(t, "inbox"); !strings.Contains(out, "Review the plan") || !strings.Contains(out, "<- alice") {
		t.Fatalf("bob's inbox = %s", out)
	}

	bob.cp(t, "ack", ref)
	bob.cp(t, "needs-input", ref, "-m", "Which env?")

	if got := alice.waitTurn(t, ref); got.State != "needs-input" || !got.MyTurn {
		t.Fatalf("alice's wait = %+v", got)
	}

	alice.cp(t, "reply", ref, "-m", "staging")

	if got := bob.waitTurn(t, ref); got.State != "acked" {
		t.Fatalf("bob's wait after answer = %+v", got)
	}

	bob.cp(t, "resolve", ref, "-m", "LGTM")
	alice.waitTurn(t, ref)
	alice.cp(t, "close", ref)

	converge(t, alice, bob, ref, "closed", 6)
}

// converge waits until both hold the thread in state with n events, and
// checks they hold the same events, each named from its own side.
func converge(t *testing.T, p, q *person, ref, state string, n int) {
	t.Helper()

	deadline := time.Now().Add(5 * time.Second)

	a, b := p.thread(t, ref), q.thread(t, ref)
	for a.State != state || b.State != state || len(a.Log) != n || len(b.Log) != n {
		if time.Now().After(deadline) {
			t.Fatalf("threads did not converge: %s %+v, %s %+v", p.name, a, q.name, b)
		}

		time.Sleep(20 * time.Millisecond)

		a, b = p.thread(t, ref), q.thread(t, ref)
	}

	for i := range a.Log {
		if a.Log[i].Action != b.Log[i].Action || a.Log[i].Body != b.Log[i].Body || a.Log[i].From != b.Log[i].From {
			t.Fatalf("event %d differs: %+v vs %+v", i, a.Log[i], b.Log[i])
		}
	}
}

// What alice sends before bob approves waits, and arrives once he does.
func TestSendBeforeApproval(t *testing.T) {
	t.Parallel()

	alice, bob := newPerson(t, "alice"), newPerson(t, "bob")

	alice.cp(t, "peer", "add", "bob", bob.url)

	var sent threadJSON
	alice.json(t, &sent, "send", "bob", "Hello early")

	deadline := time.Now().Add(5 * time.Second)
	for !strings.Contains(alice.cp(t, "show", sent.ID), "401") {
		if time.Now().After(deadline) {
			t.Fatalf("delivery did not wait on approval: %s", alice.cp(t, "show", sent.ID))
		}

		time.Sleep(20 * time.Millisecond)
	}

	var reqs struct {
		Requests []struct{ ID string } `json:"requests"`
	}
	bob.json(t, &reqs, "requests")
	bob.cp(t, "approve", reqs.Requests[0].ID, "-as", "al")

	// The approval makes alice's queued event due at once, not at its
	// backoff.
	if got := bob.waitTurn(t, ""); got.ID != sent.ID || got.State != "open" || !got.MyTurn {
		t.Fatalf("bob's thread = %+v", got)
	}

	if got := bob.thread(t, sent.ID); got.Log[0].From != "al" {
		t.Fatalf("bob names the sender %q, want al", got.Log[0].From)
	}
}

// exitOf is the exit code cpctl would end with.
func exitOf(err error) int {
	if f, ok := errors.AsType[*failure](err); ok {
		return f.exit
	}

	return 0
}

// Each refusal ends with the exit code an agent branches on.
func TestRefusals(t *testing.T) {
	t.Parallel()

	alice, bob := newPerson(t, "alice"), newPerson(t, "bob")

	if _, err := alice.try(t, "send", "bob", "hi"); exitOf(err) != exitNotFound {
		t.Fatalf("send to non-peer = %v", err)
	}

	peer(t, alice, bob)

	var sent threadJSON
	alice.json(t, &sent, "send", "bob", "Do the thing")

	out, err := alice.try(t, "resolve", sent.ID, "-m", "done")
	if exitOf(err) != exitRefused || !strings.Contains(out, "you can now comment, withdraw") || !strings.Contains(out, "hint: cpctl show") {
		t.Fatalf("sender resolve = %v\n%s", err, out)
	}

	if _, err = bob.try(t, "show", "zzzz"); exitOf(err) != exitNotFound {
		t.Fatalf("show unknown = %v", err)
	}

	stranger := &person{name: "stranger", url: bob.url, token: inbox.NewSecret(inbox.OwnerPrefix)}
	if _, err = stranger.try(t, "inbox"); exitOf(err) != exitAuth {
		t.Fatalf("wrong owner token = %v", err)
	}

	// The stream refuses it too, and wait gives up at once instead of
	// retrying until its timeout.
	if _, err = stranger.try(t, "wait", "-timeout", "5s"); exitOf(err) != exitAuth {
		t.Fatalf("wait with a wrong owner token = %v", err)
	}

	if _, err = bob.try(t, "approve", "nope"); exitOf(err) != exitUsage {
		t.Fatalf("approve malformed = %v", err)
	}

	if _, err = bob.try(t, "approve", "deadbeef"); exitOf(err) != exitNotFound {
		t.Fatalf("approve unknown = %v", err)
	}

	if _, err = bob.try(t, "resolve", sent.ID); exitOf(err) != exitUsage {
		t.Fatalf("resolve without a body = %v", err)
	}

	bob.waitTurn(t, "")
	if _, err = bob.try(t, "wait", sent.ID[:8], "-timeout", "50ms"); err != nil {
		t.Fatalf("wait on a thread already waiting on bob = %v", err)
	}

	if _, err = alice.try(t, "wait", sent.ID[:8], "-timeout", "50ms"); exitOf(err) != exitTimeout {
		t.Fatalf("wait that times out = %v", err)
	}

	unreachable := &person{name: "alice", url: "http://127.0.0.1:1", token: alice.token}
	if _, err = unreachable.try(t, "me"); exitOf(err) != exitUnreachable {
		t.Fatalf("cpd not running = %v", err)
	}
}

// An agent learns the CLI from its help: every command is listed, and each
// has its own help with flags and an example.
func TestHelp(t *testing.T) {
	t.Parallel()

	var stdout, stderr bytes.Buffer
	if err := run(t.Context(), nil, strings.NewReader(""), &stdout, &stderr); err != nil {
		t.Fatal(err)
	}

	for _, c := range commands() {
		if !strings.Contains(stdout.String(), "  "+c.name) {
			t.Errorf("overview does not list %q", c.name)
		}

		var one bytes.Buffer
		if err := run(t.Context(), append([]string{"help"}, strings.Fields(c.name)...), strings.NewReader(""), &one, &stderr); err != nil {
			t.Fatalf("help %s: %v", c.name, err)
		}

		if !strings.HasPrefix(one.String(), "cpctl "+c.name) || !strings.Contains(one.String(), "Example:\n  cpctl "+c.name) {
			t.Errorf("help %s =\n%s", c.name, one.String())
		}
	}

	stdout.Reset()

	if err := run(t.Context(), []string{"send", "-h"}, strings.NewReader(""), &stdout, &stderr); err != nil ||
		!strings.Contains(stdout.String(), "Example:") {
		t.Errorf("send -h = %v\n%s", err, stdout.String())
	}

	for _, args := range [][]string{{"frobnicate"}, {"peer"}, {"show"}, {"ls", "-bogus"}} {
		stderr.Reset()

		err := run(t.Context(), args, strings.NewReader(""), &stdout, &stderr)
		if exitOf(err) != exitUsage || !strings.Contains(stderr.String(), "hint: cpctl help") {
			t.Errorf("%v = %v\n%s", args, err, stderr.String())
		}
	}
}

// Text output ends with the commands that come next; -json may follow the
// command and turns errors into JSON too.
func TestAgentOutput(t *testing.T) {
	t.Parallel()

	alice, bob := newPerson(t, "alice"), newPerson(t, "bob")
	peer(t, alice, bob)

	out := alice.cp(t, "send", "bob", "Review", "-m", "please")
	if !strings.Contains(out, "next:\n  cpctl wait ") || !strings.Contains(out, "cpctl withdraw ") {
		t.Errorf("send output:\n%s", out)
	}

	var sum struct {
		ID      string   `json:"id"`
		Role    string   `json:"role"`
		Actions []string `json:"actions"`
		Log     any      `json:"log"`
	}
	if err := json.Unmarshal([]byte(alice.cp(t, "send", "bob", "Second", "-json")), &sum); err != nil {
		t.Fatal(err)
	}

	if sum.Role != "sender" || strings.Join(sum.Actions, ",") != "comment,withdraw" || sum.Log != nil {
		t.Errorf("send -json = %+v", sum)
	}

	deadline := time.Now().Add(5 * time.Second)
	for _, err := bob.try(t, "show", sum.ID); err != nil; _, err = bob.try(t, "show", sum.ID) {
		if time.Now().After(deadline) {
			t.Fatalf("thread never reached bob: %v", err)
		}

		time.Sleep(20 * time.Millisecond)
	}

	if out = bob.cp(t, "show", sum.ID[:8]); !strings.Contains(out, "cpctl resolve "+sum.ID[:8]+` -m "<result>"`) {
		t.Errorf("recipient's show:\n%s", out)
	}

	out, err := bob.try(t, "show", "zzzz", "-json")

	var e struct {
		Error struct {
			Exit   int    `json:"exit"`
			Status int    `json:"status"`
			Hint   string `json:"hint"`
		} `json:"error"`
	}
	if jsonErr := json.Unmarshal([]byte(out), &e); jsonErr != nil || exitOf(err) != exitNotFound ||
		e.Error.Exit != exitNotFound || e.Error.Status != http.StatusNotFound || e.Error.Hint == "" {
		t.Errorf("show unknown -json = %v, %+v\n%s", err, e, out)
	}
}

// When the other side has already asked, peer add points at their request
// instead of crossing it with a second one.
func TestPeerAddWhenAlreadyAsked(t *testing.T) {
	t.Parallel()

	alice, bob := newPerson(t, "alice"), newPerson(t, "bob")
	alice.cp(t, "peer", "add", "bob", bob.url)

	out, err := bob.try(t, "peer", "add", "alice", alice.url)
	if exitOf(err) != exitRefused || !strings.Contains(out, "approve that request") {
		t.Fatalf("peer add back = %v\n%s", err, out)
	}
}

func TestDuplicatePeerKeepsExistingThreads(t *testing.T) {
	t.Parallel()

	alice, bob := newPerson(t, "alice"), newPerson(t, "bob")
	peer(t, alice, bob)

	var sent threadJSON
	alice.json(t, &sent, "send", "bob", "Original thread")
	bob.waitTurn(t, "")

	for _, url := range []string{bob.url, bob.url + "/"} {
		out, err := alice.try(t, "peer", "add", "robert", url)
		if exitOf(err) != exitRefused || !strings.Contains(out, "already a peer") {
			t.Fatalf("duplicate daemon = %v\n%s", err, out)
		}
	}

	var requests struct {
		Requests []struct{ ID string } `json:"requests"`
	}
	bob.json(t, &requests, "requests")
	if len(requests.Requests) != 0 {
		t.Fatal("duplicate daemon received a new peering request")
	}

	bob.cp(t, "resolve", sent.ID, "-m", "Done")
	alice.waitTurn(t, sent.ID)
	converge(t, alice, bob, sent.ID, "resolved", 2)
}

// A stranger cannot pass off a request as a friend's: approving it calls the
// URL it gave, and the friend's daemon does not hold the stranger's secret.
func TestImpersonatedRequestIsNotApproved(t *testing.T) {
	t.Parallel()

	alice, bob := newPerson(t, "alice"), newPerson(t, "bob")

	body := `{"name":"alice","url":"` + alice.url + `","secret":"` + inbox.NewSecret(inbox.PeerPrefix) + `"}`

	req, err := http.NewRequestWithContext(t.Context(), http.MethodPost, bob.url+"/api/v1/peering-requests", strings.NewReader(body))
	if err != nil {
		t.Fatal(err)
	}

	req.Header.Set("Content-Type", "application/json")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}

	_ = resp.Body.Close()

	if resp.StatusCode != http.StatusAccepted {
		t.Fatalf("forged request answered %d", resp.StatusCode)
	}

	var reqs struct {
		Requests []struct{ ID string } `json:"requests"`
	}
	bob.json(t, &reqs, "requests")

	if _, err = bob.try(t, "approve", reqs.Requests[0].ID); err == nil || !strings.Contains(err.Error(), "did not confirm") {
		t.Fatalf("approving the forgery = %v", err)
	}

	if out := bob.cp(t, "peer", "ls"); strings.Contains(out, "alice") {
		t.Fatalf("forgery became a peer: %s", out)
	}
}
