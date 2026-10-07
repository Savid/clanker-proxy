package main

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/savid/clanker-proxy/api/rest"
)

func TestWebhookCLI(t *testing.T) {
	t.Parallel()
	p := newPerson(t, "alice")
	key := base64.StdEncoding.EncodeToString(make([]byte, 32))
	file := filepath.Join(t.TempDir(), "hook.key")
	if err := os.WriteFile(file, []byte(key+"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	output := p.cp(t, "webhook", "add", "agent", "https://runner.example/hook", "-secret-file", file, "-events", "thread.open,thread.reply")
	if strings.Contains(output, key) || !strings.Contains(output, "next:") {
		t.Fatal("unsafe or incomplete output")
	}
	output, duplicateErr := p.try(t, "webhook", "add", "agent", "https://runner.example/hook", "-secret-file", file)
	if exitOf(duplicateErr) != exitRefused || !strings.Contains(output, "hint: choose another name") {
		t.Fatalf("conflict guidance: %v: %s", duplicateErr, output)
	}
	var h rest.Webhook
	p.json(t, &h, "webhook", "show", "agent")
	if string(h.Origin) != "incoming" || !h.Enabled || len(h.Events) != 2 {
		t.Fatal("wrong initial configuration")
	}
	p.cp(t, "webhook", "set", "agent", "-enabled=false", "-events", "*", "-origin", "both")
	p.json(t, &h, "webhook", "show", "agent")
	if h.Enabled || string(h.Origin) != "both" || len(h.Events) != 1 || h.Events[0] != "*" {
		t.Fatal("settings were not applied")
	}
	p.cp(t, "webhook", "set", "agent", "-enabled=true")
	p.json(t, &h, "webhook", "show", "agent")
	if !h.Enabled || h.Origin != "both" {
		t.Fatal("unspecified settings changed")
	}
	if output = p.cp(t, "-json", "webhook", "ls"); strings.Contains(output, key) || strings.Contains(output, "secret") {
		t.Fatal("key exposed in list")
	}
	if output = p.cp(t, "webhook", "deliveries", "agent"); !strings.Contains(output, "next:") {
		t.Fatal("no delivery guidance")
	}
	for _, events := range []string{"bad-event", "*,thread.open", "thread.open,thread.open"} {
		if _, err := p.try(t, "webhook", "set", "agent", "-events", events); err == nil {
			t.Fatalf("accepted %s", events)
		}
	}
	p.cp(t, "webhook", "rm", "agent")
	output, err := p.try(t, "webhook", "show", "agent")
	if exitOf(err) != exitNotFound || !strings.Contains(output, "hint: cpctl webhook ls") {
		t.Fatalf("deleted webhook guidance: %v: %s", err, output)
	}
}

func TestWebhookProviderCLI(t *testing.T) {
	t.Parallel()
	p := newPerson(t, "alice")
	dir := t.TempDir()
	urlFile := filepath.Join(dir, "discord.url")
	headersFile := filepath.Join(dir, "headers.json")
	if err := os.WriteFile(urlFile, []byte("https://discord.com/api/webhooks/private-path?token=private-query\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(headersFile, []byte(`{"Authorization":"Bearer private-header"}`), 0o600); err != nil {
		t.Fatal(err)
	}
	output := p.cp(t, "webhook", "add", "discord", "-type", "discord", "-url-file", urlFile, "-headers-file", headersFile, "-origin", "both")
	if strings.Contains(output, "private-") || !strings.Contains(output, "discord") || !strings.Contains(output, "next:") {
		t.Fatal("unsafe or incomplete provider output")
	}
	var h rest.Webhook
	p.json(t, &h, "webhook", "show", "discord")
	if h.Type != "discord" || h.Signing || h.Destination != "https://discord.com" || len(h.HeaderNames) != 1 || h.Origin != "both" {
		t.Fatal("incorrect provider settings")
	}
	p.cp(t, "webhook", "set", "discord", "-enabled=false")
	if output = p.cp(t, "-json", "webhook", "ls"); strings.Contains(output, "private-") {
		t.Fatal("provider credentials exposed")
	}
	if err := os.WriteFile(headersFile, []byte(`{}`), 0o600); err != nil {
		t.Fatal(err)
	}
	p.cp(t, "webhook", "set", "discord", "-headers-file", headersFile)
	p.json(t, &h, "webhook", "show", "discord")
	if len(h.HeaderNames) != 0 || h.Enabled || h.Type != "discord" {
		t.Fatal("clearing headers changed other settings")
	}
	output = p.cp(t, "webhook", "types")
	for _, name := range []string{"generic", "discord", "slack", "teams", "google-chat", "mattermost", "rocketchat", "ntfy", "gotify", "apprise", "next:"} {
		if !strings.Contains(output, name) {
			t.Fatalf("provider help missing %s", name)
		}
	}
}

func TestWebhookSigningOptIn(t *testing.T) {
	t.Parallel()
	p := newPerson(t, "alice")
	p.cp(t, "webhook", "add", "agent", "https://runner.example/hook")
	var h rest.Webhook
	p.json(t, &h, "webhook", "show", "agent")
	if h.Signing || h.Type != "generic" {
		t.Fatal("generic should default to unsigned")
	}
	key := filepath.Join(t.TempDir(), "key")
	if err := os.WriteFile(key, []byte(base64.StdEncoding.EncodeToString(make([]byte, 32))), 0o600); err != nil {
		t.Fatal(err)
	}
	p.cp(t, "webhook", "set", "agent", "-secret-file", key)
	p.json(t, &h, "webhook", "show", "agent")
	if !h.Signing {
		t.Fatal("key did not enable signing")
	}
	p.cp(t, "webhook", "set", "agent", "-enabled=false")
	p.json(t, &h, "webhook", "show", "agent")
	if !h.Signing {
		t.Fatal("omitting the key disabled signing")
	}
	p.cp(t, "webhook", "set", "agent", "-signing=false")
	p.json(t, &h, "webhook", "show", "agent")
	if h.Signing {
		t.Fatal("signing was not disabled")
	}
	if _, err := p.try(t, "webhook", "add", "discord", "https://discord.com/api/webhooks/id/token", "-type", "discord", "-secret-file", key); err == nil {
		t.Fatal("Discord accepted a generic signing key")
	}
}

func TestWebhookDeliveryPagination(t *testing.T) {
	t.Parallel()
	alice, bob := newPerson(t, "alice"), newPerson(t, "bob")
	peer(t, alice, bob)
	key := base64.StdEncoding.EncodeToString(make([]byte, 32))
	file := filepath.Join(t.TempDir(), "hook.key")
	if err := os.WriteFile(file, []byte(key), 0o600); err != nil {
		t.Fatal(err)
	}
	// Nothing listens on loopback port 1, so every delivery stays pending.
	alice.cp(t, "webhook", "add", "agent", "http://127.0.0.1:1/hook?project=testing", "-secret-file", file, "-origin", "outgoing")
	var th threadJSON
	alice.json(t, &th, "send", "bob", "Pagination test", "-m", "test")
	for range 4 {
		alice.cp(t, "reply", th.ID, "-m", "more")
	}
	var page rest.WebhookDeliveryList
	alice.json(t, &page, "webhook", "deliveries", "agent", "-limit", "2", "-status", "pending")
	if len(page.Deliveries) != 2 || !page.NextCursor.Set {
		t.Fatal("first page lacks cursor")
	}
	firstIDs := map[rest.ID]bool{}
	for _, d := range page.Deliveries {
		firstIDs[d.ID] = true
	}
	cursor := page.NextCursor.Value
	page = rest.WebhookDeliveryList{}
	alice.json(t, &page, "webhook", "deliveries", "agent", "-limit", "2", "-cursor", cursor, "-status", "pending")
	if len(page.Deliveries) != 2 || !page.NextCursor.Set {
		t.Fatal("second page missing")
	}
	for _, d := range page.Deliveries {
		if firstIDs[d.ID] {
			t.Fatal("pagination repeated a record")
		}
	}
	cursor = page.NextCursor.Value
	page = rest.WebhookDeliveryList{}
	alice.json(t, &page, "webhook", "deliveries", "agent", "-limit", "2", "-cursor", cursor, "-status", "pending")
	if len(page.Deliveries) != 1 || page.NextCursor.Set {
		t.Fatal("last page incorrect")
	}
	alice.json(t, &page, "webhook", "deliveries", "agent", "-status", "failed")
	if len(page.Deliveries) != 0 {
		t.Fatal("status filter ignored")
	}
}

func TestWebhookReachesReceiver(t *testing.T) {
	t.Parallel()
	alice, bob := newPerson(t, "alice"), newPerson(t, "bob")
	peer(t, alice, bob)
	key := base64.StdEncoding.EncodeToString([]byte("0123456789abcdef0123456789abcdef"))
	file := filepath.Join(t.TempDir(), "hook.key")
	if err := os.WriteFile(file, []byte(key), 0o600); err != nil {
		t.Fatal(err)
	}
	type attempt struct {
		header http.Header
		body   []byte
	}
	got := make(chan attempt, 10)
	receiver := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, err := io.ReadAll(r.Body)
		if err != nil {
			t.Error(err)
		}
		got <- attempt{r.Header.Clone(), body}
		w.WriteHeader(http.StatusNoContent)
	}))
	defer receiver.Close()
	bob.cp(t, "webhook", "add", "agent", receiver.URL+"/hook", "-secret-file", file, "-events", "thread.open")
	var th threadJSON
	alice.json(t, &th, "send", "bob", "Webhook test", "-m", "test")
	var a attempt
	select {
	case a = <-got:
	case <-time.After(10 * time.Second):
		t.Fatal("bob's daemon never notified the receiver")
	}
	raw, err := base64.StdEncoding.DecodeString(key)
	if err != nil {
		t.Fatal(err)
	}
	mac := hmac.New(sha256.New, raw)
	_, _ = mac.Write([]byte(a.header.Get("Webhook-Id") + "." + a.header.Get("Webhook-Timestamp") + "."))
	_, _ = mac.Write(a.body)
	if a.header.Get("Webhook-Signature") != "v1,"+base64.StdEncoding.EncodeToString(mac.Sum(nil)) {
		t.Fatal("receiver could not verify the signature")
	}
	var payload struct {
		ID, Type, Origin, Subject, Peer string
		MyTurn                          bool `json:"myTurn"`
	}
	if err = json.Unmarshal(a.body, &payload); err != nil {
		t.Fatal(err)
	}
	if payload.Type != "thread.open" || payload.Origin != "incoming" || payload.Subject != th.ID ||
		payload.Peer != "alice" || !payload.MyTurn || payload.ID != a.header.Get("Webhook-Id") {
		t.Fatalf("payload %s", a.body)
	}
	// The attempt is recorded just after the receiver answers.
	deadline := time.Now().Add(5 * time.Second)
	for {
		var page rest.WebhookDeliveryList
		bob.json(t, &page, "webhook", "deliveries", "agent")
		if len(page.Deliveries) == 1 && page.Deliveries[0].Status == "delivered" && string(page.Deliveries[0].Subject) == th.ID {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("deliveries %+v", page.Deliveries)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func TestWebhookFileErrors(t *testing.T) {
	t.Parallel()
	p := newPerson(t, "alice")
	missing := filepath.Join(t.TempDir(), "missing.url")
	output, err := p.try(t, "webhook", "add", "chat", "-type", "slack", "-url-file", missing)
	if exitOf(err) != exitUsage || !strings.Contains(output, "destination URL file "+missing+" does not exist") {
		t.Fatalf("missing file: %v: %s", err, output)
	}
	output, err = p.try(t, "webhook", "add", "chat", "-type", "slack", "-url-file", "-", "-headers-file", "-")
	if exitOf(err) != exitUsage || !strings.Contains(output, "only one of") {
		t.Fatalf("shared stdin: %v: %s", err, output)
	}
	p.cp(t, "webhook", "add", "agent", "https://runner.example/hook")
	if output, err = p.try(t, "webhook", "set", "agent", "-secret-file", "-", "-headers-file", "-"); exitOf(err) != exitUsage || !strings.Contains(output, "only one of") {
		t.Fatalf("shared stdin on set: %v: %s", err, output)
	}
}
