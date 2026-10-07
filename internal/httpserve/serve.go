// Package httpserve holds the daemon's HTTP plumbing: listening, request
// IDs, the access log, RFC 9457 problems, event streams and graceful
// shutdown.
package httpserve

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"runtime/debug"
	"time"
)

const shutdownTimeout = 5 * time.Second

// Logger returns log with each record carrying its request's ID.
func Logger(log *slog.Logger) *slog.Logger {
	return slog.New(requestIDHandler{log.Handler()})
}

// Wrap adds request IDs, the access log and panic recovery around h, and
// caps request bodies at maxBody bytes.
func Wrap(log *slog.Logger, h http.Handler, maxBody int64) http.Handler {
	return withRequestID(accessLog(log, recoverPanics(log, http.MaxBytesHandler(h, maxBody))))
}

// recoverPanics answers a handler's panic with a 500 problem and logs it, so
// one bad request cannot take the daemon down or leak a stack trace.
func recoverPanics(log *slog.Logger, next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		defer func() {
			v := recover()
			if v == nil {
				return
			}

			if v == http.ErrAbortHandler { //nolint:errorlint // recover returns the value panicked with, not a wrapped error
				panic(v)
			}

			log.ErrorContext(r.Context(), "handler panicked", "panic", v, "stack", string(debug.Stack()))
			WriteProblem(w, http.StatusInternalServerError, "")
		}()

		next.ServeHTTP(w, r)
	})
}

// Listen listens on a TCP address.
func Listen(ctx context.Context, addr string) (net.Listener, error) {
	ln, err := (&net.ListenConfig{}).Listen(ctx, "tcp", addr)
	if err != nil {
		return nil, fmt.Errorf("listen %s: %w", addr, err)
	}

	return ln, nil
}

// Serve serves h on ln until ctx is cancelled, then shuts down gracefully.
// onShutdown runs as shutdown begins, to end long-lived event streams.
func Serve(ctx context.Context, log *slog.Logger, ln net.Listener, h http.Handler, onShutdown func()) error {
	srv := newServer(h)
	if onShutdown != nil {
		srv.RegisterOnShutdown(onShutdown)
	}

	log.InfoContext(ctx, "serving", "addr", ln.Addr().String())

	served := make(chan error, 1)
	go func() { served <- srv.Serve(ln) }()

	select {
	case err := <-served:
		return fmt.Errorf("serve: %w", err)
	case <-ctx.Done():
	}

	shutdownCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), shutdownTimeout)
	defer cancel()

	if err := srv.Shutdown(shutdownCtx); err != nil {
		return fmt.Errorf("shutdown: %w", err)
	}

	if err := <-served; !errors.Is(err, http.ErrServerClosed) {
		return fmt.Errorf("serve: %w", err)
	}

	return nil
}

// newServer is the http.Server's limits around h.
func newServer(h http.Handler) *http.Server {
	return &http.Server{
		Handler:           h,
		ReadHeaderTimeout: 5 * time.Second,
		// ReadTimeout bounds a slowly trickled body. It does not cut event
		// streams: once a handler runs and the body is read, net/http clears
		// the read deadline.
		ReadTimeout:    30 * time.Second,
		IdleTimeout:    2 * time.Minute,
		MaxHeaderBytes: 16 << 10,
		// No WriteTimeout: event streams are long-lived and bound each write
		// themselves.
	}
}

// problem is the RFC 9457 body, for answers written outside the generated
// server.
type problem struct {
	Title  string `json:"title"`
	Status int    `json:"status"`
	Detail string `json:"detail,omitempty"`
}

// WriteProblem answers with an RFC 9457 problem.
func WriteProblem(w http.ResponseWriter, status int, detail string) {
	body, err := json.Marshal(problem{Title: http.StatusText(status), Status: status, Detail: detail})
	if err != nil {
		http.Error(w, http.StatusText(http.StatusInternalServerError), http.StatusInternalServerError)

		return
	}

	w.Header().Set("Content-Type", "application/problem+json")
	w.WriteHeader(status)
	_, _ = w.Write(body)
}
