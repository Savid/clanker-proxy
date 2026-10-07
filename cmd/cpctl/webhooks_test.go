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
	if _, err := p.try(t, "webhook", "show", "agent"); exitOf(err) != exitNotFound {
		t.Fatalf("deleted webhook: %v", err)
	}
}
