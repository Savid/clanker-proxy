package delivery

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"strconv"
	"time"

	"golang.org/x/sync/errgroup"

	"github.com/savid/clanker-proxy/internal/store"
	"github.com/savid/clanker-proxy/pkg/webhook"
)

// Webhooks delivers owner notifications independently of the peer outbox.
// Only one Run or Drain may execute at a time for a given database.
type Webhooks struct {
	log   *slog.Logger
	store *store.Store
	now   func() time.Time
	http  *http.Client
}

// NewWebhooks uses the same injectable scheduling clock as peer delivery.
func NewWebhooks(log *slog.Logger, st *store.Store, cfg Config) *Webhooks {
	if cfg.Now == nil {
		cfg.Now = time.Now
	}
	return &Webhooks{log: log, store: st, now: cfg.Now, http: &http.Client{Timeout: 10 * time.Second, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}}
}

// Run polls persisted work, including deliveries left by a previous process.
func (d *Webhooks) Run(ctx context.Context) error {
	tick := time.NewTicker(time.Second)
	defer tick.Stop()
	for {
		if err := d.Drain(ctx); err != nil && ctx.Err() == nil {
			d.log.ErrorContext(ctx, "webhook queue failed", "error", err)
		}
		select {
		case <-ctx.Done():
			return nil
		case <-tick.C:
		}
	}
}

// Drain attempts one due notification per endpoint, with bounded parallelism.
func (d *Webhooks) Drain(ctx context.Context) error {
	now := d.now().UTC()
	if err := d.store.MaintainWebhooks(ctx, now); err != nil {
		return err
	}
	jobs, err := d.store.DueWebhooks(ctx, now)
	if err != nil {
		return err
	}
	var group errgroup.Group
	group.SetLimit(4)
	for _, job := range jobs {
		group.Go(func() error { return d.deliverWebhook(ctx, job) })
	}
	return group.Wait()
}

func (d *Webhooks) deliverWebhook(ctx context.Context, job store.WebhookDelivery) error {
	// Re-read just before sending so queued notifications respect pauses and key rotation.
	hook, err := d.store.WebhookDestination(ctx, job.ID)
	if errors.Is(err, store.ErrNotFound) {
		return nil
	}
	if err != nil {
		return err
	}
	if !hook.Enabled {
		return nil
	}
	status, message := d.postWebhook(ctx, hook, job)
	if ctx.Err() != nil {
		return ctx.Err()
	}
	now := d.now().UTC()
	next := now.Add(Backoff(job.Attempts))
	if status == "pending" && !next.Before(job.RetryUntil) {
		status = "failed"
		message = "retry window expired"
	}
	return d.store.FinishWebhook(ctx, job.ID, status, message, next, now)
}

func (d *Webhooks) postWebhook(ctx context.Context, hook webhook.Config, job store.WebhookDelivery) (string, string) {
	timestamp := strconv.FormatInt(d.now().Unix(), 10)
	sig, err := webhook.Signature(hook.Secret, job.ID, timestamp, job.Payload)
	if err != nil {
		return "failed", "invalid signing key"
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, hook.URL, bytes.NewReader(job.Payload))
	if err != nil {
		return "failed", "invalid destination"
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Webhook-Id", job.ID)
	req.Header.Set("Webhook-Timestamp", timestamp)
	req.Header.Set("Webhook-Signature", sig)
	resp, err := d.http.Do(req)
	if err != nil {
		return "pending", "connection failed or timed out"
	}
	defer resp.Body.Close()
	_, _ = io.Copy(io.Discard, io.LimitReader(resp.Body, 4096))
	if resp.StatusCode >= 200 && resp.StatusCode < 300 {
		return "delivered", ""
	}
	message := fmt.Sprintf("HTTP %d", resp.StatusCode)
	if resp.StatusCode == http.StatusRequestTimeout || resp.StatusCode == http.StatusTooManyRequests || resp.StatusCode >= 500 {
		return "pending", message
	}
	return "failed", message
}
