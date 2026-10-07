package server_test

import (
	"encoding/base64"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

func TestWebhookInputAndSecretRedaction(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	key := base64.StdEncoding.EncodeToString(make([]byte, 32))
	post := func(body map[string]any) *httptest.ResponseRecorder {
		data, err := json.Marshal(body)
		if err != nil {
			t.Fatal(err)
		}
		req := httptest.NewRequestWithContext(t.Context(), http.MethodPost, "/api/v1/webhooks", strings.NewReader(string(data)))
		req.Header.Set("Content-Type", "application/json")
		req.Header.Set("Authorization", "Bearer "+f.owner)
		rec := httptest.NewRecorder()
		f.h.ServeHTTP(rec, req)
		return rec
	}
	body := map[string]any{"name": "agent", "url": "https://runner.example/hooks", "secret": key, "events": []string{"*"}, "origin": "incoming", "enabled": true}
	for _, url := range []string{"http://example.com", "https://user:private@runner.example", "https://runner.example/" + strings.Repeat("x", 512)} {
		body["url"] = url
		rec := post(body)
		if rec.Code != http.StatusBadRequest {
			t.Errorf("invalid URL status=%d", rec.Code)
		}
		if strings.Contains(rec.Body.String(), key) || strings.Contains(rec.Body.String(), "private") {
			t.Error("credential leaked in error")
		}
	}
	body["url"] = "https://runner.example/hooks"
	rec := post(body)
	if rec.Code != http.StatusCreated {
		t.Fatalf("create status=%d: %s", rec.Code, rec.Body.String())
	}
	if strings.Contains(rec.Body.String(), "secret") || strings.Contains(rec.Body.String(), key) {
		t.Fatal("create returned signing key")
	}
	if rec = post(body); rec.Code != http.StatusConflict {
		t.Fatal("duplicate name accepted")
	}
	body["name"] = "bad-key"
	body["secret"] = "top-secret-invalid-key"
	rec = post(body)
	if rec.Code != http.StatusBadRequest || strings.Contains(rec.Body.String(), "top-secret-invalid-key") {
		t.Fatal("invalid key accepted or exposed")
	}
}
