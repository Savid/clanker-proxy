package delivery

import (
	"context"
	"crypto/sha256"
	"crypto/tls"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"os"
	"strconv"
	"sync"
	"syscall"
	"time"

	"golang.org/x/sync/errgroup"

	"github.com/savid/clanker-proxy/internal/store"
	"github.com/savid/clanker-proxy/pkg/webhook"
)

const (
	// webhookWorkers bounds concurrent requests across all endpoints.
	webhookWorkers = 4
	webhookTimeout = 10 * time.Second
	// webhookMaintain spaces expiry and retention writes; DueWebhooks skips
	// expired work in between.
	webhookMaintain = time.Minute
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
	return &Webhooks{log: log, store: st, now: cfg.Now, http: &http.Client{Timeout: webhookTimeout, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}}
}

type webhookResult struct {
	name string
	err  error
}

// webhookRun is Run's in-memory state.
type webhookRun struct {
	active map[string]string
	// held keeps an endpoint idle after a store error: the attempt's outcome
	// was not recorded, so restarting at once would resend the same delivery
	// in a tight loop.
	held       map[string]time.Time
	maintained time.Time
}

// Run refills available slots as requests finish, without waiting for slow endpoints.
func (d *Webhooks) Run(ctx context.Context) error {
	ctx, cancel := context.WithCancel(ctx)
	var workers sync.WaitGroup
	defer func() { cancel(); workers.Wait() }()
	tick := time.NewTicker(time.Second)
	defer tick.Stop()
	r := &webhookRun{active: map[string]string{}, held: map[string]time.Time{}}
	results := make(chan webhookResult, webhookWorkers)
	for {
		if err := d.startWebhooks(ctx, r, results, &workers); err != nil && ctx.Err() == nil {
			d.log.ErrorContext(ctx, "webhook queue failed", "error", err)
		}
		select {
		case <-ctx.Done():
			return nil
		case result := <-results:
			delete(r.active, result.name)
			if result.err != nil && ctx.Err() == nil {
				r.held[result.name] = d.now().Add(minBackoff)
				d.log.ErrorContext(ctx, "webhook attempt not recorded", "webhook", result.name, "error", result.err)
			}
		case <-tick.C:
		}
	}
}

