package server

import (
	"context"

	"github.com/savid/clanker-proxy/api/rest"
	"github.com/savid/clanker-proxy/pkg/webhook"
)

func webhookResponse(c webhook.Config) *rest.Webhook {
	events := make(rest.WebhookEvents, 0, len(c.Events))
	for _, event := range c.Events {
		events = append(events, rest.WebhookEventsItem(event))
	}
	names := make([]rest.WebhookHeaderName, 0, len(c.Headers))
	for _, name := range c.HeaderNames() {
		names = append(names, rest.WebhookHeaderName(name))
	}
	out := &rest.Webhook{Name: rest.Name(c.Name), Type: rest.WebhookType(c.Type), Destination: c.Destination(), Signing: c.Secret != "", HeaderNames: names, Events: events, Origin: rest.WebhookOrigin(c.Origin), Peers: toPeers(c.Peers), Enabled: c.Enabled}
	if !c.PausedUntil.IsZero() {
		out.PausedUntil = rest.NewOptDateTime(c.PausedUntil)
	}
	return out
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
	c, err := o.inbox.CreateWebhook(ctx, webhook.Config{Name: string(r.Name), Type: string(r.Type), URL: string(r.URL), Secret: string(r.Secret.Or("")), Headers: webhookHeaders(r.Headers), Events: webhookEvents(r.Events), Origin: string(r.Origin), Peers: peerNames(r.Peers), Enabled: r.Enabled})
	if err != nil {
		return nil, err
	}
	return webhookResponse(c), nil
}

func (o *operations) UpdateWebhook(ctx context.Context, r *rest.WebhookUpdate, p rest.UpdateWebhookParams) (*rest.Webhook, error) {
	change := webhook.Update{}
	if v, ok := r.URL.Get(); ok {
		change.URL = new(string(v))
	}
	if r.Events != nil {
		change.Events = webhookEvents(r.Events)
	}
	if v, ok := r.Origin.Get(); ok {
		change.Origin = new(string(v))
	}
	if r.Peers != nil {
		change.Peers = new(peerNames(r.Peers))
	}
	if v, ok := r.Enabled.Get(); ok {
		change.Enabled = &v
	}
	if v, ok := r.Secret.Get(); ok {
		change.Secret = new(string(v))
	}
	if r.Headers != nil {
		change.Headers = new(webhookHeaders(r.Headers))
	}
	c, err := o.inbox.UpdateWebhook(ctx, string(p.Name), change)
	if err != nil {
		return nil, err
	}
	return webhookResponse(c), nil
}

func webhookHeaders(headers rest.WebhookHeaders) []webhook.Header {
	out := make([]webhook.Header, 0, len(headers))
	for _, h := range headers {
		out = append(out, webhook.Header{Name: string(h.Name), Value: h.Value})
	}
	return out
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
