package server

import (
	"net/http"
	"time"

	"github.com/savid/clanker-proxy/internal/httpserve"
)

const (
	// streamKeepalive is shorter than common proxy idle timeouts.
	streamKeepalive = 15 * time.Second
	// streamRetry is the reconnect delay sent to clients.
	streamRetry = 2 * time.Second
)

// streamEvents serves the streamEvents operation: a `thread` event with the thread's
// summary each time a thread changes. It is routed by hand, so it checks the
// owner or agent token itself, as the spec's security for it requires.
func (s *Server) streamEvents(w http.ResponseWriter, r *http.Request) {
	ctx := r.Context()

	admitted, err := s.sec.ownerOrAgent(ctx, bearer(r))
	if err != nil {
		s.log.ErrorContext(ctx, "stream security failed", "error", err)
		httpserve.WriteProblem(w, http.StatusInternalServerError, "")

		return
	}

	if !admitted {
		httpserve.WriteProblem(w, http.StatusUnauthorized, errUnauthorized.Error())

		return
	}

	changes, unsubscribe := s.ops.inbox.Subscribe()
	defer unsubscribe()

	sse, err := httpserve.StartSSE(w, streamRetry)
	if err != nil {
		return
	}

	keepalive := time.NewTicker(streamKeepalive)
	defer keepalive.Stop()

	self := s.ops.inbox.Self()

	for ok := true; ok; {
		select {
		case <-ctx.Done():
			return
		case <-s.shutdown:
			return
		case <-keepalive.C:
			ok = sse.Ping() == nil
		case sum := <-changes:
			out := summary(sum, self)

			data, encodeErr := out.MarshalJSON()
			if encodeErr != nil {
				s.log.ErrorContext(ctx, "encode thread", "error", encodeErr)

				return
			}

			ok = sse.Event("thread", data) == nil
		}
	}
}
