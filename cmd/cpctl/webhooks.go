package main

import (
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"net/url"
	"os"
	"strconv"
	"strings"
	"text/tabwriter"

	"github.com/savid/clanker-proxy/api/rest"
	"github.com/savid/clanker-proxy/pkg/webhook"
)

func webhookCommands() []*command {
	return []*command{
		{
			name: "webhook add", args: "<name> <url> -secret-file <path>", minArgs: 2, maxArgs: 2,
			summary: "notify an HTTP endpoint about selected events",
			about:   "Requires HTTPS, or HTTP on literal loopback. Generate a signing key with: umask 077; openssl rand -base64 32 > webhook.key. Give that file to your receiver securely; cpctl never prints the key. Defaults: all events, incoming only, enabled. Only future events are queued. The receiver payload and signing contract are in <cpd-url>/openapi.yaml (WebhookPayload).",
			example: "cpctl webhook add agent https://runner.example.com/hooks -secret-file webhook.key -events thread.open,thread.reply -origin incoming", flags: webhookAddFlags,
		},
		{name: "webhook ls", summary: "list configured webhooks", example: "cpctl webhook ls", flags: func(*flag.FlagSet) func(*app, []string) error { return runWebhooks }},
		{name: "webhook show", args: "<name>", minArgs: 1, maxArgs: 1, summary: "show a webhook without its signing key", example: "cpctl webhook show agent", flags: func(*flag.FlagSet) func(*app, []string) error { return runWebhookShow }},
		{
			name: "webhook set", args: "<name> [flags]", minArgs: 1, maxArgs: 1, summary: "change a webhook's settings or pause it",
			about:   "Unspecified settings stay unchanged. Pausing stops new notifications and holds queued ones; their seven-day retry window still expires. Pending deliveries use the current URL and signing key. A request already in flight may finish after a change.",
			example: "cpctl webhook set agent -enabled=false", flags: webhookSetFlags,
		},
		{name: "webhook rm", args: "<name>", minArgs: 1, maxArgs: 1, summary: "delete a webhook and its queued deliveries", about: "Deletes delivery history too. A request already in flight may finish.", example: "cpctl webhook rm agent", flags: func(*flag.FlagSet) func(*app, []string) error { return runWebhookRemove }},
		{
			name: "webhook deliveries", args: "<name>", minArgs: 1, maxArgs: 1, summary: "page through delivery history",
			about:   "2xx succeeds. Connection failures, 408, 429 and 5xx retry with jittered backoff for up to seven days (429/503 respect Retry-After and pause that endpoint); other statuses fail immediately. Completed history is kept for seven days. Delivery order is not guaranteed and retries can duplicate notifications. Use -status failed to find errors and -cursor to read older pages. Inspect lastError, then retry failed deliveries after fixing the receiver.",
			example: "cpctl webhook deliveries agent", flags: webhookDeliveryFlags,
		},
		{name: "webhook retry", args: "<name> <delivery-id>", minArgs: 2, maxArgs: 2, summary: "retry a failed delivery", about: "Requires an enabled webhook and a failed delivery. Preserves its ID and payload; starts a new seven-day retry window.", example: "cpctl webhook retry agent 765a0b0c-0000-4000-8000-000000000001", flags: func(*flag.FlagSet) func(*app, []string) error { return runWebhookRetry }},
		{
			name: "webhook events", summary: "list supported event subscriptions", local: true,
			about:   "Use '*' alone for all current and future types, or comma-separated types for a fixed subscription. Origin is incoming (peer events), outgoing (owner actions), or both. peering.requested is incoming. Replies notify even when the turn stays the same. Only the newest 100 peering-request notifications per endpoint are retained, including unsent ones. Retries and delivery status changes never produce notifications.",
			example: "cpctl webhook events", flags: func(*flag.FlagSet) func(*app, []string) error { return runWebhookEvents },
		},
	}
}

type webhookFlags struct{ url, events, origin, secret, enabled string }

