package delivery

import (
	"testing"
	"time"
)

func TestWebhookRetryAfterBounds(t *testing.T) {
	t.Parallel()
	now := time.Date(2026, 10, 7, 0, 0, 0, 0, time.UTC)
	until := now.Add(time.Hour)
	for _, tc := range []struct {
		value string
		want  time.Time
	}{
		{"garbage", time.Time{}},
		{"-1", time.Time{}},
		{"9223372036854775808", time.Time{}},
		{"9223372036854775807", until},
		{"0", now},
		{"Sun, 06 Nov 1994 08:49:37 GMT", time.Time{}},
		{"Wed, 07 Oct 2026 02:00:00 GMT", until},
	} {
		if got := webhookRetryAfter(tc.value, now, until); !got.Equal(tc.want) {
			t.Errorf("%q gave %s, want %s", tc.value, got, tc.want)
		}
	}
	a, b := webhookBackoff("a", 0), webhookBackoff("b", 0)
	if a == b || a < 15*time.Second || a > 19*time.Second || b < 15*time.Second || b > 19*time.Second {
		t.Fatal("retry jitter does not spread bounded attempts")
	}
}