func (d *Webhooks) startWebhooks(ctx context.Context, r *webhookRun, results chan<- webhookResult, workers *sync.WaitGroup) error {
	if len(r.active) == webhookWorkers {
		return nil
	}
	now := d.now().UTC()
	if now.Sub(r.maintained) >= webhookMaintain {
		activeIDs := make([]string, 0, len(r.active))
		for _, id := range r.active {
			activeIDs = append(activeIDs, id)
		}
		if err := d.store.MaintainWebhooks(ctx, now, activeIDs); err != nil {
			return err
		}
		r.maintained = now
	}
	jobs, err := d.store.DueWebhooks(ctx, now)
	if err != nil {
		return err
	}
	for name, until := range r.held {
		if !now.Before(until) {
			delete(r.held, name)
		}
	}
	for _, job := range jobs {
		if len(r.active) == webhookWorkers {
			break
		}
		if _, running := r.active[job.Name]; running {
			continue
		}
		if _, held := r.held[job.Name]; held {
			continue
		}
		r.active[job.Name] = job.ID
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
	group.SetLimit(webhookWorkers)
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
	if err = d.store.StartWebhook(ctx, job.ID); err != nil {
		return err
	}
	status, message, retryAfter := d.postWebhook(ctx, hook, job)
	if ctx.Err() != nil {
		return ctx.Err()
	}
	now := d.now().UTC()
	next := latest(now.Add(webhookBackoff(job.ID, job.Attempts)), retryAfter)
	if status == "pending" {
		pause := webhookPause(hook, job, now, retryAfter)
		if err = d.store.DeferWebhook(ctx, job.ID, hook, pause); err != nil {
			return err
		}
		// The delivery cannot go before its endpoint does; say so in nextAttemptAt.
		next = latest(next, pause)
		if !next.Before(job.RetryUntil) {
			status = "failed"
			message = "retry window expired after " + message
		}
	}
	if err = d.store.FinishWebhook(ctx, job.ID, status, message, next, now); err != nil {
		return err
	}
	attrs := []any{"webhook", hook.Name, "delivery", job.ID, "attempt", job.Attempts + 1}
	switch status {
	case "delivered":
		d.log.InfoContext(ctx, "webhook delivered", attrs...)
	case "pending":
		d.log.WarnContext(ctx, "webhook attempt failed; will retry", append(attrs, "retry_at", next, "error", message)...)
	default:
		d.log.ErrorContext(ctx, "webhook delivery failed", append(attrs, "error", message)...)
	}
	return nil
}

// webhookPause holds the whole endpoint after a retryable failure, backing off
// on its consecutive failures, so a dead receiver is probed by one delivery at
// a time rather than by every queued one. The cap keeps a nearly expired
// delivery's backoff from holding up fresher work.
func webhookPause(hook webhook.Config, job store.WebhookDelivery, now, retryAfter time.Time) time.Time {
	pause := latest(now.Add(webhookBackoff(hook.Name, hook.Failures)), retryAfter)
	if pause.After(job.RetryUntil) {
		return job.RetryUntil
	}
	return pause
}

func latest(a, b time.Time) time.Time {
	if b.After(a) {
		return b
	}
	return a
}

func (d *Webhooks) postWebhook(ctx context.Context, hook webhook.Config, job store.WebhookDelivery) (string, string, time.Time) {
	req, err := webhookRequest(ctx, hook, job, d.now())
	if err != nil {
		return "failed", err.Error(), time.Time{}
	}
	resp, err := d.http.Do(req)
	if err != nil {
		return "pending", transportError(err), time.Time{}
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(io.LimitReader(resp.Body, 4096))
	if resp.StatusCode >= 200 && resp.StatusCode < 300 {
		return "delivered", "", time.Time{}
	}
	message := fmt.Sprintf("HTTP %d", resp.StatusCode)
	if resp.StatusCode == http.StatusRequestTimeout || resp.StatusCode == http.StatusTooManyRequests || resp.StatusCode >= 500 {
		var cooldown time.Time
		if resp.StatusCode == http.StatusTooManyRequests || resp.StatusCode == http.StatusServiceUnavailable {
			cooldown = webhookRetryAfter(resp.Header.Get("Retry-After"), d.now().UTC(), job.RetryUntil)
		}
		if provider, _ := webhook.LookupProvider(hook.Type); provider.RetryAfterBody && resp.StatusCode == http.StatusTooManyRequests {
			cooldown = latest(cooldown, bodyRetryAfter(body, d.now().UTC(), job.RetryUntil))
		}
		return "pending", message, cooldown
	}
	return "failed", message, time.Time{}
}

// transportError omits URLs and response bodies, which can contain credentials.
func transportError(err error) string {
	var dnsErr *net.DNSError
	var certErr *tls.CertificateVerificationError
	var headerErr tls.RecordHeaderError
	switch {
	case errors.As(err, &dnsErr):
		return "DNS lookup failed"
	case errors.As(err, &certErr):
		return "TLS certificate rejected"
	case errors.As(err, &headerErr):
		return "TLS handshake failed"
	case os.IsTimeout(err) || errors.Is(err, context.DeadlineExceeded):
		return "timed out"
	case errors.Is(err, syscall.ECONNREFUSED):
		return "connection refused"
	default:
		return "connection failed"
	}
}

func webhookBackoff(key string, attempts int) time.Duration {
	base := Backoff(attempts)
	// Key-derived jitter remains stable across restarts and spreads retries across endpoints.
	sum := sha256.Sum256([]byte(key + ":" + strconv.Itoa(attempts)))
	return base + base*time.Duration(sum[0])/(4*255)
}

// webhookRetryAfter honors a Retry-After header, in seconds or as a date.
func webhookRetryAfter(value string, now, until time.Time) time.Time {
	if seconds, err := strconv.ParseInt(value, 10, 64); err == nil && seconds >= 0 {
		return retryAt(time.Duration(min(seconds, int64(maxBackoff/time.Second)))*time.Second, now, until)
	}
	if at, err := http.ParseTime(value); err == nil && at.After(now) {
		return retryAt(at.Sub(now), now, until)
	}
	return time.Time{}
}

// retryAt honors a receiver's requested delay for at most maxBackoff, so one
// misconfigured proxy cannot pause an endpoint for its whole retry window.
func retryAt(delay time.Duration, now, until time.Time) time.Time {
	limit := now.Add(maxBackoff)
	if until.Before(limit) {
		limit = until
	}
	if at := now.Add(delay); at.Before(limit) {
		return at
	}
	return limit
}
