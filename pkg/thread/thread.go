package thread

import (
	"cmp"
	"errors"
	"fmt"
	"slices"
	"time"
)

// State is where a thread is in its workflow.
type State string

// States. Closed, declined and withdrawn are terminal; reopen leaves closed.
const (
	StateOpen       State = "open"
	StateAcked      State = "acked"
	StateNeedsInput State = "needs-input"
	StateResolved   State = "resolved"
	StateClosed     State = "closed"
	StateDeclined   State = "declined"
	StateWithdrawn  State = "withdrawn"
)

// States lists every state.
func States() []State {
	return []State{StateOpen, StateAcked, StateNeedsInput, StateResolved, StateClosed, StateDeclined, StateWithdrawn}
}

// Role is a participant's part in a thread.
type Role string

// Roles. The sender opened the thread; the recipient was asked.
const (
	RoleSender    Role = "sender"
	RoleRecipient Role = "recipient"
)

// Rule is who may take an action and from which states. Comment is allowed
// to either participant in every state, so it has no rule.
type Rule struct {
	By   Role
	From []State
	To   State
}

var active = []State{StateOpen, StateAcked, StateNeedsInput}

// Rules is the workflow: the recipient works the thread, the sender closes or
// pulls it.
func Rules() map[Action]Rule {
	return map[Action]Rule{
		ActionAck:        {By: RoleRecipient, From: []State{StateOpen}, To: StateAcked},
		ActionNeedsInput: {By: RoleRecipient, From: []State{StateOpen, StateAcked}, To: StateNeedsInput},
		ActionResolve:    {By: RoleRecipient, From: active, To: StateResolved},
		ActionDecline:    {By: RoleRecipient, From: active, To: StateDeclined},
		ActionWithdraw:   {By: RoleSender, From: active, To: StateWithdrawn},
		ActionClose:      {By: RoleSender, From: []State{StateResolved}, To: StateClosed},
		ActionReopen:     {By: RoleSender, From: []State{StateResolved, StateClosed}, To: StateAcked},
	}
}

// Thread is the state replayed from a thread's events.
type Thread struct {
	ID        string
	Title     string
	Kind      Kind
	Labels    []string
	Sender    string
	Recipient string
	State     State
	OpenedAt  time.Time
	UpdatedAt time.Time
	// Events in replay order, each with what it did.
	Events []Applied
}

// Applied is an event and its effect. An event that could not apply (the
// other side's action came first, or it was not its author's to take) is
// kept, with Ignored saying why; its body still reads as a comment.
type Applied struct {
	Event
	Ignored string
}

// ErrNoOpen is returned by Replay when no event opens the thread.
var ErrNoOpen = errors.New("thread has no open event")

// Replay orders events and folds them into a thread. The open event comes
// first; the rest follow by clock, then ID, so two peers holding the same
// events reach the same state, and an event always follows those its author
// had seen.
func Replay(events []Event) (Thread, error) {
	sorted := slices.Clone(events)
	slices.SortFunc(sorted, func(a, b Event) int {
		if (a.Action == ActionOpen) != (b.Action == ActionOpen) {
			if a.Action == ActionOpen {
				return -1
			}

			return 1
		}

		return cmp.Or(cmp.Compare(a.Clock, b.Clock), cmp.Compare(a.ID, b.ID))
	})

	if len(sorted) == 0 || sorted[0].Action != ActionOpen {
		return Thread{}, ErrNoOpen
	}

	open := sorted[0]
	t := Thread{
		ID: open.ID, Title: open.Title, Kind: open.Kind, Labels: open.Labels,
		Sender: open.From, Recipient: open.To, State: StateOpen,
		OpenedAt: open.At, UpdatedAt: open.At,
		Events: []Applied{{Event: open}},
	}

	for _, e := range sorted[1:] {
		if e.Thread != t.ID {
			return Thread{}, fmt.Errorf("event %s belongs to thread %s, not %s", e.ID, e.Thread, t.ID)
		}

		t = t.Apply(e)
	}

	return t, nil
}

// Apply returns the thread with e appended. An event that may not apply is
// appended with Ignored set and leaves the state alone.
func (t Thread) Apply(e Event) Thread {
	next := t
	next.Events = append(slices.Clone(t.Events), Applied{Event: e})
	last := &next.Events[len(next.Events)-1]

	if err := t.Check(e); err != nil {
		last.Ignored = err.Error()

		return next
	}

	next.UpdatedAt = later(t.UpdatedAt, e.At)
	next.State = t.after(e)

	return next
}

