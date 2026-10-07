package main

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"syscall"
	"time"

	"github.com/savid/clanker-proxy/api/rest"
)

const reconnectDelay = 2 * time.Second

var errTimeout = errors.New("timed out waiting")

// stream is one connection to the event stream.
type stream struct {
	body io.ReadCloser
	sc   *bufio.Scanner
}

// openStream connects to the stream. Once it returns, every later change is
// delivered on it.
func (a *app) openStream(ctx context.Context) (*stream, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, strings.TrimRight(a.url, "/")+"/api/v1/stream", nil)
	if err != nil {
		return nil, fmt.Errorf("stream: %w", err)
	}

	req.Header.Set("Accept", "text/event-stream")
	req.Header.Set("Authorization", "Bearer "+a.token)

	resp, err := ownerHTTPClient(0).Do(req)
	if err != nil {
		return nil, fmt.Errorf("stream: %w", err)
	}

	if resp.StatusCode != http.StatusOK {
		defer resp.Body.Close()

		// A refusal is final: wait and watch stop rather than retry it.
		if resp.StatusCode < http.StatusInternalServerError {
			p := &rest.ProblemStatusCode{StatusCode: resp.StatusCode}

			body, _ := io.ReadAll(io.LimitReader(resp.Body, 64<<10))
			if err = p.Response.UnmarshalJSON(body); err != nil {
				p.Response = rest.Problem{Title: resp.Status, Status: int32(resp.StatusCode)} //nolint:gosec // HTTP statuses fit
			}

			return nil, p
		}

		return nil, fmt.Errorf("stream: %s", resp.Status)
	}

	sc := bufio.NewScanner(resp.Body)
	sc.Buffer(make([]byte, 0, 64<<10), 1<<20)

	return &stream{body: resp.Body, sc: sc}, nil
}

// next returns the next thread change.
func (s *stream) next() (rest.ThreadSummary, error) {
	var name, data string

	for s.sc.Scan() {
		line := s.sc.Text()

		switch {
		case line == "":
			if name == "thread" && data != "" {
				var t rest.ThreadSummary
				if err := t.UnmarshalJSON([]byte(data)); err != nil {
					return t, fmt.Errorf("stream: %w", err)
				}

				return t, nil
			}

			name, data = "", ""
		case strings.HasPrefix(line, "event: "):
			name = strings.TrimPrefix(line, "event: ")
		case strings.HasPrefix(line, "data: "):
			data = strings.TrimPrefix(line, "data: ")
		}
	}

	if err := s.sc.Err(); err != nil {
		return rest.ThreadSummary{}, fmt.Errorf("stream: %w", err)
	}

	return rest.ThreadSummary{}, errors.New("stream: closed by cpd")
}

func (s *stream) close() { _ = s.body.Close() }

func runWatch(a *app, _ []string) error {
	show := func(t rest.ThreadSummary) bool {
		_ = a.print(&t, func(w io.Writer) {
			fmt.Fprintf(w, "%s  %s  %s %s  %s  turn: %s  %s\n",
				a.now().UTC().Format(time.TimeOnly), t.ID[:shortID], direction(t), t.Peer, t.State, turn(t), title(t))
		})

		return false
	}

	for a.ctx.Err() == nil {
		_, err := a.follow(a.ctx, show)
		if isStreamRefusal(err) {
			return err
		}

		if a.ctx.Err() == nil {
			fmt.Fprintf(a.stderr, "cpctl: %v; reconnecting\n", err)
		}

		select {
		case <-a.ctx.Done():
		case <-time.After(reconnectDelay):
		}
	}

	return nil
}

// follow connects to the stream and passes each change to done until it
// returns true, which follow returns; or until the stream fails.
func (a *app) follow(ctx context.Context, done func(rest.ThreadSummary) bool) (rest.ThreadSummary, error) {
	s, err := a.openStream(ctx)
	if err != nil {
		return rest.ThreadSummary{}, err
	}
	defer s.close()

	return s.until(done)
}

