package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"io"
	"strings"
	"testing"

	"github.com/savid/clanker-proxy/cli/update"
)

type fakeReleaseClient struct {
	result    update.Result
	err       error
	installed bool
	dir       string
}

func (c *fakeReleaseClient) Check(context.Context, string) (update.Result, error) {
	return c.result, c.err
}

func (c *fakeReleaseClient) Install(_ context.Context, _, dir string) (update.Result, error) {
	c.installed, c.dir = true, dir
	return c.result, c.err
}

func TestUpdateOutput(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name        string
		check, json bool
		result      update.Result
		want        string
	}{
		{name: "available", check: true, result: update.Result{Current: "v0.1.0", Latest: "v0.2.0", Available: true}, want: "cpctl update"},
		{name: "installed", result: update.Result{Latest: "v0.2.0", Installed: true}, want: "restart cpd"},
		{name: "current", check: true, result: update.Result{Current: "v0.2.0", Latest: "v0.2.0"}, want: "latest stable v0.2.0"},
		{name: "json", check: true, json: true, result: update.Result{Current: "v0.1.0", Latest: "v0.2.0", Available: true}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			var out bytes.Buffer
			a := &app{ctx: t.Context(), stdout: &out, json: tc.json}
			client := &fakeReleaseClient{result: tc.result}
			resolve := func() (string, error) {
				if tc.check {
					t.Fatal("check resolved installation directory")
				}
				return "/example/bin", nil
			}
			if err := a.runUpdate(tc.check, client, resolve); err != nil {
				t.Fatal(err)
			}
			if client.installed == tc.check || (!tc.check && client.dir != "/example/bin") {
				t.Fatalf("wrong install call: %+v", client)
			}
			checkUpdateOutput(t, out.Bytes(), tc.json, tc.result, tc.want)
		})
	}
}

func checkUpdateOutput(t *testing.T, out []byte, asJSON bool, result update.Result, want string) {
	t.Helper()
	if asJSON {
		var got update.Result
		if err := json.Unmarshal(out, &got); err != nil || got != result {
			t.Fatalf("JSON = %s, %v", out, err)
		}
		return
	}
	if !strings.Contains(string(out), want) || !strings.Contains(string(out), "next:") {
		t.Fatalf("output = %s", out)
	}
}

func TestUpdateFailureHasHint(t *testing.T) {
	t.Parallel()
	a := &app{ctx: t.Context(), stdout: io.Discard}
	err := a.runUpdate(true, &fakeReleaseClient{err: errors.New("GitHub unavailable")}, nil)
	f := a.classify(err, updateCommands()[0])
	if f.exit != exitError || f.hint == "" || !strings.Contains(f.msg, "GitHub unavailable") {
		t.Fatalf("failure = %+v", f)
	}
}

func TestLocalCommandDoesNotCreateDaemonClient(t *testing.T) {
	t.Parallel()
	a := &app{ctx: t.Context(), url: "invalid daemon URL"}
	cmd := *updateCommands()[0]
	called := false
	cmd.flags = func(*flag.FlagSet) func(*app, []string) error {
		return func(a *app, _ []string) error {
			called = true
			if a.client != nil {
				t.Fatal("local command initialized daemon client")
			}
			return nil
		}
	}
	if err := cmd.exec(a, nil); err != nil || !called {
		t.Fatalf("local command = %v, called %t", err, called)
	}
}
