package inbox

import (
	"strings"
	"testing"

	"github.com/savid/clanker-proxy/pkg/thread"
)

func TestFullThread(t *testing.T) {
	t.Parallel()
	small := thread.Event{Thread: "t", Body: "x"}
	big := thread.Event{Thread: "t", Body: strings.Repeat("x", thread.MaxBody)}

	if err := full(make([]thread.Event, thread.MaxEvents-1), small); err != nil {
		t.Fatalf("below the event limit: %v", err)
	}
	if err := full(make([]thread.Event, thread.MaxEvents), small); err == nil {
		t.Fatal("past the event limit")
	}

	events := make([]thread.Event, thread.MaxThreadBody/len(big.Body))
	for i := range events {
		events[i] = big
	}
	if err := full(events[:len(events)-1], big); err != nil {
		t.Fatalf("at the body limit: %v", err)
	}
	if err := full(events, small); err == nil {
		t.Fatal("past the body limit")
	}
}
