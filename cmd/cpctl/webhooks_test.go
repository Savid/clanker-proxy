package main

import (
	"encoding/base64"
	"os"
	"path/filepath"
	"strings"
	"testing"

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
	if exitOf(duplicateErr) != exitRefused || !strings.Contains(output, "hint: cpctl webhook ls") {
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

func TestWebhookDeliveryPagination(t *testing.T) {
	t.Parallel()
	alice, bob := newPerson(t, "alice"), newPerson(t, "bob")
	peer(t, alice, bob)
	key := base64.StdEncoding.EncodeToString(make([]byte, 32))
	file := filepath.Join(t.TempDir(), "hook.key")
	if err := os.WriteFile(file, []byte(key), 0o600); err != nil {
		t.Fatal(err)
	}
	alice.cp(t, "webhook", "add", "agent", "https://runner.example/hook?project=testing", "-secret-file", file, "-origin", "outgoing")
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
