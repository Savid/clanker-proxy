package httpserve

import (
	"bytes"
	"errors"
	"fmt"
	"net/http"
	"time"
)

// sseWriteTimeout bounds each frame write, so a stalled client is dropped
// instead of holding its handler open.
const sseWriteTimeout = 5 * time.Second

// SSE writes text/event-stream frames and flushes each one.
type SSE struct {
	w  http.ResponseWriter
	rc *http.ResponseController
}

// StartSSE commits the response to an event stream and tells the client how
// long to wait before reconnecting.
func StartSSE(w http.ResponseWriter, retry time.Duration) (*SSE, error) {
	h := w.Header()
	h.Set("Content-Type", "text/event-stream")
	h.Set("Cache-Control", "no-cache")
	// Stop reverse proxies such as nginx from buffering the stream.
	h.Set("X-Accel-Buffering", "no")
	w.WriteHeader(http.StatusOK)

	s := &SSE{w: w, rc: http.NewResponseController(w)}

	return s, s.write(fmt.Appendf(nil, "retry: %d\n\n", retry.Milliseconds()))
}

// Event sends one named event. data must be a single line, such as compact JSON.
func (s *SSE) Event(name string, data []byte) error {
	if bytes.ContainsAny(data, "\r\n") {
		return errors.New("event data contains a newline")
	}

	frame := make([]byte, 0, len(name)+len(data)+16)
	frame = append(frame, "event: "...)
	frame = append(frame, name...)
	frame = append(frame, "\ndata: "...)
	frame = append(frame, data...)
	frame = append(frame, "\n\n"...)

	return s.write(frame)
}

// Ping sends a comment, which clients ignore, to keep idle connections open.
func (s *SSE) Ping() error {
	return s.write([]byte(": ping\n\n"))
}

func (s *SSE) write(frame []byte) error {
	if err := s.rc.SetWriteDeadline(time.Now().Add(sseWriteTimeout)); err != nil && !errors.Is(err, http.ErrNotSupported) {
		return fmt.Errorf("set write deadline: %w", err)
	}

	if _, err := s.w.Write(frame); err != nil {
		return fmt.Errorf("write event: %w", err)
	}

	if err := s.rc.Flush(); err != nil {
		return fmt.Errorf("flush event: %w", err)
	}

	// Clear the deadline so it cannot fire while the stream is idle.
	if err := s.rc.SetWriteDeadline(time.Time{}); err != nil && !errors.Is(err, http.ErrNotSupported) {
		return fmt.Errorf("clear write deadline: %w", err)
	}

	return nil
}
