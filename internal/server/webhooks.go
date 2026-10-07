package server

import (
	"context"
	"net/url"

	"github.com/savid/clanker-proxy/api/rest"
	"github.com/savid/clanker-proxy/pkg/webhook"
)

func webhookResponse(c webhook.Config) *rest.Webhook {
	events := make(rest.WebhookEvents, 0, len(c.Events))
	for _, event := range c.Events {
		events = append(events, rest.WebhookEventsItem(event))
	}
	u, _ := url.Parse(c.URL)
	out := &rest.Webhook{Name: rest.Name(c.Name), URL: rest.WebhookURL(*u), Events: events, Origin: rest.WebhookOrigin(c.Origin), Enabled: c.Enabled}
	if !c.PausedUntil.IsZero() {
		out.PausedUntil = rest.NewOptDateTime(c.PausedUntil)
	}
	return out
}

func webhookURL(u rest.WebhookURL) string {
	plain := url.URL(u)
	return plain.String()
}

func webhookEvents(events rest.WebhookEvents) []string {
	out := make([]string, 0, len(events))
	for _, event := range events {
		out = append(out, string(event))
	}
	return out
}

func (o *operations) ListWebhooks(ctx context.Context) (*rest.WebhookList, error) {
	hooks, err := o.inbox.Webhooks(ctx)
	if err != nil {
		return nil, err
	}
	out := &rest.WebhookList{Webhooks: make([]rest.Webhook, 0, len(hooks))}
	for _, c := range hooks {
		out.Webhooks = append(out.Webhooks, *webhookResponse(c))
	}
	return out, nil
}

func (o *operations) GetWebhook(ctx context.Context, p rest.GetWebhookParams) (*rest.Webhook, error) {
	c, err := o.inbox.Webhook(ctx, string(p.Name))
	if err != nil {
		return nil, err
	}
	return webhookResponse(c), nil
}

func (o *operations) CreateWebhook(ctx context.Context, r *rest.WebhookCreate) (*rest.Webhook, error) {
	c, err := o.inbox.CreateWebhook(ctx, webhook.Config{Name: string(r.Name), URL: webhookURL(r.URL), Secret: string(r.Secret), Events: webhookEvents(r.Events), Origin: string(r.Origin), Enabled: r.Enabled})
	if err != nil {
		return nil, err
	}
	return webhookResponse(c), nil
}

func (o *operations) UpdateWebhook(ctx context.Context, r *rest.WebhookUpdate, p rest.UpdateWebhookParams) (*rest.Webhook, error) {
	c, err := o.inbox.UpdateWebhook(ctx, webhook.Config{Name: string(p.Name), URL: webhookURL(r.URL), Secret: string(r.Secret.Or("")), Events: webhookEvents(r.Events), Origin: string(r.Origin), Enabled: r.Enabled})
	if err != nil {
		return nil, err
	}
	return webhookResponse(c), nil
}

func (o *operations) DeleteWebhook(ctx context.Context, p rest.DeleteWebhookParams) error {
	return o.inbox.DeleteWebhook(ctx, string(p.Name))
}

func (o *operations) ListWebhookDeliveries(ctx context.Context, p rest.ListWebhookDeliveriesParams) (*rest.WebhookDeliveryList, error) {
	ds, cursor, err := o.inbox.WebhookDeliveries(ctx, string(p.Name), p.Cursor.Or(""), int(p.Limit.Or(100)), string(p.Status.Or("")))
	if err != nil {
		return nil, err
	}
	out := &rest.WebhookDeliveryList{Deliveries: make([]rest.WebhookDelivery, 0, len(ds))}
	if cursor != "" {
		out.NextCursor = rest.NewOptString(cursor)
	}
	for _, d := range ds {
		r := rest.WebhookDelivery{ID: rest.ID(d.ID), Event: rest.WebhookEventType(d.Event), Origin: rest.WebhookEventOrigin(d.Origin), Subject: rest.WebhookSubject(d.Subject), Status: rest.WebhookDeliveryStatus(d.Status), Attempts: count(d.Attempts), CreatedAt: d.CreatedAt, LastError: d.LastError}
		if d.Status == "pending" {
			r.NextAttemptAt = rest.NewOptDateTime(d.NextAttemptAt)
		}
		out.Deliveries = append(out.Deliveries, r)
	}
	return out, nil
}

func (o *operations) RetryWebhookDelivery(ctx context.Context, p rest.RetryWebhookDeliveryParams) error {
	return o.inbox.RetryWebhook(ctx, string(p.Name), string(p.ID))
}
