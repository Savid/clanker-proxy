package thread_test

import (
	"fmt"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/savid/clanker-proxy/pkg/thread"
)

const (
	threadID = "00000000-0000-4000-8000-000000000000"
	alice    = "alice"
	bob      = "bob"
)

var t0 = time.Date(2026, 10, 7, 0, 0, 0, 0, time.UTC)

// builder mints events for one thread, one minute apart.
type builder struct{ n int }

func (b *builder) open(kind thread.Kind) thread.Event {
	return thread.Event{
		ID: threadID, Thread: threadID, From: alice, To: bob, At: t0, Clock: 1,
		Action: thread.ActionOpen, Title: "Review the plan", Kind: kind, Body: "Can you look?",
	}
}

func (b *builder) ev(from string, a thread.Action) thread.Event {
	b.n++
	to := bob
	if from == bob {
		to = alice
	}

	return thread.Event{
		ID: fmt.Sprintf("00000000-0000-4000-8000-%012d", b.n), Thread: threadID,
		From: from, To: to, At: t0.Add(time.Duration(b.n) * time.Minute), Clock: int64(b.n) + 1, Action: a,
	}
}

func TestWorkflow(t *testing.T) {
	t.Parallel()

	type step struct {
		from    string
		action  thread.Action
		state   thread.State
		ignored string
	}

	for name, tc := range map[string]struct {
		kind  thread.Kind
		steps []step
	}{
		"request to closed": {thread.KindRequest, []step{
			{bob, thread.ActionAck, thread.StateAcked, ""},
			{bob, thread.ActionNeedsInput, thread.StateNeedsInput, ""},
			{alice, thread.ActionComment, thread.StateAcked, ""},
			{bob, thread.ActionComment, thread.StateAcked, ""},
			{bob, thread.ActionResolve, thread.StateResolved, ""},
			{alice, thread.ActionReopen, thread.StateAcked, ""},
			{bob, thread.ActionResolve, thread.StateResolved, ""},
			{alice, thread.ActionClose, thread.StateClosed, ""},
			{alice, thread.ActionComment, thread.StateClosed, ""},
			{alice, thread.ActionReopen, thread.StateAcked, ""},
		}},
		"fyi closes on ack": {thread.KindFYI, []step{
			{bob, thread.ActionAck, thread.StateClosed, ""},
		}},
		"recipient-only actions": {thread.KindRequest, []step{
			{alice, thread.ActionAck, thread.StateOpen, "only the recipient may ack"},
			{alice, thread.ActionResolve, thread.StateOpen, "only the recipient may resolve"},
			{bob, thread.ActionResolve, thread.StateResolved, ""},
			{bob, thread.ActionClose, thread.StateResolved, "only the sender may close"},
		}},
		"terminal states hold": {thread.KindRequest, []step{
			{bob, thread.ActionDecline, thread.StateDeclined, ""},
			{bob, thread.ActionAck, thread.StateDeclined, "cannot ack a thread that is declined"},
			{alice, thread.ActionReopen, thread.StateDeclined, "cannot reopen a thread that is declined"},
			{alice, thread.ActionWithdraw, thread.StateDeclined, "cannot withdraw a thread that is declined"},
		}},
		"sender withdraws": {thread.KindRequest, []step{
			{bob, thread.ActionAck, thread.StateAcked, ""},
			{alice, thread.ActionWithdraw, thread.StateWithdrawn, ""},
		}},
		"no second open": {thread.KindRequest, []step{
			{alice, thread.ActionOpen, thread.StateOpen, "thread is already open"},
		}},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			var b builder

			events := []thread.Event{b.open(tc.kind)}
			for _, s := range tc.steps {
				events = append(events, b.ev(s.from, s.action))

				got, err := thread.Replay(events)
				if err != nil {
					t.Fatal(err)
				}

				last := got.Events[len(got.Events)-1]
				if got.State != s.state || last.Ignored != s.ignored {
					t.Fatalf("after %s %s: state %s ignored %q, want %s %q", s.from, s.action, got.State, last.Ignored, s.state, s.ignored)
				}
			}
		})
	}
}

