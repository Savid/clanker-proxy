package delivery

import (
	"bytes"
	"context"
	"crypto/sha256"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"strconv"
	"sync"
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

type webhookResult struct {
	name string
	err  error
}

// Run refills available slots as requests finish, without waiting for slow endpoints.
func (d *Webhooks) Run(ctx context.Context) error {
	ctx, cancel := context.WithCancel(ctx)
	var workers sync.WaitGroup
	defer func() { cancel(); workers.Wait() }()
	tick := time.NewTicker(time.Second)
	defer tick.Stop()
	active := map[string]string{}
	results := make(chan webhookResult, 4)
	for {
		if err := d.startWebhooks(ctx, active, results, &workers); err != nil && ctx.Err() == nil {
			d.log.ErrorContext(ctx, "webhook queue failed", "error", err)
		}
		select {
		case <-ctx.Done():
			return nil
		case result := <-results:
			delete(active, result.name)
			if result.err != nil && ctx.Err() == nil {
				d.log.ErrorContext(ctx, "webhook attempt failed", "webhook", result.name, "error", result.err)
			}
		case <-tick.C:
		}
	}
}

func (d *Webhooks) startWebhooks(ctx context.Context, active map[string]string, results chan<- webhookResult, workers *sync.WaitGroup) error {
	if len(active) == cap(results) {
		return nil
	}
	now := d.now().UTC()
	activeIDs := make([]string, 0, len(active))
	for _, id := range active {
		activeIDs = append(activeIDs, id)
	}
	if err := d.store.MaintainWebhooks(ctx, now, activeIDs); err != nil {
		return err
	}
	jobs, err := d.store.DueWebhooks(ctx, now)
	if err != nil {
		return err
	}
	for _, job := range jobs {
		if len(active) == cap(results) {
			break
		}
		if _, running := active[job.Name]; running {
			continue
		}
		active[job.Name] = job.ID
		workers.Go(func() { results <- webhookResult{name: job.Name, err: d.deliverWebhook(ctx, job)} })
	}
	return nil
}

// Drain attempts one due notification per endpoint, with bounded parallelism.
func (d *Webhooks) Drain(ctx context.Context) error {
	now := d.now().UTC()
	if err := d.store.MaintainWebhooks(ctx, now, nil); err != nil {
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
	status, message, retryAfter := d.postWebhook(ctx, hook, job)
	if ctx.Err() != nil {
		return ctx.Err()
	}
	now := d.now().UTC()
	next := now.Add(webhookBackoff(job.ID, job.Attempts))
	if retryAfter.After(next) {
		next = retryAfter
	}
	if !retryAfter.IsZero() {
		cooldown := next
		if cooldown.After(job.RetryUntil) {
			cooldown = job.RetryUntil
		}
		if err = d.store.DeferWebhook(ctx, job.ID, hook.URL, cooldown); err != nil {
			return err
		}
	}
	if status == "pending" && !next.Before(job.RetryUntil) {
		status = "failed"
		message = "retry window expired"
	}
	return d.store.FinishWebhook(ctx, job.ID, status, message, next, now)
}

func (d *Webhooks) postWebhook(ctx context.Context, hook webhook.Config, job store.WebhookDelivery) (string, string, time.Time) {
	timestamp := strconv.FormatInt(d.now().Unix(), 10)
	sig, err := webhook.Signature(hook.Secret, job.ID, timestamp, job.Payload)
	if err != nil {
		return "failed", "invalid signing key", time.Time{}
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, hook.URL, bytes.NewReader(job.Payload))
	if err != nil {
		return "failed", "invalid destination", time.Time{}
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Webhook-Id", job.ID)
	req.Header.Set("Webhook-Timestamp", timestamp)
	req.Header.Set("Webhook-Signature", sig)
	resp, err := d.http.Do(req)
	if err != nil {
		return "pending", "connection failed or timed out", time.Time{}
	}
	defer resp.Body.Close()
	_, _ = io.Copy(io.Discard, io.LimitReader(resp.Body, 4096))
	if resp.StatusCode >= 200 && resp.StatusCode < 300 {
		return "delivered", "", time.Time{}
	}
	message := fmt.Sprintf("HTTP %d", resp.StatusCode)
	if resp.StatusCode == http.StatusRequestTimeout || resp.StatusCode == http.StatusTooManyRequests || resp.StatusCode >= 500 {
		var cooldown time.Time
		if resp.StatusCode == http.StatusTooManyRequests || resp.StatusCode == http.StatusServiceUnavailable {
			now := d.now().UTC()
			cooldown = webhookRetryAfter(resp.Header.Get("Retry-After"), now, job.RetryUntil)
			if cooldown.IsZero() {
				cooldown = now.Add(webhookBackoff(job.ID, job.Attempts))
			}
		}
		return "pending", message, cooldown
	}
	return "failed", message, time.Time{}
}

func webhookBackoff(id string, attempts int) time.Duration {
	base := Backoff(attempts)
	// UUID-derived jitter remains stable across restarts and spreads retries across endpoints.
	sum := sha256.Sum256([]byte(id + ":" + strconv.Itoa(attempts)))
	return base + base*time.Duration(sum[0])/(4*255)
}

func webhookRetryAfter(value string, now, until time.Time) time.Time {
	if seconds, err := strconv.ParseInt(value, 10, 64); err == nil && seconds >= 0 {
		remaining := max(time.Duration(0), until.Sub(now))
		if seconds >= int64(remaining/time.Second) {
			return until
		}
		return now.Add(time.Duration(seconds) * time.Second)
	}
	if at, err := http.ParseTime(value); err == nil && at.After(now) {
		if at.After(until) {
			return until
		}
		return at
	}
	return time.Time{}
}
