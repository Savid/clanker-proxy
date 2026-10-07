package server

import (
	"context"
	"errors"
	"testing"
)

func TestScopeFailsClosed(t *testing.T) {
	t.Parallel()
	if _, err := scopeOf(context.Background()); !errors.Is(err, errUnauthorized) {
		t.Fatalf("no caller recorded: %v", err)
	}
	if inScope(context.Background(), "bob") {
		t.Fatal("no caller recorded, yet in scope")
	}
	agent := withCaller(context.Background(), caller{agent: true, peers: []string{"bob"}})
	if !inScope(agent, "bob") || inScope(agent, "carol") {
		t.Fatal("agent scope not applied")
	}
	if !inScope(withCaller(context.Background(), caller{}), "carol") {
		t.Fatal("owner out of scope")
	}
}