// until reads changes until done returns true for one.
func (s *stream) until(done func(rest.ThreadSummary) bool) (rest.ThreadSummary, error) {
	for {
		t, err := s.next()
		if err != nil || done(t) {
			return t, err
		}
	}
}

// ready reports whether a wait is over for t: it is the owner's turn, or the
// thread has ended.
func ready(t rest.ThreadSummary) bool {
	return t.MyTurn || !t.Turn.Set
}

func runWait(a *app, pos []string, timeout time.Duration) error {
	ctx := a.ctx
	if timeout > 0 {
		var cancel context.CancelFunc

		ctx, cancel = context.WithTimeout(ctx, timeout)
		defer cancel()
	}

	ref := ""
	if len(pos) == 1 {
		ref = pos[0]
	}

	t, err := a.wait(ctx, ref)
	if err != nil {
		return err
	}

	return a.print(&t, func(w io.Writer) {
		if t.MyTurn {
			fmt.Fprintln(w, "your turn on:")
		} else {
			fmt.Fprintln(w, "ended:")
		}

		printSummary(w, t)
		next(w, append([]step{{"cpctl show " + string(t.ID)[:shortID], "read what changed"}}, threadSteps(t)...)...)
	})
}

// wait retries waitOnce across dropped connections until it succeeds, the
// daemon refuses, or ctx ends. A daemon that was never reached is reported,
// not retried: waiting on it would only hide that it is down.
func (a *app) wait(ctx context.Context, ref string) (rest.ThreadSummary, error) {
	reached := false

	for {
		t, opened, err := a.waitOnce(ctx, ref)
		reached = reached || opened

		switch {
		case err == nil:
			return t, nil
		case errors.Is(ctx.Err(), context.DeadlineExceeded):
			return t, errTimeout
		case ctx.Err() != nil:
			return t, ctx.Err()
		case isStreamRefusal(err), !reached && isUnreachable(err):
			return t, err
		}

		select {
		case <-ctx.Done():
		case <-time.After(reconnectDelay):
		}
	}
}

// waitOnce subscribes, then checks the current state, so no change between
// the two is missed; then follows the stream until the wait is over. It
// reports whether the stream opened.
func (a *app) waitOnce(ctx context.Context, ref string) (rest.ThreadSummary, bool, error) {
	s, err := a.openStream(ctx)
	if err != nil {
		return rest.ThreadSummary{}, false, err
	}
	defer s.close()

	if ref == "" {
		t, anyErr := a.waitAny(ctx, s)

		return t, true, anyErr
	}

	th, err := a.client.GetThread(ctx, rest.GetThreadParams{Ref: ref})
	if err != nil {
		return rest.ThreadSummary{}, true, err
	}

	if sum := summaryOf(th); ready(sum) {
		return sum, true, nil
	}

	t, err := s.until(func(t rest.ThreadSummary) bool { return t.ID == th.ID && ready(t) })

	return t, true, err
}

// waitAny returns the newest unanswered thread, waiting for one if there is
// none. A thread the owner has acked or replied to does not count until
// the peer moves again, so a wait right after acting does not end at once.
func (a *app) waitAny(ctx context.Context, s *stream) (rest.ThreadSummary, error) {
	l, err := a.client.ListThreads(ctx, rest.ListThreadsParams{
		Turn: rest.NewOptListThreadsTurn(rest.ListThreadsTurnMine), Limit: rest.NewOptInt32(500),
	})
	if err != nil {
		return rest.ThreadSummary{}, err
	}

	for _, t := range l.Threads {
		if unanswered(t) {
			return t, nil
		}
	}

	return s.until(unanswered)
}

func isUnreachable(err error) bool {
	return errors.Is(err, syscall.ECONNREFUSED) || isDialError(err)
}

func isStreamRefusal(err error) bool {
	_, ok := errors.AsType[*rest.ProblemStatusCode](err)

	return ok || errors.Is(err, errRedirect)
}
