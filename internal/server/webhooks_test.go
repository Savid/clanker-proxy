package server_test

import (
	"encoding/base64"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"slices"
	"strings"
	"testing"

	"github.com/savid/clanker-proxy/api/rest"
	"github.com/savid/clanker-proxy/pkg/webhook"
)

// The catalog is listed in the spec twice and in pkg/webhook once; WebhookEvents'
// maxItems must also grow with it.
func TestWebhookCatalogMatchesSpec(t *testing.T) {
	t.Parallel()
	var types, items []string
	for _, v := range rest.WebhookEventType("").AllValues() {
		types = append(types, string(v))
	}
	for _, v := range rest.WebhookEventsItem("").AllValues() {
		items = append(items, string(v))
	}
	if !slices.Equal(types, webhook.Types()) || !slices.Equal(items, append([]string{"*"}, webhook.Types()...)) {
		t.Fatalf("spec %v / %v, pkg/webhook %v", types, items, webhook.Types())
	}
	events := make(rest.WebhookEvents, 0, len(items)-1)
	for _, v := range items[1:] {
		events = append(events, rest.WebhookEventsItem(v))
	}
	if err := events.Validate(); err != nil {
		t.Fatalf("the spec refuses a subscription to every type: %v", err)
	}
	var providers []string
	for _, p := range webhook.Providers() {
		providers = append(providers, p.Type)
	}
	var wireProviders []string
	for _, p := range rest.WebhookType("").AllValues() {
		wireProviders = append(wireProviders, string(p))
	}
	if !slices.Equal(providers, wireProviders) {
		t.Fatalf("provider catalog differs: %v / %v", providers, wireProviders)
	}
}

func TestWebhookPrivateDestinationAndUpdates(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	call := func(method, path, body string) *httptest.ResponseRecorder {
		req := httptest.NewRequestWithContext(t.Context(), method, path, strings.NewReader(body))
		req.Header.Set("Content-Type", "application/json")
		req.Header.Set("Authorization", "Bearer "+f.owner)
		rec := httptest.NewRecorder()
		f.h.ServeHTTP(rec, req)
		for _, secret := range []string{"private-path", "private-query", "private-header"} {
			if strings.Contains(rec.Body.String(), secret) {
				t.Fatal("response exposes a destination credential")
			}
		}
		return rec
	}
	created := call(http.MethodPost, "/api/v1/webhooks", `{"name":"discord","type":"discord","url":"https://discord.com/api/webhooks/private-path?token=private-query","events":["*"],"origin":"both","enabled":true,"headers":[{"name":"Authorization","value":"Bearer private-header"}]}`)
	if created.Code != http.StatusCreated {
		t.Fatalf("create: %d %s", created.Code, created.Body.String())
	}
	patched := call(http.MethodPatch, "/api/v1/webhooks/discord", `{"enabled":false}`)
	if patched.Code != http.StatusOK {
		t.Fatalf("patch: %d %s", patched.Code, patched.Body.String())
	}
	var h rest.Webhook
	if err := json.Unmarshal(patched.Body.Bytes(), &h); err != nil {
		t.Fatal(err)
	}
	if h.Type != "discord" || h.Destination != "https://discord.com" || h.Signing || h.Enabled || h.Origin != "both" || len(h.HeaderNames) != 1 {
		t.Fatal("partial update changed omitted fields")
	}
	if rec := call(http.MethodPatch, "/api/v1/webhooks/discord", `{"type":"generic"}`); rec.Code != http.StatusBadRequest {
		t.Fatal("attempted destination type change was not refused")
	}
	for _, path := range []string{"/api/v1/webhooks", "/api/v1/webhooks/discord"} {
		if rec := call(http.MethodGet, path, ""); rec.Code != http.StatusOK {
			t.Fatal("read failed")
		}
	}
	patched = call(http.MethodPatch, "/api/v1/webhooks/discord", `{"headers":[]}`)
	if patched.Code != http.StatusOK {
		t.Fatal("could not clear headers")
	}
	h = rest.Webhook{}
	if err := json.Unmarshal(patched.Body.Bytes(), &h); err != nil || len(h.HeaderNames) != 0 {
		t.Fatal("headers were not cleared")
	}
}

func TestWebhookCredentialValidation(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	for _, headers := range []string{
		`[{"name":"Authorization","value":"private-header\r\nInjected: yes"}]`,
		`[{"name":"Authorization","value":"private-header"},{"name":"authorization","value":"duplicate"}]`,
		`[{"name":"Webhook-Signature","value":"private-header"}]`,
		`[{"name":"Content-Type","value":"private-header"}]`,
	} {
		body := `{"name":"bad","type":"generic","url":"https://example.com/private-path","events":["*"],"origin":"incoming","enabled":true,"headers":` + headers + `}`
		req := httptest.NewRequestWithContext(t.Context(), http.MethodPost, "/api/v1/webhooks", strings.NewReader(body))
		req.Header.Set("Content-Type", "application/json")
		req.Header.Set("Authorization", "Bearer "+f.owner)
		rec := httptest.NewRecorder()
		f.h.ServeHTTP(rec, req)
		if rec.Code != http.StatusBadRequest || strings.Contains(rec.Body.String(), "private-") {
			t.Fatal("invalid credential accepted or exposed")
		}
	}
}

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
	body := map[string]any{"name": "agent", "type": "generic", "url": "https://runner.example/hooks", "secret": key, "events": []string{"*"}, "origin": "incoming", "enabled": true}
	for _, url := range []string{"http://example.com", "https://user:private@runner.example", "https://runner.example/" + strings.Repeat("x", 2048)} {
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

func TestWebhookInvalidUpdateChangesNothing(t *testing.T) {
	t.Parallel()
	f := newFixture(t)
	call := func(method, path, body string) *httptest.ResponseRecorder {
		req := httptest.NewRequestWithContext(t.Context(), method, path, strings.NewReader(body))
		req.Header.Set("Content-Type", "application/json")
		req.Header.Set("Authorization", "Bearer "+f.owner)
		rec := httptest.NewRecorder()
		f.h.ServeHTTP(rec, req)
		return rec
	}
	if rec := call(http.MethodPost, "/api/v1/webhooks", `{"name":"chat","type":"discord","url":"https://discord.com/api/webhooks/id/token","events":["*"],"origin":"both","enabled":true}`); rec.Code != http.StatusCreated {
		t.Fatalf("create: %d %s", rec.Code, rec.Body.String())
	}
	key := base64.StdEncoding.EncodeToString(make([]byte, 32))
	// Each passes the schema and fails validation of the merged settings.
	for body, detail := range map[string]string{
		`{"secret":"` + key + `","enabled":false}`:       "signing keys apply only to generic webhooks",
		`{"url":"http://example.com/x","enabled":false}`: "webhook URL requires HTTPS",
	} {
		rec := call(http.MethodPatch, "/api/v1/webhooks/chat", body)
		var p rest.Problem
		if err := json.Unmarshal(rec.Body.Bytes(), &p); err != nil || rec.Code != http.StatusBadRequest || !strings.HasPrefix(p.Detail.Or(""), detail) {
			t.Fatalf("%s: %d %s", body, rec.Code, rec.Body.String())
		}
	}
	var h rest.Webhook
	if err := json.Unmarshal(call(http.MethodGet, "/api/v1/webhooks/chat", "").Body.Bytes(), &h); err != nil || !h.Enabled || h.Destination != "https://discord.com" || h.Signing {
		t.Fatalf("refused update changed the webhook: %+v %v", h, err)
	}
}