// NextClock is the clock for a new event: one past every event held.
func (t Thread) NextClock() int64 {
	var c int64
	for _, e := range t.Events {
		c = max(c, e.Clock)
	}

	return c + 1
}

// maxLead is how far past NextClock a peer's event may be. An honest peer's
// clock is one past the events it holds, which can run ahead of this
// daemon's only by the peer's own events that this daemon refused.
const maxLead = 1000

// CheckClock reports why a peer's event e has a clock the thread cannot take,
// or nil. Peers deliver in order, so each author's clocks rise; a lower one
// would replay ahead of what its author already said. A clock far ahead
// would use up the room below MaxClock.
func (t Thread) CheckClock(e Event) error {
	for _, a := range t.Events {
		if a.From == e.From && a.Clock >= e.Clock {
			return fmt.Errorf("clock %d is not after %s's clock %d in this thread", e.Clock, e.From, a.Clock)
		}
	}

	if limit := t.NextClock() + maxLead; e.Clock > limit {
		return fmt.Errorf("clock %d is past %d, this thread's limit", e.Clock, limit)
	}

	return nil
}

// Check reports why e may not apply to the thread as it stands, or nil.
func (t Thread) Check(e Event) error {
	if e.Thread != t.ID {
		return fmt.Errorf("event belongs to thread %s", e.Thread)
	}

	role, ok := t.RoleOf(e.From)
	if !ok || e.To != t.Peer(e.From) {
		return fmt.Errorf("%s to %s is not between this thread's participants", e.From, e.To)
	}

	if e.Action == ActionOpen {
		return errors.New("thread is already open")
	}

	if e.Action == ActionComment {
		return nil
	}

	rule := Rules()[e.Action]
	if rule.By != role {
		return fmt.Errorf("only the %s may %s", rule.By, e.Action)
	}

	if !slices.Contains(rule.From, t.State) {
		return fmt.Errorf("cannot %s a thread that is %s", e.Action, t.State)
	}

	return nil
}

// after is the state once a checked event applies.
func (t Thread) after(e Event) State {
	switch e.Action {
	case ActionComment:
		// The sender answering a question hands the thread back.
		if t.State == StateNeedsInput && e.From == t.Sender {
			return StateAcked
		}

		return t.State
	case ActionAck:
		if t.Kind == KindFYI {
			return StateClosed
		}

		return Rules()[e.Action].To
	case ActionNeedsInput, ActionResolve, ActionClose, ActionReopen, ActionDecline, ActionWithdraw:
		return Rules()[e.Action].To
	case ActionOpen:
		return StateOpen
	default:
		return t.State
	}
}

// Allowed is what user may do to the thread as it stands, in Actions order.
// A participant may always comment.
func (t Thread) Allowed(user string) []Action {
	role, ok := t.RoleOf(user)
	if !ok {
		return nil
	}

	rules := Rules()

	var out []Action

	for _, a := range Actions() {
		r, ruled := rules[a]
		if a == ActionComment || ruled && r.By == role && slices.Contains(r.From, t.State) {
			out = append(out, a)
		}
	}

	return out
}

// RoleOf is user's role in the thread.
func (t Thread) RoleOf(user string) (Role, bool) {
	switch user {
	case t.Sender:
		return RoleSender, true
	case t.Recipient:
		return RoleRecipient, true
	default:
		return "", false
	}
}

// Turn is whose move it is: the recipient while the thread is open or acked,
// the sender while it needs input or is resolved, and nobody once it ends.
func (t Thread) Turn() string {
	switch t.State {
	case StateOpen, StateAcked:
		return t.Recipient
	case StateNeedsInput, StateResolved:
		return t.Sender
	case StateClosed, StateDeclined, StateWithdrawn:
		return ""
	default:
		return ""
	}
}

// Peer is the participant who is not user.
func (t Thread) Peer(user string) string {
	if user == t.Sender {
		return t.Recipient
	}

	return t.Sender
}

func later(a, b time.Time) time.Time {
	if b.After(a) {
		return b
	}

	return a
}
