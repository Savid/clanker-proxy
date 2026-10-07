package delivery

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"math"
	"net/http"
	"strconv"
	"time"

	"github.com/savid/clanker-proxy/internal/store"
	"github.com/savid/clanker-proxy/pkg/webhook"
)

// Errors returned here are recorded as delivery failures, so they must not
// include anything copied from the destination URL or authentication headers.
func webhookRequest(ctx context.Context, hook webhook.Config, job store.WebhookDelivery, now time.Time) (*http.Request, error) {
	provider, ok := webhook.LookupProvider(hook.Type)
	if !ok {
		return nil, errors.New("unsupported webhook type")
	}
	message, err := webhook.Render(hook.Type, job.Payload)
	if err != nil {
		return nil, errors.New("cannot format notification")
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, hook.URL, bytes.NewReader(message.Body))
	if err != nil {
		return nil, errors.New("invalid destination")
	}
	if len(provider.Query) > 0 {
		query := req.URL.Query()
		for name, value := range provider.Query {
			query.Set(name, value)
		}
		req.URL.RawQuery = query.Encode()
	}
	// The owner's headers come after the formatter's, so a configured header
	// such as ntfy's Title replaces the generated one.
	for _, h := range message.Headers {
		req.Header.Set(h.Name, h.Value)
	}
	for _, h := range hook.Headers {
		req.Header.Set(h.Name, h.Value)
	}
	req.Header.Set("Content-Type", message.ContentType)
	req.Header.Set("User-Agent", "cpd")
	if provider.Signed {
		timestamp := strconv.FormatInt(now.Unix(), 10)
		req.Header.Set("Webhook-Id", job.ID)
		req.Header.Set("Idempotency-Key", job.ID)
		req.Header.Set("Webhook-Timestamp", timestamp)
		if hook.Secret != "" {
			sig, signErr := webhook.Signature(hook.Secret, job.ID, timestamp, message.Body)
			if signErr != nil {
				return nil, errors.New("invalid signing key")
			}
			req.Header.Set("Webhook-Signature", sig)
		}
	}
	return req, nil
}

// bodyRetryAfter reads a JSON retry_after in seconds, which may be fractional.
func bodyRetryAfter(body []byte, now, until time.Time) time.Time {
	var response struct {
		RetryAfter float64 `json:"retry_after"` //nolint:tagliatelle // Discord's wire format uses snake_case.
	}
	if err := json.Unmarshal(body, &response); err != nil || response.RetryAfter <= 0 {
		return time.Time{}
	}
	// Clamp before converting: a huge float does not fit in a Duration.
	seconds := math.Min(math.Ceil(response.RetryAfter), maxBackoff.Seconds())
	return retryAt(time.Duration(seconds)*time.Second, now, until)
}
