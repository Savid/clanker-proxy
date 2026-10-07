// Package thread defines the events two peers exchange and replays a
// thread's events into its state. It is pure: the same events give the same
// thread on both peers, whatever order they arrived in.
package thread

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"regexp"
	"slices"
	"strings"
	"time"
	"unicode/utf8"
)

// Limits on an event's fields.
const (
	MaxTitle  = 200
	MaxBody   = 64 << 10
	MaxLabels = 10
	MaxLabel  = 40
	MaxClock  = 1_000_000_000
)

// Action is what an event does to its thread.
type Action string

// Actions. Open starts a thread; comment adds to it in any state; the rest
// move it between states (see Rules).
const (
	ActionOpen       Action = "open"
	ActionComment    Action = "comment"
	ActionAck        Action = "ack"
	ActionNeedsInput Action = "needs-input"
	ActionResolve    Action = "resolve"
	ActionClose      Action = "close"
	ActionReopen     Action = "reopen"
	ActionDecline    Action = "decline"
	ActionWithdraw   Action = "withdraw"
)

// Actions lists every action, in the order a thread usually meets them.
func Actions() []Action {
	return []Action{
		ActionOpen, ActionComment, ActionAck, ActionNeedsInput, ActionResolve,
		ActionClose, ActionReopen, ActionDecline, ActionWithdraw,
	}
}

// Kind is what the sender expects back.
type Kind string

// Kinds. A request expects a resolution; an fyi closes when acknowledged.
const (
	KindRequest Kind = "request"
	KindFYI     Kind = "fyi"
)

// Event is one step in a thread. From and To are names as this daemon knows
// them: its owner's and the peer's. On the wire they are left out, since the
// peer's secret says who sent it.
type Event struct {
	ID     string    `json:"id"`
	Thread string    `json:"thread"`
	From   string    `json:"from"`
	To     string    `json:"to"`
	At     time.Time `json:"at"`
	// Clock orders the thread: one more than the highest clock its author
	// had seen in it.
	Clock  int64  `json:"clock"`
	Action Action `json:"action"`
	// Title, Kind and Labels are set on open only.
	Title  string   `json:"title,omitempty"`
	Kind   Kind     `json:"kind,omitempty"`
	Labels []string `json:"labels,omitempty"`
	Body   string   `json:"body,omitempty"`
}

var (
	// Names: lower-case alphanumerics and single inner hyphens, at most 39.
	namePattern  = regexp.MustCompile(`^[a-z0-9](?:[a-z0-9]|-[a-z0-9]){0,38}$`)
	idPattern    = regexp.MustCompile(`^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`)
	labelPattern = regexp.MustCompile(`^[a-z0-9][a-z0-9._/-]*$`)
)

// NormalizeName lower-cases a participant's name and reports whether it is
// valid.
func NormalizeName(s string) (string, bool) {
	s = strings.ToLower(strings.TrimSpace(s))

	return s, namePattern.MatchString(s)
}

// ValidID reports whether s is a lower-case UUID, the form of event and
// thread IDs.
func ValidID(s string) bool {
	return idPattern.MatchString(s)
}

// Marshal encodes the event for storage.
func (e Event) Marshal() ([]byte, error) {
	if err := e.Validate(); err != nil {
		return nil, err
	}

	b, err := json.Marshal(e)
	if err != nil {
		return nil, fmt.Errorf("encode event: %w", err)
	}

	return b, nil
}

// Parse decodes and validates a stored event, rejecting unknown fields.
func Parse(payload []byte) (Event, error) {
	dec := json.NewDecoder(bytes.NewReader(payload))
	dec.DisallowUnknownFields()

	var e Event
	if err := dec.Decode(&e); err != nil {
		return Event{}, fmt.Errorf("decode event: %w", err)
	}

	if dec.More() {
		return Event{}, errors.New("decode event: trailing data")
	}

	if err := e.Validate(); err != nil {
		return Event{}, err
	}

	return e, nil
}

// Validate checks the event on its own, without its thread.
func (e Event) Validate() error {
	var errs []error

	add := func(format string, args ...any) { errs = append(errs, fmt.Errorf(format, args...)) }

	if !ValidID(e.ID) {
		add("id %q: want a lower-case UUID", e.ID)
	}

	if !ValidID(e.Thread) {
		add("thread %q: want a lower-case UUID", e.Thread)
	}

	for field, name := range map[string]string{"from": e.From, "to": e.To} {
		if n, ok := NormalizeName(name); !ok || n != name {
			add("%s %q: want a lower-case name of letters, digits and hyphens", field, name)
		}
	}

	if e.From == e.To {
		add("from and to are both %q", e.From)
	}

	if e.At.IsZero() || e.At.Location() != time.UTC {
		add("at: want a UTC time")
	}

	if (e.Action == ActionOpen) != (e.Clock == 1) || e.Clock < 1 || e.Clock > MaxClock {
		add("clock %d: want 1 for open, 2 to %d for the rest", e.Clock, MaxClock)
	}

	if !slices.Contains(Actions(), e.Action) {
		add("action %q: want one of %v", e.Action, Actions())
	}

	if len(e.Body) > MaxBody || !utf8.ValidString(e.Body) {
		add("body: want UTF-8 of at most %d bytes", MaxBody)
	}

	if e.Action == ActionOpen {
		errs = append(errs, e.validateOpen()...)
	} else if e.Title != "" || e.Kind != "" || len(e.Labels) > 0 {
		add("title, kind and labels are set on open only")
	}

	return errors.Join(errs...)
}

func (e Event) validateOpen() []error {
	var errs []error

	if e.Thread != e.ID {
		errs = append(errs, errors.New("open: thread must equal id"))
	}

	if t := strings.TrimSpace(e.Title); t == "" || t != e.Title || len(e.Title) > MaxTitle || !utf8.ValidString(e.Title) ||
		strings.ContainsAny(e.Title, "\r\n") {
		errs = append(errs, fmt.Errorf("open: title must be one trimmed line of 1 to %d bytes", MaxTitle))
	}

	if e.Kind != KindRequest && e.Kind != KindFYI {
		errs = append(errs, fmt.Errorf("open: kind %q: want %q or %q", e.Kind, KindRequest, KindFYI))
	}

	if len(e.Labels) > MaxLabels {
		errs = append(errs, fmt.Errorf("open: at most %d labels", MaxLabels))
	}

	for _, l := range e.Labels {
		if len(l) > MaxLabel || !labelPattern.MatchString(l) {
			errs = append(errs, fmt.Errorf("open: label %q: want lower-case [a-z0-9._/-], at most %d", l, MaxLabel))
		}
	}

	if !slices.IsSorted(e.Labels) || len(slices.Compact(slices.Clone(e.Labels))) != len(e.Labels) {
		errs = append(errs, errors.New("open: labels must be sorted and unique"))
	}

	return errs
}
