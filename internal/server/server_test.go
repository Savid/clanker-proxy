package server_test

import (
	"encoding/json"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/savid/clanker-proxy/api"
	"github.com/savid/clanker-proxy/internal/inbox"
	"github.com/savid/clanker-proxy/internal/server"
	"github.com/savid/clanker-proxy/internal/testutil/apicontract"
	"github.com/savid/clanker-proxy/internal/testutil/inboxtest"
)

type fixture struct {
	h          http.Handler
	owner      string
	agent      string
	peerSecret string
}

func newFixture(t *testing.T) fixture {
	t.Helper()

	ib := inboxtest.New(t)
	f := fixture{owner: inbox.NewSecret(inbox.OwnerPrefix), peerSecret: ib.ActivePeer(t, "friend")}

	agent, _, err := ib.CreateAgentToken(t.Context(), "helper", time.Time{})
	if err != nil {
		t.Fatal(err)
	}

	f.agent = agent

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
// declares. It refuses a valid agent token with 403, and the rest with 401
// and a WWW-Authenticate challenge. The spec is the access policy; this
// holds the server to it.
func TestAccessLevels(t *testing.T) {
	t.Parallel()

	f := newFixture(t)

	callers := map[string]string{
		"nobody":     "",
		"ownerToken": f.owner,
		"agentToken": f.agent,
		"peerSecret": f.peerSecret,
		"garbage":    "cpo_" + strings.Repeat("x", 43),
		"badAgent":   "cpa_" + strings.Repeat("x", 43),
	}

	for _, op := range apicontract.Operations(t, api.Spec) {
		for caller, token := range callers {
			rec := apicontract.Do(t, f.h, op, token)

			admitted := len(op.Schemes) == 0
			for _, s := range op.Schemes {
				admitted = admitted || s == caller
			}

			refusal := http.StatusUnauthorized
			if caller == "agentToken" && len(op.Schemes) > 0 && slices.Contains(op.Schemes, "ownerToken") {
				refusal = http.StatusForbidden
			}

			refused := rec.Code == refusal

			switch {
			case admitted && (refused || rec.Code == http.StatusUnauthorized):
				t.Errorf("%s: %s was refused: %s", op.ID, caller, rec.Body.String())
			case !admitted && !refused:
				t.Errorf("%s: %s got %d, want %d: %s", op.ID, caller, rec.Code, refusal, rec.Body.String())
			case rec.Code == http.StatusUnauthorized && rec.Header().Get("WWW-Authenticate") == "":
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

	// A refused field is named plainly, without the decoder's call chain.
	if rec := post(`{"name":"Not A Name","url":"http://x.test","secret":"cpp_x"}`); rec.Code != http.StatusBadRequest ||
		!strings.Contains(rec.Body.String(), `"detail":"invalid name: `) {
		t.Errorf("bad name: %d %s", rec.Code, rec.Body.String())
	}

	if rec := post(`not json`); rec.Code != http.StatusBadRequest ||
		!strings.Contains(rec.Body.String(), `"detail":"invalid request body: `) {
		t.Errorf("bad JSON: %d %s", rec.Code, rec.Body.String())
	}

	if rec := post(`{"name":"x","url":"http://x.test/` + strings.Repeat("a", 600) + `","secret":"` + inbox.NewSecret(inbox.PeerPrefix) + `"}`); rec.Code != http.StatusBadRequest {
		t.Errorf("long url: %d %s", rec.Code, rec.Body.String())
	}

	// Pending requests are capped: past the cap, a new one is taken and the
	// oldest dropped, so a flood cannot shut out later requests.
	for i := range inbox.MaxPendingRequests + 1 {
		rec := post(`{"name":"x` + string(rune('a'+i)) + `","url":"http://x.test","secret":"` + inbox.NewSecret(inbox.PeerPrefix) + `"}`)
		if rec.Code != http.StatusAccepted {
			t.Fatalf("request %d answered %d %s", i, rec.Code, rec.Body.String())
		}
	}

	rec := httptest.NewRecorder()
	req := httptest.NewRequestWithContext(t.Context(), http.MethodGet, "/api/v1/peering-requests", nil)
	req.Header.Set("Authorization", "Bearer "+f.owner)
	f.h.ServeHTTP(rec, req)

	var list struct{ Requests []struct{ Name string } }
	if err := json.Unmarshal(rec.Body.Bytes(), &list); err != nil {
		t.Fatal(err)
	}

	if n := len(list.Requests); n != inbox.MaxPendingRequests || list.Requests[0].Name != "xb" {
		t.Errorf("pending after the cap: %d, oldest %+v; want %d from xb", n, list.Requests[0], inbox.MaxPendingRequests)
	}
}
