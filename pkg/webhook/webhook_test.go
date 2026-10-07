package webhook_test

import (
	"encoding/base64"
	"slices"
	"strings"
	"testing"

	"github.com/savid/clanker-proxy/pkg/thread"
	"github.com/savid/clanker-proxy/pkg/webhook"
)

func TestValidation(t *testing.T) {
	t.Parallel()
	key := base64.StdEncoding.EncodeToString(make([]byte, 32))
	cases := []struct {
		url   string
		valid bool
	}{
		{"https://runner.example/hook", true},
		{"http://127.0.0.1:8123/hook", true},
		{"http://[::1]:8123/hook", true},
		{"http://public.example", false},
		{"http://localhost", false},
		{"https://user:password@runner.example", false},
		{"https://runner.example?project=example", true},
		{"https://runner.example#fragment", false},
		{"file:///etc/passwd", false},
	}
	for _, tc := range cases {
		t.Run(tc.url, func(t *testing.T) {
			t.Parallel()
			c := webhook.Config{Type: "generic", Name: "agent", URL: tc.url, Events: []string{"*"}, Origin: "incoming", Secret: key}
			if err := c.Validate(); (err == nil) != tc.valid {
				t.Fatalf("valid=%t, error=%v", tc.valid, err)
			}
		})
	}
	for _, events := range [][]string{nil, {"*", "thread.open"}, {"thread.open", "thread.open"}, {"made-up"}} {
		c := webhook.Config{Type: "generic", Name: "agent", URL: "https://runner.example", Events: events, Origin: "both", Secret: key}
		if c.Validate() == nil {
			t.Errorf("accepted invalid events %v", events)
		}
	}
	for _, action := range thread.Actions() {
		if !slices.Contains(webhook.Types(), webhook.ThreadType(action)) {
			t.Errorf("missing subscription for %s", action)
		}
	}
}

func TestSignature(t *testing.T) {
	t.Parallel()
	// Published wire content is signed byte-for-byte, not re-encoded JSON.
	key := base64.StdEncoding.EncodeToString([]byte(strings.Repeat("a", 32)))
	got, err := webhook.Signature(key, "event-id", "1700000000", []byte(`{"hello":"world"}`))
	if err != nil {
		t.Fatal(err)
	}
	const want = "v1,sTL4E9ah/5ZFL+CR/fTl252h2LiBHuo+k+zA5Nd1A40="
	if got != want {
		t.Fatalf("signature=%q, want %q", got, want)
	}
}