// Two peers acting at once see the same result once both events arrive,
// whichever arrived first.
func TestReplayIsOrderIndependent(t *testing.T) {
	t.Parallel()

	var b builder

	open := b.open(thread.KindRequest)
	resolve := b.ev(bob, thread.ActionResolve)
	withdraw := b.ev(alice, thread.ActionWithdraw)
	// Both acted having seen only the open: their clocks are equal, and the
	// IDs break the tie the same way on both sides.
	withdraw.Clock = resolve.Clock

	first, err := thread.Replay([]thread.Event{withdraw, resolve, open})
	if err != nil {
		t.Fatal(err)
	}

	second, err := thread.Replay([]thread.Event{open, resolve, withdraw})
	if err != nil {
		t.Fatal(err)
	}

	if first.State != second.State || first.State != thread.StateResolved {
		t.Fatalf("states %s and %s, want both resolved", first.State, second.State)
	}

	if ignored := second.Events[2].Ignored; !strings.Contains(ignored, "cannot withdraw") {
		t.Errorf("late withdraw ignored = %q", ignored)
	}

	// An event follows what its author had seen, even with an earlier time.
	ack := b.ev(bob, thread.ActionAck)
	ack.At = open.At.Add(-time.Hour)

	got, err := thread.Replay([]thread.Event{ack, open})
	if err != nil || got.State != thread.StateAcked {
		t.Errorf("ack written before the open by the clock on the wall: %s, %v", got.State, err)
	}
}

func TestReplayErrors(t *testing.T) {
	t.Parallel()

	var b builder

	if _, err := thread.Replay([]thread.Event{b.ev(bob, thread.ActionAck)}); err == nil {
		t.Error("replay without open succeeded")
	}

	stray := b.ev(bob, thread.ActionAck)
	stray.Thread = "00000000-0000-4000-8000-ffffffffffff"

	if _, err := thread.Replay([]thread.Event{b.open(thread.KindRequest), stray}); err == nil {
		t.Error("replay with another thread's event succeeded")
	}

	outsider := b.ev(bob, thread.ActionComment)
	outsider.From = "mallory"

	got, err := thread.Replay([]thread.Event{b.open(thread.KindRequest), outsider})
	if err != nil || got.Events[1].Ignored == "" {
		t.Errorf("outsider comment: %v, ignored %q", err, got.Events[1].Ignored)
	}
}

func TestCheckClock(t *testing.T) {
	t.Parallel()

	var b builder

	resolve := b.ev(bob, thread.ActionResolve) // clock 2
	closed := b.ev(alice, thread.ActionClose)  // clock 3

	th, err := thread.Replay([]thread.Event{b.open(thread.KindRequest), resolve, closed})
	if err != nil {
		t.Fatal(err)
	}

	for name, tc := range map[string]struct {
		from  string
		clock int64
		ok    bool
	}{
		"after everything":           {bob, 4, true},
		"concurrent with alice":      {bob, 3, true},
		"at bob's own earlier clock": {bob, 2, false},
		"before bob's own clock":     {bob, 1, false},
		"at alice's own clock":       {alice, 3, false},
		"at the lead limit":          {bob, th.NextClock() + 1000, true},
		"past the lead limit":        {bob, th.NextClock() + 1001, false},
	} {
		e := b.ev(tc.from, thread.ActionDecline)
		e.Clock = tc.clock

		if got := th.CheckClock(e); (got == nil) != tc.ok {
			t.Errorf("%s: CheckClock = %v, want ok %v", name, got, tc.ok)
		}
	}
}

func TestLengthsCountCharacters(t *testing.T) {
	t.Parallel()

	var b builder

	open := b.open(thread.KindRequest)
	open.Title = strings.Repeat("界", thread.MaxTitle)
	open.Body = strings.Repeat("界", thread.MaxBody)

	if err := open.Validate(); err != nil {
		t.Errorf("multibyte title and body at the limits: %v", err)
	}

	open.Title += "界"
	if err := open.Validate(); err == nil {
		t.Error("title one character over the limit passed")
	}
}

func TestAllowed(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		state     thread.State
		sender    []thread.Action
		recipient []thread.Action
	}{
		{
			thread.StateOpen,
			[]thread.Action{thread.ActionComment, thread.ActionWithdraw},
			[]thread.Action{thread.ActionComment, thread.ActionAck, thread.ActionNeedsInput, thread.ActionResolve, thread.ActionDecline},
		},
		{
			thread.StateAcked,
			[]thread.Action{thread.ActionComment, thread.ActionWithdraw},
			[]thread.Action{thread.ActionComment, thread.ActionNeedsInput, thread.ActionResolve, thread.ActionDecline},
		},
		{
			thread.StateNeedsInput,
			[]thread.Action{thread.ActionComment, thread.ActionWithdraw},
			[]thread.Action{thread.ActionComment, thread.ActionResolve, thread.ActionDecline},
		},
		{
			thread.StateResolved,
			[]thread.Action{thread.ActionComment, thread.ActionClose, thread.ActionReopen},
			[]thread.Action{thread.ActionComment},
		},
		{
			thread.StateClosed,
			[]thread.Action{thread.ActionComment, thread.ActionReopen},
			[]thread.Action{thread.ActionComment},
		},
		{thread.StateDeclined, []thread.Action{thread.ActionComment}, []thread.Action{thread.ActionComment}},
		{thread.StateWithdrawn, []thread.Action{thread.ActionComment}, []thread.Action{thread.ActionComment}},
	} {
		th := thread.Thread{Sender: alice, Recipient: bob, State: tc.state}
		if got := th.Allowed(alice); !slices.Equal(got, tc.sender) {
			t.Errorf("%s: sender may %v, want %v", tc.state, got, tc.sender)
		}

		if got := th.Allowed(bob); !slices.Equal(got, tc.recipient) {
			t.Errorf("%s: recipient may %v, want %v", tc.state, got, tc.recipient)
		}

		if got := th.Allowed("mallory"); got != nil {
			t.Errorf("%s: outsider may %v", tc.state, got)
		}
	}
}

