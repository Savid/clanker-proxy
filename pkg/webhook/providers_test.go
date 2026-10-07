package webhook_test

import (
	"encoding/base64"
	"encoding/json"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/savid/clanker-proxy/pkg/thread"
	"github.com/savid/clanker-proxy/pkg/webhook"
)

func TestDestinationValidation(t *testing.T) {
	t.Parallel()
	key := base64.StdEncoding.EncodeToString(make([]byte, 32))
	for _, p := range webhook.Providers() {
		t.Run(p.Type, func(t *testing.T) {
			t.Parallel()
			c := webhook.Config{Name: "notify", Type: p.Type, URL: "https://receiver.example/path-token?token=query-token", Origin: "both", Events: []string{"*"}}
			if err := c.Validate(); err != nil {
				t.Fatal(err)
			}
			if c.Destination() != "https://receiver.example" {
				t.Fatal("destination exposes a path or query")
			}
			c.Secret = key
			if err := c.Validate(); (err == nil) != (p.Type == "generic") {
				t.Fatalf("signing validation for %s: %v", p.Type, err)
			}
		})
	}
}

func TestHeaderValidation(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name    string
		headers []webhook.Header
		valid   bool
	}{
		{"authentication", []webhook.Header{{Name: "Authorization", Value: "Bearer private"}, {Name: "X-Gotify-Key", Value: "private"}}, true},
		{"duplicates", []webhook.Header{{Name: "Authorization", Value: "a"}, {Name: "authorization", Value: "b"}}, false},
		{"line break", []webhook.Header{{Name: "Authorization", Value: "secret\r\nInjected: yes"}}, false},
		{"control", []webhook.Header{{Name: "Authorization", Value: "secret\x00"}}, false},
		{"bad name", []webhook.Header{{Name: "Bad:Name", Value: "private"}}, false},
		{"long name", []webhook.Header{{Name: strings.Repeat("x", 65), Value: "private"}}, false},
		{"long value", []webhook.Header{{Name: "Authorization", Value: strings.Repeat("x", 4097)}}, false},
		{"empty value", []webhook.Header{{Name: "Authorization", Value: ""}}, false},
	}
	for _, name := range []string{"Host", "Content-Length", "Content-Type", "User-Agent", "Connection", "Transfer-Encoding", "Webhook-Signature", "Webhook-Id", "Proxy-Authorization"} {
		cases = append(cases, struct {
			name    string
			headers []webhook.Header
			valid   bool
		}{name, []webhook.Header{{Name: name, Value: "private"}}, false})
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			c := webhook.Config{Name: "notify", Type: "generic", URL: "https://receiver.example", Origin: "both", Events: []string{"*"}, Headers: tc.headers}
			err := c.Validate()
			if (err == nil) != tc.valid {
				t.Fatalf("valid=%t error=%v", tc.valid, err)
			}
			if err != nil && strings.Contains(err.Error(), "private") {
				t.Fatal("header value exposed")
			}
		})
	}
}

func TestProviderPayloads(t *testing.T) {
	t.Parallel()
	const raw = `{"id":"delivery-id","type":"thread.open","origin":"incoming","at":"2026-10-07T10:00:00Z","subject":"thread-id","peer":"bob","state":"open","myTurn":true,"body":"private-message","title":"private-title"}`
	for _, provider := range webhook.Providers() {
		t.Run(provider.Type, func(t *testing.T) {
			t.Parallel()
			m, err := webhook.Render(provider.Type, []byte(raw))
			if err != nil {
				t.Fatal(err)
			}
			assertRenderedMessage(t, provider.Type, raw, m)
		})
	}
}

func assertRenderedMessage(t *testing.T, kind, raw string, m webhook.Message) {
	t.Helper()
	if kind == "generic" {
		if string(m.Body) != raw {
			t.Fatal("generic payload was re-encoded before signing")
		}
		return
	}
	if strings.Contains(string(m.Body), "private-") || !strings.Contains(string(m.Body), "cpctl show thread-id") || !strings.Contains(string(m.Body), "your turn") {
		t.Fatalf("incorrect metadata summary: %s", m.Body)
	}
	if kind == "ntfy" {
		if m.ContentType != "text/plain; charset=utf-8" || len(m.Headers) != 1 || m.Headers[0].Name != "Title" {
			t.Fatal("ntfy must publish text to the topic URL")
		}
		return
	}
	var body map[string]any
	if err := json.Unmarshal(m.Body, &body); err != nil || m.ContentType != "application/json" {
		t.Fatal("invalid provider JSON")
	}
	assertProviderShape(t, kind, body)
}

func assertProviderShape(t *testing.T, kind string, body map[string]any) {
	t.Helper()
	switch kind {
	case "discord":
		if !reflect.DeepEqual(body["allowed_mentions"], map[string]any{"parse": []any{}}) {
			t.Fatal("Discord mentions are not disabled")
		}
		if embeds, ok := body["embeds"].([]any); !ok || len(embeds) != 1 {
			t.Fatal("missing Discord embed")
		}
	case "slack":
		if body["text"] == nil || body["blocks"] == nil || body["unfurl_links"] != false {
			t.Fatal("missing Slack fallback or plain-text block")
		}
	case "teams":
		if body["type"] != "message" || body["attachments"] == nil {
			t.Fatal("missing Teams Adaptive Card envelope")
		}
	case "gotify":
		if body["message"] == nil || body["title"] == nil || body["priority"] != float64(5) {
			t.Fatal("missing Gotify message")
		}
	case "apprise":
		if body["body"] == nil || body["title"] == nil || body["format"] != "text" {
			t.Fatal("missing Apprise notification")
		}
	default:
		if body["text"] == nil {
			t.Fatal("missing chat text")
		}
	}
}

func TestPeeringSummaryAndMentions(t *testing.T) {
	t.Parallel()
	p := webhook.Payload{ID: "delivery-id", Type: "peering.requested", Origin: "incoming", At: time.Now(), Subject: "12345678", Peer: "@everyone <@123>"}
	raw, err := json.Marshal(p)
	if err != nil {
		t.Fatal(err)
	}
	for _, kind := range []string{"discord", "slack", "teams", "google-chat", "mattermost", "rocketchat"} {
		m, renderErr := webhook.Render(kind, raw)
		if renderErr != nil {
			t.Fatal(renderErr)
		}
		body := string(m.Body)
		if !strings.Contains(body, "unverified") || !strings.Contains(body, "cpctl requests") || strings.Contains(body, "cpctl show") || strings.Contains(body, "@everyone") || strings.Contains(body, "<@123>") {
			t.Fatalf("unsafe peering summary for %s: %s", kind, body)
		}
	}
}

func TestEveryEventRenders(t *testing.T) {
	t.Parallel()
	for _, event := range webhook.Types() {
		raw, err := json.Marshal(webhook.Payload{ID: "delivery-id", Type: event, Origin: "incoming", At: time.Now(), Subject: "thread-id", Peer: "bob", State: thread.StateOpen})
		if err != nil {
			t.Fatal(err)
		}
		for _, p := range webhook.Providers() {
			if _, err = webhook.Render(p.Type, raw); err != nil {
				t.Fatalf("%s %s: %v", p.Type, event, err)
			}
		}
	}
}
