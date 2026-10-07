package server_test

import (
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/savid/clanker-proxy/api"
	"github.com/savid/clanker-proxy/internal/inbox"
	"github.com/savid/clanker-proxy/internal/server"
	"github.com/savid/clanker-proxy/internal/testutil/apicontract"
	"github.com/savid/clanker-proxy/internal/testutil/inboxtest"
)

type fixture struct {
	h          http.Handler
	owner      string
	peerSecret string
}

func newFixture(t *testing.T) fixture {
	t.Helper()

	ib := inboxtest.New(t)
	f := fixture{owner: inbox.NewSecret(inbox.OwnerPrefix), peerSecret: ib.ActivePeer(t, "friend")}

	s, err := server.New(slog.New(slog.DiscardHandler), ib.Inbox, server.Config{OwnerToken: f.owner, Version: "test"})
	if err != nil {
		t.Fatal(err)
	}

	t.Cleanup(s.Shutdown)
	f.h = s

	return f
}

func TestEveryOperationIsServed(t *testing.T) {
	t.Parallel()

	f := newFixture(t)

	for _, op := range apicontract.Operations(t, api.Spec) {
		token := f.owner
		if len(op.Schemes) == 1 && op.Schemes[0] == "peerSecret" {
			token = f.peerSecret
		}

		if why := apicontract.Routed(op, apicontract.Do(t, f.h, op, token)); why != "" {
			t.Errorf("%s (%s %s): %s", op.ID, op.Method, op.Path, why)
		}
	}
}

// Every operation admits exactly the callers its security in the spec
// declares, and refuses the rest with 401 and a WWW-Authenticate challenge.
// The spec is the access policy; this holds the server to it.
func TestAccessLevels(t *testing.T) {
	t.Parallel()

	f := newFixture(t)

	callers := map[string]string{
		"nobody":     "",
		"ownerToken": f.owner,
		"peerSecret": f.peerSecret,
		"garbage":    "cpo_" + strings.Repeat("x", 43),
	}

	for _, op := range apicontract.Operations(t, api.Spec) {
		for caller, token := range callers {
			rec := apicontract.Do(t, f.h, op, token)

			admitted := len(op.Schemes) == 0
			for _, s := range op.Schemes {
				admitted = admitted || s == caller
			}

			got401 := rec.Code == http.StatusUnauthorized

			switch {
			case admitted && got401:
				t.Errorf("%s: %s was refused: %s", op.ID, caller, rec.Body.String())
			case !admitted && !got401:
				t.Errorf("%s: %s was let in: %d %s", op.ID, caller, rec.Code, rec.Body.String())
			case got401 && rec.Header().Get("WWW-Authenticate") == "":
				t.Errorf("%s: 401 for %s has no WWW-Authenticate", op.ID, caller)
			}
		}
	}
}

func TestRoutes(t *testing.T) {
	t.Parallel()

	f := newFixture(t)

	for _, tc := range []struct {
		method, path string
		status       int
		contentType  string
	}{
		{http.MethodGet, "/openapi.yaml", http.StatusOK, "application/yaml"},
		{http.MethodGet, "/api/v1/health", http.StatusOK, "application/json; charset=utf-8"},
		{http.MethodGet, "/", http.StatusNotFound, "application/problem+json"},
		{http.MethodGet, "/api/v1/missing", http.StatusNotFound, "application/problem+json"},
		{http.MethodPut, "/api/v1/health", http.StatusMethodNotAllowed, "application/problem+json"},
	} {
		rec := httptest.NewRecorder()
		f.h.ServeHTTP(rec, httptest.NewRequestWithContext(t.Context(), tc.method, tc.path, nil))

		if rec.Code != tc.status || rec.Header().Get("Content-Type") != tc.contentType {
			t.Errorf("%s %s = %d %q, want %d %q", tc.method, tc.path, rec.Code, rec.Header().Get("Content-Type"), tc.status, tc.contentType)
		}
	}
}

func TestRequestLimits(t *testing.T) {
	t.Parallel()

	f := newFixture(t)

	post := func(body string) *httptest.ResponseRecorder {
		rec := httptest.NewRecorder()
		req := httptest.NewRequestWithContext(t.Context(), http.MethodPost, "/api/v1/peering-requests", strings.NewReader(body))
		req.Header.Set("Content-Type", "application/json")
		f.h.ServeHTTP(rec, req)

		return rec
	}

	if rec := post(`{"name":"x","url":"http://x.test","secret":"` + strings.Repeat("a", 2<<20) + `"}`); rec.Code != http.StatusRequestEntityTooLarge {
		t.Errorf("oversized body: %d %s", rec.Code, rec.Body.String())
	}

	if rec := post(`{"name":"Not A Name","url":"http://x.test","secret":"cpp_x"}`); rec.Code != http.StatusBadRequest {
		t.Errorf("bad name: %d %s", rec.Code, rec.Body.String())
	}

	if rec := post(`{"name":"x","url":"http://x.test/` + strings.Repeat("a", 600) + `","secret":"` + inbox.NewSecret(inbox.PeerPrefix) + `"}`); rec.Code != http.StatusBadRequest {
		t.Errorf("long url: %d %s", rec.Code, rec.Body.String())
	}

	// Pending requests are capped; past the cap they are refused.
	var last *httptest.ResponseRecorder
	for i := range inbox.MaxPendingRequests + 1 {
		last = post(`{"name":"x` + string(rune('a'+i)) + `","url":"http://x.test","secret":"` + inbox.NewSecret(inbox.PeerPrefix) + `"}`)
	}

	if last.Code != http.StatusTooManyRequests {
		t.Errorf("request past the cap answered %d %s", last.Code, last.Body.String())
	}
}
