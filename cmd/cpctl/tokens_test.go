package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestAgentToken(t *testing.T) {
	t.Parallel()
	alice, bob := newPerson(t, "alice"), newPerson(t, "bob")
	peer(t, alice, bob)
	var th threadJSON
	bob.json(t, &th, "send", "alice", "Check the build", "-m", "does it pass?")

	file := filepath.Join(t.TempDir(), "helper.token")
	out := alice.cp(t, "token", "add", "helper", "-o", file, "-expires", "1h")
	data, err := os.ReadFile(file)
	if err != nil {
		t.Fatal(err)
	}
	token := strings.TrimSpace(string(data))
	if info, _ := os.Stat(file); info.Mode().Perm() != 0o600 || !strings.HasPrefix(token, "cpa_") || strings.Contains(out, token) || !strings.Contains(out, "next:") {
		t.Fatalf("token file %v, output %s", info.Mode(), out)
	}
	if out, err = alice.try(t, "token", "add", "helper", "-o", filepath.Join(t.TempDir(), "x")); exitOf(err) != exitRefused || !strings.Contains(out, "cpctl token rm") {
		t.Fatalf("duplicate name: %v %s", err, out)
	}
	if _, err = alice.try(t, "token", "add", "other", "-o", file); exitOf(err) != exitUsage {
		t.Fatalf("overwrote a token file: %v", err)
	}
	if out = alice.cp(t, "token", "add", "piped", "-o", "-"); !strings.HasPrefix(out, "cpa_") || strings.Count(out, "\n") != 1 {
		t.Fatalf("stdout token: %q", out)
	}

	agent := &person{name: "alice's agent", url: alice.url, token: token}
	agent.cp(t, "show", th.ID)
	agent.cp(t, "reply", th.ID, "-m", "looking")
	for _, args := range [][]string{{"peer", "ls"}, {"send", "bob", "hi"}, {"token", "ls"}, {"webhook", "ls"}} {
		if out, err = agent.try(t, args...); exitOf(err) != exitRefused || !strings.Contains(out, "agent token") {
			t.Fatalf("agent ran %v: %v %s", args, err, out)
		}
	}

	if out = alice.cp(t, "token", "ls"); !strings.Contains(out, "helper") || !strings.Contains(out, "used ") || !strings.Contains(out, "piped") {
		t.Fatalf("token ls: %s", out)
	}
	alice.cp(t, "token", "rm", "helper")
	if _, err = agent.try(t, "show", th.ID); exitOf(err) != exitAuth {
		t.Fatalf("revoked token: %v", err)
	}
}