func TestTurn(t *testing.T) {
	t.Parallel()

	want := map[thread.State]string{
		thread.StateOpen: bob, thread.StateAcked: bob,
		thread.StateNeedsInput: alice, thread.StateResolved: alice,
		thread.StateClosed: "", thread.StateDeclined: "", thread.StateWithdrawn: "",
	}

	for _, s := range thread.States() {
		th := thread.Thread{Sender: alice, Recipient: bob, State: s}
		if got := th.Turn(); got != want[s] {
			t.Errorf("Turn in %s = %q, want %q", s, got, want[s])
		}
	}
}

// Every rule's target is a state, and every non-open, non-comment action has
// a rule.
func TestRulesCoverActions(t *testing.T) {
	t.Parallel()

	rules := thread.Rules()
	for _, a := range thread.Actions() {
		r, ok := rules[a]
		if a == thread.ActionOpen || a == thread.ActionComment {
			if ok {
				t.Errorf("%s has a rule", a)
			}

			continue
		}

		if !ok || !slices.Contains(thread.States(), r.To) || len(r.From) == 0 {
			t.Errorf("%s: rule %+v", a, r)
		}
	}
}

func TestEventRoundTrip(t *testing.T) {
	t.Parallel()

	var b builder

	open := b.open(thread.KindRequest)
	open.Labels = []string{"infra", "repo/x"}

	payload, err := open.Marshal()
	if err != nil {
		t.Fatal(err)
	}

	got, err := thread.Parse(payload)
	if err != nil {
		t.Fatal(err)
	}

	if !got.At.Equal(open.At) || got.Title != open.Title || !slices.Equal(got.Labels, open.Labels) {
		t.Errorf("round trip = %+v", got)
	}
}

func TestParseRejects(t *testing.T) {
	t.Parallel()

	var b builder

	valid, err := b.open(thread.KindRequest).Marshal()
	if err != nil {
		t.Fatal(err)
	}

	for name, payload := range map[string]string{
		"unknown field": strings.Replace(string(valid), `{"id"`, `{"extra":true,"id"`, 1),
		"trailing data": string(valid) + "{}",
		"open clock":    strings.Replace(string(valid), `"clock":1`, `"clock":2`, 1),
		"local time":    strings.Replace(string(valid), `00:00Z"`, `00:00+10:00"`, 1),
		"upper user":    strings.Replace(string(valid), `"from":"alice"`, `"from":"Alice"`, 1),
		"self":          strings.Replace(string(valid), `"to":"bob"`, `"to":"alice"`, 1),
		"bad action":    strings.Replace(string(valid), `"open"`, `"merge"`, 1),
		"bad kind":      strings.Replace(string(valid), `"request"`, `"task"`, 1),
		"title on ack":  strings.Replace(string(valid), `"open"`, `"ack"`, 1),
		"blank title":   strings.Replace(string(valid), `"Review the plan"`, `" "`, 1),
		"open thread":   strings.Replace(string(valid), `"thread":"`+threadID, `"thread":"00000000-0000-4000-8000-000000000001`, 1),
	} {
		if _, err = thread.Parse([]byte(payload)); err == nil {
			t.Errorf("%s: Parse(%s) succeeded", name, payload)
		}
	}
}

func TestNormalizeName(t *testing.T) {
	t.Parallel()

	for in, want := range map[string]bool{
		"Savid": true, "a-b": true, "-a": false, "a--b": false, "a-": false, "": false,
		strings.Repeat("a", 39): true, strings.Repeat("a", 40): false, "a_b": false,
		"a" + strings.Repeat("-b", 19): true, "a" + strings.Repeat("-b", 20): false,
	} {
		if _, ok := thread.NormalizeName(in); ok != want {
			t.Errorf("NormalizeName(%q) ok = %v, want %v", in, ok, want)
		}
	}
}
