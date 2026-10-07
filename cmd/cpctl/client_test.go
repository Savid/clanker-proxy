package main

import (
	"bytes"
	"context"
	"io"
	"net/http"
	"strings"
	"testing"
	"time"
)

type roundTripFunc func(*http.Request) (*http.Response, error)

func (f roundTripFunc) RoundTrip(r *http.Request) (*http.Response, error) { return f(r) }

func TestOwnerRequestsRejectRedirects(t *testing.T) {
	// The production clients use the default transport. Replacing it avoids
	// sending test credentials to an external host during a downgrade.
	previous := http.DefaultTransport
	t.Cleanup(func() { http.DefaultTransport = previous })

	for _, target := range []string{
		"http://cp.example.test",
		"https://other.example.test",
		"https://cp.example.test/redirected",
	} {
		for _, command := range []string{"me", "wait", "watch"} {
			t.Run(command+"/"+target, func(t *testing.T) {
				calls, forwarded := 0, false
				http.DefaultTransport = roundTripFunc(func(r *http.Request) (*http.Response, error) {
					calls++
					resp := &http.Response{StatusCode: http.StatusOK, Header: make(http.Header), Request: r}
					if calls == 1 {
						resp.StatusCode = http.StatusTemporaryRedirect
						resp.Header.Set("Location", target+r.URL.Path)
						resp.Body = io.NopCloser(strings.NewReader(""))
					} else {
						forwarded = r.Header.Get("Authorization") != ""
						resp.Header.Set("Content-Type", "application/json")
						resp.Body = io.NopCloser(strings.NewReader(`{"name":"alice","version":"test"}`))
					}

					return resp, nil
				})

				ctx, cancel := context.WithTimeout(t.Context(), time.Second)
				defer cancel()

				var stderr bytes.Buffer

				err := run(ctx, []string{"-url", "https://cp.example.test", "-token", "cpo_" + strings.Repeat("x", 43), command},
					strings.NewReader(""), io.Discard, &stderr)
				if exitOf(err) != exitUsage || !strings.Contains(stderr.String(), "hint: set CP_URL") {
					t.Errorf("redirect was not refused: %v", err)
				}

				if calls != 1 || forwarded {
					t.Errorf("redirect caused %d requests; credential forwarded: %t", calls, forwarded)
				}
			})
		}
	}
}
