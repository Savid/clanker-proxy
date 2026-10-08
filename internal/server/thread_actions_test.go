package server_test

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/savid/clanker-proxy/api/rest"
)

func TestReopenRequiresReason(t *testing.T) {
	t.Parallel()

	f := newFixture(t)
	post := func(path, token, body string) *httptest.ResponseRecorder {
		req := httptest.NewRequestWithContext(t.Context(), http.MethodPost, path, strings.NewReader(body))
		req.Header.Set("Authorization", "Bearer "+token)
		req.Header.Set("Content-Type", "application/json")
		rec := httptest.NewRecorder()
		f.h.ServeHTTP(rec, req)

		return rec
	}

	rec := post("/api/v1/threads", f.owner, `{"to":"friend","title":"Review"}`)
	var opened rest.ThreadSummary
	if err := json.Unmarshal(rec.Body.Bytes(), &opened); err != nil || opened.ID == "" {
		t.Fatalf("open: %d %s, %v", rec.Code, rec.Body.String(), err)
	}
	path := "/api/v1/threads/" + string(opened.ID) + "/events"
	if rec = post(path, f.owner, `{"action":"close"}`); rec.Code != http.StatusOK {
		t.Fatalf("close: %d %s", rec.Code, rec.Body.String())
	}

	for _, tc := range []struct{ token, body string }{
		{f.owner, `{"action":"reopen"}`},
		{f.owner, `{"action":"reopen","body":""}`},
		{f.owner, `{"action":"reopen","body":" \t\r\n\u2003"}`},
		{f.agent, `{"action":"reopen"}`},
		{f.agent, `{"action":"reopen","body":""}`},
		{f.agent, `{"action":"reopen","body":" \t\r\n\u2003"}`},
	} {
		rec = post(path, tc.token, tc.body)
		if rec.Code != http.StatusBadRequest || !strings.Contains(rec.Body.String(), `"detail":"reopen requires a reason"`) {
			t.Errorf("reopen %s: %d %s", tc.body, rec.Code, rec.Body.String())
		}
	}

	if rec = post(path, f.agent, `{"action":"reopen","body":"Still failing"}`); rec.Code != http.StatusOK ||
		!strings.Contains(rec.Body.String(), `"state":"acked"`) {
		t.Fatalf("reopen with reason: %d %s", rec.Code, rec.Body.String())
	}
}