func webhookAddFlags(fs *flag.FlagSet) func(*app, []string) error {
	events := fs.String("events", "*", "comma-separated event `types`, or '*' alone")
	origin := fs.String("origin", "incoming", "incoming, outgoing or both")
	secret := fs.String("secret-file", "", "file containing the base64 signing key")
	return func(a *app, pos []string) error {
		key, err := readWebhookKey(*secret)
		if err != nil {
			return err
		}
		cfg := webhook.Config{Name: pos[0], URL: pos[1], Secret: key, Events: strings.Split(*events, ","), Origin: *origin, Enabled: true}
		if err = cfg.Validate(); err != nil {
			return usageError("%v", err)
		}
		u, _ := url.Parse(cfg.URL)
		hook, err := a.client.CreateWebhook(a.ctx, &rest.WebhookCreate{Name: rest.Name(cfg.Name), URL: rest.DaemonURL(*u), Events: wireWebhookEvents(cfg.Events), Origin: rest.WebhookOrigin(cfg.Origin), Enabled: true, Secret: rest.WebhookSecret(key)})
		if err != nil {
			return err
		}
		return printWebhook(a, hook)
	}
}

func webhookSetFlags(fs *flag.FlagSet) func(*app, []string) error {
	f := &webhookFlags{}
	fs.StringVar(&f.url, "url", "", "new destination URL")
	fs.StringVar(&f.events, "events", "", "comma-separated event types, or '*' alone")
	fs.StringVar(&f.origin, "origin", "", "incoming, outgoing or both")
	fs.StringVar(&f.secret, "secret-file", "", "replace the signing key from this file")
	fs.StringVar(&f.enabled, "enabled", "", "true to enable, false to pause")
	return func(a *app, pos []string) error {
		if *f == (webhookFlags{}) {
			return usageError("provide at least one setting to change")
		}
		return runWebhookSet(a, pos[0], f)
	}
}

func runWebhookSet(a *app, name string, f *webhookFlags) error {
	hook, err := a.client.GetWebhook(a.ctx, rest.GetWebhookParams{Name: rest.Name(name)})
	if err != nil {
		return err
	}
	req := &rest.WebhookUpdate{URL: hook.URL, Events: hook.Events, Origin: hook.Origin, Enabled: hook.Enabled}
	if f.url != "" {
		u, e := url.Parse(f.url)
		if e != nil {
			return usageError("invalid webhook URL")
		}
		req.URL = rest.DaemonURL(*u)
	}
	if f.events != "" {
		req.Events = wireWebhookEvents(strings.Split(f.events, ","))
	}
	if f.origin != "" {
		req.Origin = rest.WebhookOrigin(f.origin)
	}
	if f.enabled != "" {
		enabled, e := strconv.ParseBool(f.enabled)
		if e != nil {
			return usageError("-enabled must be true or false")
		}
		req.Enabled = enabled
	}
	if f.secret != "" {
		key, e := readWebhookKey(f.secret)
		if e != nil {
			return e
		}
		req.Secret = rest.NewOptWebhookSecret(rest.WebhookSecret(key))
	}
	hook, err = a.client.UpdateWebhook(a.ctx, req, rest.UpdateWebhookParams{Name: rest.Name(name)})
	if err != nil {
		return err
	}
	return printWebhook(a, hook)
}

func readWebhookKey(path string) (string, error) {
	if path == "" {
		return "", usageError("-secret-file is required; generate one with: umask 077; openssl rand -base64 32 > webhook.key")
	}
	f, err := os.Open(path)
	if err != nil {
		return "", usageError("cannot open signing-key file")
	}
	defer f.Close()
	data, err := io.ReadAll(io.LimitReader(f, 128))
	if err != nil {
		return "", usageError("cannot read signing-key file")
	}
	key := strings.TrimSpace(string(data))
	if len(key) != 44 {
		return "", usageError("signing-key file must contain the base64 encoding of 32 bytes")
	}
	return key, nil
}

func wireWebhookEvents(events []string) rest.WebhookEvents {
	out := make(rest.WebhookEvents, 0, len(events))
	for _, event := range events {
		out = append(out, rest.WebhookEventsItem(event))
	}
	return out
}

func printWebhook(a *app, h *rest.Webhook) error {
	return a.print(h, func(w io.Writer) {
		fmt.Fprintf(w, "%s → %s (enabled: %t, origin: %s)\nevents: %s\n", h.Name, urlString(h.URL), h.Enabled, h.Origin, joinWebhookEvents(h.Events))
		next(w, step{"cpctl webhook deliveries " + string(h.Name), "inspect delivery status"})
	})
}

func joinWebhookEvents(events rest.WebhookEvents) string {
	parts := make([]string, 0, len(events))
	for _, event := range events {
		parts = append(parts, string(event))
	}
	return strings.Join(parts, ",")
}

