package httpserve

import (
	"bufio"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

// An event stream outlives ReadTimeout: net/http clears the read deadline
// once the handler is running.
func TestStreamOutlivesReadTimeout(t *testing.T) {
	t.Parallel()

	const events = 5

	srv := httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		sse, err := StartSSE(w, time.Second)
		if err != nil {
			return
		}

		for range events {
			time.Sleep(100 * time.Millisecond)

			if sse.Event("tick", []byte("{}")) != nil {
				return
			}
		}
	}))
	srv.Config = newServer(srv.Config.Handler)
	srv.Config.ReadTimeout = 150 * time.Millisecond
	srv.Start()
	defer srv.Close()

	req, err := http.NewRequestWithContext(t.Context(), http.MethodGet, srv.URL, nil)
	if err != nil {
		t.Fatal(err)
	}

	resp, err := srv.Client().Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()

	got := 0

	for sc := bufio.NewScanner(resp.Body); sc.Scan(); {
		if strings.HasPrefix(sc.Text(), "event: tick") {
			got++
		}
	}

	if got != events {
		t.Fatalf("got %d events over %s; want %d", got, events*100*time.Millisecond, events)
	}
}

func TestRecoverPanics(t *testing.T) {
	t.Parallel()

	h := Wrap(Logger(discard()), http.HandlerFunc(func(http.ResponseWriter, *http.Request) { panic("boom") }), 1<<10)

	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, httptest.NewRequestWithContext(t.Context(), http.MethodGet, "/", nil))

	if rec.Code != http.StatusInternalServerError || rec.Header().Get("Content-Type") != "application/problem+json" ||
		strings.Contains(rec.Body.String(), "boom") {
		t.Fatalf("panic answered %d %q %s", rec.Code, rec.Header().Get("Content-Type"), rec.Body.String())
	}
}

func discard() *slog.Logger { return slog.New(slog.DiscardHandler) }
