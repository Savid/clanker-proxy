package inbox

import (
	"context"
	"errors"

	"github.com/savid/clanker-proxy/internal/store"
	"github.com/savid/clanker-proxy/pkg/webhook"
)

// Webhooks returns owner-managed destinations.
func (b *Inbox) Webhooks(ctx context.Context) ([]webhook.Config, error) { return b.store.Webhooks(ctx) }

// Webhook returns one destination. API serialization must omit its signing key.
func (b *Inbox) Webhook(ctx context.Context, name string) (webhook.Config, error) {
	c, err := b.store.Webhook(ctx, name)
	return c, webhookError(err)
}

// CreateWebhook validates a new notification destination.
func (b *Inbox) CreateWebhook(ctx context.Context, c webhook.Config) (webhook.Config, error) {
	if err := c.Validate(); err != nil {
		return c, errorf(KindInvalid, "%v", err)
	}
	return c, webhookError(b.store.CreateWebhook(ctx, c))
}

// UpdateWebhook replaces filters and destination, retaining an omitted signing key.
func (b *Inbox) UpdateWebhook(ctx context.Context, c webhook.Config) (webhook.Config, error) {
	old, err := b.Webhook(ctx, c.Name)
	if err != nil {
		return c, err
	}
	check := c
	if check.Secret == "" {
		check.Secret = old.Secret
	}
	if err = check.Validate(); err != nil {
		return c, errorf(KindInvalid, "%v", err)
	}
	if err = b.store.UpdateWebhook(ctx, c); err != nil {
		return c, webhookError(err)
	}
	return b.Webhook(ctx, c.Name)
}

// DeleteWebhook removes a destination and its delivery history.
func (b *Inbox) DeleteWebhook(ctx context.Context, name string) error {
	return webhookError(b.store.DeleteWebhook(ctx, name))
}

// WebhookDeliveries returns recent delivery metadata.
func (b *Inbox) WebhookDeliveries(ctx context.Context, name string, f store.WebhookFilter) ([]store.WebhookDelivery, error) {
	if _, err := b.Webhook(ctx, name); err != nil {
		return nil, err
	}
	return b.store.WebhookDeliveries(ctx, name, f)
}

// RetryWebhook explicitly retries a terminal failure.
func (b *Inbox) RetryWebhook(ctx context.Context, name, id string) error {
	return webhookError(b.store.RetryWebhook(ctx, name, id, b.now()))
}

func webhookError(err error) error {
	switch {
	case errors.Is(err, store.ErrNotFound):
		return errorf(KindNotFound, "webhook or delivery not found")
	case errors.Is(err, store.ErrExists):
		return errorf(KindConflict, "webhook name already exists")
	case errors.Is(err, store.ErrWebhookRetry):
		return errorf(KindConflict, "only failed deliveries of enabled webhooks can be retried")
	default:
		return err
	}
}