func runWebhooks(a *app, _ []string) error {
	hooks, err := a.client.ListWebhooks(a.ctx)
	if err != nil {
		return err
	}
	return a.print(hooks, func(w io.Writer) {
		if len(hooks.Webhooks) == 0 {
			fmt.Fprintln(w, "no webhooks")
		}
		tw := tabwriter.NewWriter(w, 0, 4, 2, ' ', 0)
		for _, h := range hooks.Webhooks {
			fmt.Fprintf(tw, "%s\tenabled: %t\t%s\t%s\t%s\n", h.Name, h.Enabled, h.Origin, joinWebhookEvents(h.Events), urlString(h.URL))
		}
		_ = tw.Flush()
		next(w, step{"cpctl help webhook add", "configure a receiver"}, step{"cpctl webhook events", "see available subscriptions"})
	})
}

func runWebhookShow(a *app, pos []string) error {
	h, err := a.client.GetWebhook(a.ctx, rest.GetWebhookParams{Name: rest.Name(pos[0])})
	if err != nil {
		return err
	}
	return printWebhook(a, h)
}

func runWebhookRemove(a *app, pos []string) error {
	if err := a.client.DeleteWebhook(a.ctx, rest.DeleteWebhookParams{Name: rest.Name(pos[0])}); err != nil {
		return err
	}
	return a.print(json.RawMessage(`{"deleted":true}`), func(w io.Writer) {
		fmt.Fprintln(w, "webhook deleted")
		next(w, step{"cpctl webhook ls", "see remaining webhooks"})
	})
}

func webhookDeliveryFlags(fs *flag.FlagSet) func(*app, []string) error {
	limit := fs.Int("limit", 100, "records per page (1–100)")
	cursor := fs.String("cursor", "", "nextCursor from the preceding page")
	status := fs.String("status", "", "pending, delivered or failed (default: all)")
	return func(a *app, pos []string) error {
		if *limit < 1 || *limit > 100 {
			return usageError("-limit must be between 1 and 100")
		}
		p := rest.ListWebhookDeliveriesParams{Name: rest.Name(pos[0]), Limit: rest.NewOptInt32(int32(*limit))}
		if *cursor != "" {
			p.Cursor = rest.NewOptString(*cursor)
		}
		if *status != "" {
			p.Status = rest.NewOptListWebhookDeliveriesStatus(rest.ListWebhookDeliveriesStatus(*status))
		}
		return runWebhookDeliveries(a, p)
	}
}

func runWebhookDeliveries(a *app, p rest.ListWebhookDeliveriesParams) error {
	ds, err := a.client.ListWebhookDeliveries(a.ctx, p)
	if err != nil {
		return err
	}
	return a.print(ds, func(w io.Writer) {
		if len(ds.Deliveries) == 0 {
			fmt.Fprintln(w, "no deliveries")
		}
		for _, d := range ds.Deliveries {
			fmt.Fprintf(w, "%s  %s  %s  attempts: %d  %s\n", d.ID, d.Event, d.Status, d.Attempts, d.LastError)
		}
		if cursor, ok := ds.NextCursor.Get(); ok {
			command := "cpctl webhook deliveries " + string(p.Name) + " -cursor " + cursor + " -limit " + strconv.Itoa(int(p.Limit.Or(100)))
			if status, filtered := p.Status.Get(); filtered {
				command += " -status " + string(status)
			}
			next(w, step{command, "read older deliveries"})
		}
		next(w, step{"cpctl webhook retry " + string(p.Name) + " <delivery-id>", "retry a failed delivery after fixing its receiver"})
	})
}

func runWebhookRetry(a *app, pos []string) error {
	if err := a.client.RetryWebhookDelivery(a.ctx, rest.RetryWebhookDeliveryParams{Name: rest.Name(pos[0]), ID: rest.ID(pos[1])}); err != nil {
		return err
	}
	return a.print(json.RawMessage(`{"queued":true}`), func(w io.Writer) {
		fmt.Fprintln(w, "delivery queued")
		next(w, step{"cpctl webhook deliveries " + pos[0], "check the next attempt"})
	})
}

func runWebhookEvents(a *app, _ []string) error {
	data, err := json.Marshal(map[string][]string{"events": webhook.Types()})
	if err != nil {
		return err
	}
	return a.print(json.RawMessage(data), func(w io.Writer) {
		for _, event := range webhook.Types() {
			fmt.Fprintln(w, event)
		}
		next(w, step{"cpctl help webhook add", "subscribe a receiver"})
	})
}
