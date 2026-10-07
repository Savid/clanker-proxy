package main

import (
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"maps"
	"os"
	"slices"
	"strconv"
	"strings"
	"text/tabwriter"
	"time"

	"github.com/savid/clanker-proxy/api/rest"
	"github.com/savid/clanker-proxy/pkg/webhook"
)

func webhookCommands() []*command {
	return []*command{
		{
			name: "webhook add", args: "<name> [<url>] [-type <type>] [-url-file <path>]", minArgs: 1, maxArgs: 2,
			summary: "send events to a chat service or HTTP receiver",
			about:   "Use cpctl webhook types to choose a destination format. Defaults: generic JSON, unsigned, all events, incoming only, enabled. Supply the URL as an argument or with -url-file (use - for stdin). Provider URLs contain credentials: prefer a private file. Generic signing is opt-in with -secret-file; generate a key with: umask 077; openssl rand -base64 32 > webhook.key. Share it with your receiver securely. -headers-file reads a JSON object of header names and values for authentication. URLs, keys and header values are never printed. Requires HTTPS, or HTTP on literal loopback. Only future events are queued. Chat messages contain metadata, not thread titles or bodies. Notification retries can produce duplicate chat messages.",
			example: "cpctl webhook add discord -type discord -url-file discord.url -events '*' -origin both", flags: webhookAddFlags,
		},
		{name: "webhook ls", summary: "list configured webhooks", example: "cpctl webhook ls", flags: func(*flag.FlagSet) func(*app, []string) error { return runWebhooks }},
		{name: "webhook show", args: "<name>", minArgs: 1, maxArgs: 1, summary: "show a webhook's type, filters and credential status", example: "cpctl webhook show agent", flags: func(*flag.FlagSet) func(*app, []string) error { return runWebhookShow }},
		{
			name: "webhook set", args: "<name>", minArgs: 1, maxArgs: 1, summary: "change a webhook's settings or pause it",
			about:   "Unspecified settings stay unchanged. Type is immutable; create a new destination to change formats. -url-file replaces the private URL, -headers-file replaces all custom headers ({} clears them), -secret-file enables or rotates generic signing, and -signing=false removes the signing key. Pausing stops new notifications and holds queued ones; their seven-day retry window still expires. Pending deliveries use the current URL and credentials. Any change ends the endpoint's failure backoff. A request already in flight may finish after a change. During key rotation, have the receiver accept both keys until in-flight requests finish.",
			example: "cpctl webhook set agent -enabled=false", flags: webhookSetFlags,
		},
		{name: "webhook rm", args: "<name>", minArgs: 1, maxArgs: 1, summary: "delete a webhook and its queued deliveries", about: "Deletes delivery history too. A request already in flight may finish.", example: "cpctl webhook rm agent", flags: func(*flag.FlagSet) func(*app, []string) error { return runWebhookRemove }},
		{
			name: "webhook deliveries", args: "<name>", minArgs: 1, maxArgs: 1, summary: "page through delivery history",
			about:   "2xx succeeds. Connection failures, 408, 429 and 5xx retry with jittered backoff for up to seven days, and pause the whole endpoint with its own backoff; Retry-After is honored up to an hour. Apprise 424 also retries. Other statuses fail immediately. webhook show reports the pause. Completed and failed history is kept for seven days. Delivery order is not guaranteed and retries can duplicate notifications or chat messages. Use -status failed to find errors and -cursor to read older pages. Inspect lastError, then retry failed deliveries after fixing the receiver.",
			example: "cpctl webhook deliveries agent", flags: webhookDeliveryFlags,
		},
		{name: "webhook retry", args: "<name> <delivery-id>", minArgs: 2, maxArgs: 2, summary: "retry a failed delivery", about: "Requires an enabled webhook and a failed delivery. Preserves its ID and event metadata; uses current destination credentials and a fresh seven-day retry window. Ends endpoint backoff.", example: "cpctl webhook retry agent 765a0b0c-0000-4000-8000-000000000001", flags: func(*flag.FlagSet) func(*app, []string) error { return runWebhookRetry }},
		{
			name: "webhook types", summary: "list destination formats and authentication requirements", local: true,
			about:   "Destination types choose the message format. Generic sends event JSON and supports optional signing. Discord, Slack, Teams Workflows, Google Chat, Mattermost and Rocket.Chat use provider webhook URLs. ntfy uses a topic URL; Gotify uses /message with an application token. Apprise API routes to a saved provider configuration. Use -headers-file for authentication headers. All destinations share event/origin filters and persistent retries.",
			example: "cpctl webhook types", flags: func(*flag.FlagSet) func(*app, []string) error { return runWebhookTypes },
		},
		{
			name: "webhook events", summary: "list supported event subscriptions", local: true,
			about:   "Use '*' alone for all current and future types, or comma-separated types for a fixed subscription. Origin is incoming (peer events), outgoing (owner actions), or both. peering.requested is incoming. Replies notify even when the turn stays the same. Only the newest 100 peering-request notifications per endpoint are retained, including unsent ones. Retries and delivery status changes never produce notifications.",
			example: "cpctl webhook events", flags: func(*flag.FlagSet) func(*app, []string) error { return runWebhookEvents },
		},
	}
}

type webhookFlags struct{ url, urlFile, events, origin, secret, enabled, headers, signing string }

func webhookAddFlags(fs *flag.FlagSet) func(*app, []string) error {
	events := fs.String("events", "*", "comma-separated event `types`, or '*' alone")
	origin := fs.String("origin", "incoming", "incoming, outgoing or both")
	secret := fs.String("secret-file", "", "file containing an optional generic signing key")
	kind := fs.String("type", "generic", "destination `type`; see webhook types")
	urlFile := fs.String("url-file", "", "private `file` containing the URL; - reads stdin")
	headersFile := fs.String("headers-file", "", "private JSON `file` of header names and values")
	return func(a *app, pos []string) error {
		destination := ""
		if len(pos) == 2 {
			destination = pos[1]
		}
		destination, err := a.webhookDestination(destination, *urlFile)
		if err != nil {
			return err
		}
		key, err := a.readWebhookKey(*secret)
		if err != nil {
			return err
		}
		headers, err := a.readWebhookHeaders(*headersFile)
		if err != nil {
			return err
		}
		cfg := webhook.Config{Name: pos[0], Type: *kind, URL: destination, Secret: key, Headers: headers, Events: strings.Split(*events, ","), Origin: *origin, Enabled: true}
		if err = cfg.Validate(); err != nil {
			return usageError("%v", err)
		}
		hook, err := a.client.CreateWebhook(a.ctx, &rest.WebhookCreate{Name: rest.Name(cfg.Name), Type: rest.WebhookType(cfg.Type), URL: rest.WebhookURL(cfg.URL), Events: wireWebhookEvents(cfg.Events), Origin: rest.WebhookOrigin(cfg.Origin), Enabled: true, Secret: rest.NewOptWebhookSecret(rest.WebhookSecret(key)), Headers: wireWebhookHeaders(headers)})
		if err != nil {
			return err
		}
		return printWebhook(a, hook)
	}
}

func webhookSetFlags(fs *flag.FlagSet) func(*app, []string) error {
	f := &webhookFlags{}
	fs.StringVar(&f.url, "url", "", "new destination URL")
	fs.StringVar(&f.urlFile, "url-file", "", "private file containing the new URL; - reads stdin")
	fs.StringVar(&f.events, "events", "", "comma-separated event types, or '*' alone")
	fs.StringVar(&f.origin, "origin", "", "incoming, outgoing or both")
	fs.StringVar(&f.secret, "secret-file", "", "enable or rotate generic signing with this key")
	fs.StringVar(&f.enabled, "enabled", "", "true to enable, false to pause")
	fs.StringVar(&f.headers, "headers-file", "", "replace headers from a private JSON object; {} clears")
	fs.StringVar(&f.signing, "signing", "", "false removes the generic signing key; enable with -secret-file")
	return func(a *app, pos []string) error {
		if *f == (webhookFlags{}) {
			return usageError("provide at least one setting to change")
		}
		return runWebhookSet(a, pos[0], f)
	}
}

func runWebhookSet(a *app, name string, f *webhookFlags) error {
	req := &rest.WebhookUpdate{}
	if f.url != "" || f.urlFile != "" {
		u, err := a.webhookDestination(f.url, f.urlFile)
		if err != nil {
			return err
		}
		req.URL = rest.NewOptWebhookURL(rest.WebhookURL(u))
	}
	if f.events != "" {
		req.Events = wireWebhookEvents(strings.Split(f.events, ","))
	}
	if f.origin != "" {
		req.Origin = rest.NewOptWebhookOrigin(rest.WebhookOrigin(f.origin))
	}
	if f.enabled != "" {
		enabled, err := strconv.ParseBool(f.enabled)
		if err != nil {
			return usageError("-enabled must be true or false")
		}
		req.Enabled = rest.NewOptBool(enabled)
	}
	if f.secret != "" {
		key, err := a.readWebhookKey(f.secret)
		if err != nil {
			return err
		}
		req.Secret = rest.NewOptWebhookSecret(rest.WebhookSecret(key))
	}
	if f.signing != "" {
		if f.signing != "false" || f.secret != "" {
			return usageError("use -signing=false to disable signing, or -secret-file to enable it")
		}
		req.Secret = rest.NewOptWebhookSecret("")
	}
	if f.headers != "" {
		headers, err := a.readWebhookHeaders(f.headers)
		if err != nil {
			return err
		}
		req.Headers = wireWebhookHeaders(headers)
	}
	hook, err := a.client.UpdateWebhook(a.ctx, req, rest.UpdateWebhookParams{Name: rest.Name(name)})
	if err != nil {
		return err
	}
	return printWebhook(a, hook)
}

func (a *app) readWebhookKey(path string) (string, error) {
	if path == "" {
		return "", nil
	}
	data, err := a.readWebhookFile(path, "signing key", 128)
	if err != nil {
		return "", err
	}
	key := strings.TrimSpace(string(data))
	if len(key) != 44 {
		return "", usageError("signing-key file must contain the base64 encoding of 32 bytes")
	}
	return key, nil
}

func (a *app) readWebhookFile(path, label string, limit int64) ([]byte, error) {
	reader := a.stdin
	if path != "-" {
		f, err := os.Open(path)
		if err != nil {
			return nil, usageError("cannot open %s file", label)
		}
		defer f.Close()
		reader = f
	}
	data, err := io.ReadAll(io.LimitReader(reader, limit+1))
	if err != nil || int64(len(data)) > limit {
		return nil, usageError("cannot read %s file (maximum %d bytes)", label, limit)
	}
	return data, nil
}

func (a *app) webhookDestination(value, file string) (string, error) {
	if (value == "") == (file == "") {
		return "", usageError("provide exactly one destination URL or -url-file")
	}
	if file != "" {
		data, err := a.readWebhookFile(file, "destination URL", 2050)
		if err != nil {
			return "", err
		}
		value = strings.TrimSpace(string(data))
	}
	if value == "" {
		return "", usageError("destination URL file is empty")
	}
	return value, nil
}

func (a *app) readWebhookHeaders(file string) ([]webhook.Header, error) {
	if file == "" {
		return nil, nil
	}
	data, err := a.readWebhookFile(file, "headers", 128*1024)
	if err != nil {
		return nil, err
	}
	var values map[string]string
	if err = json.Unmarshal(data, &values); err != nil || values == nil {
		return nil, usageError("headers file must contain a JSON object of header names and string values")
	}
	out := make([]webhook.Header, 0, len(values))
	for _, name := range slices.Sorted(maps.Keys(values)) {
		out = append(out, webhook.Header{Name: name, Value: values[name]})
	}
	return out, nil
}

func wireWebhookHeaders(headers []webhook.Header) rest.WebhookHeaders {
	out := make(rest.WebhookHeaders, 0, len(headers))
	for _, h := range headers {
		out = append(out, rest.WebhookHeadersItem{Name: rest.WebhookHeaderName(h.Name), Value: h.Value})
	}
	return out
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
		fmt.Fprintf(w, "%s → %s (%s, enabled: %t, origin: %s, signing: %t)\nevents: %s\n", h.Name, h.Destination, h.Type, h.Enabled, h.Origin, h.Signing, joinWebhookEvents(h.Events))
		if len(h.HeaderNames) > 0 {
			fmt.Fprintf(w, "headers: %v (values hidden)\n", h.HeaderNames)
		}
		steps := []step{{"cpctl webhook deliveries " + string(h.Name), "inspect delivery status"}}
		if until, ok := h.PausedUntil.Get(); ok {
			fmt.Fprintf(w, "paused until %s after failed attempts\n", until.UTC().Format(time.RFC3339))
			steps = append(steps, step{"cpctl webhook set " + string(h.Name) + " -enabled=true", "resume now, once the receiver is fixed"})
		}
		next(w, steps...)
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
			fmt.Fprintf(tw, "%s\t%s\tenabled: %t\t%s\t%s\t%s\n", h.Name, h.Type, h.Enabled, h.Origin, joinWebhookEvents(h.Events), h.Destination)
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
	if !a.json {
		fmt.Fprintf(a.stdout, "deleted webhook %s and its delivery history\n", pos[0])
		next(a.stdout, step{"cpctl webhook ls", "see remaining webhooks"})
	}
	return nil
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
		tw := tabwriter.NewWriter(w, 0, 4, 2, ' ', 0)
		failed := false
		for _, d := range ds.Deliveries {
			detail := d.LastError
			if at, ok := d.NextAttemptAt.Get(); ok {
				detail = strings.TrimSpace("next attempt " + at.UTC().Format(time.RFC3339) + "  " + detail)
			}
			failed = failed || d.Status == rest.WebhookDeliveryStatusFailed
			fmt.Fprintf(tw, "%s\t%s\t%s\t%s\t%s\tattempts: %d\t%s\n", d.ID, d.CreatedAt.UTC().Format(time.RFC3339), d.Event, d.Subject, d.Status, d.Attempts, detail)
		}
		_ = tw.Flush()
		var steps []step
		if cursor, ok := ds.NextCursor.Get(); ok {
			command := "cpctl webhook deliveries " + string(p.Name) + " -cursor " + cursor + " -limit " + strconv.Itoa(int(p.Limit.Or(100)))
			if status, filtered := p.Status.Get(); filtered {
				command += " -status " + string(status)
			}
			steps = append(steps, step{command, "read older deliveries"})
		}
		switch {
		case failed:
			steps = append(steps, step{"cpctl webhook retry " + string(p.Name) + " <delivery-id>", "retry a failed delivery after fixing its receiver"})
		case !p.Status.Set:
			steps = append(steps, step{"cpctl webhook deliveries " + string(p.Name) + " -status failed", "list only failures"})
		}
		steps = append(steps, step{"cpctl webhook show " + string(p.Name), "check whether the endpoint is paused"})
		next(w, steps...)
	})
}

func runWebhookRetry(a *app, pos []string) error {
	if err := a.client.RetryWebhookDelivery(a.ctx, rest.RetryWebhookDeliveryParams{Name: rest.Name(pos[0]), ID: rest.ID(pos[1])}); err != nil {
		return err
	}
	if !a.json {
		fmt.Fprintf(a.stdout, "delivery %s queued\n", pos[1])
		next(a.stdout, step{"cpctl webhook deliveries " + pos[0], "check the next attempt"})
	}
	return nil
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

func runWebhookTypes(a *app, _ []string) error {
	providers := webhook.Providers()
	data, err := json.Marshal(map[string][]webhook.Provider{"types": providers})
	if err != nil {
		return err
	}
	return a.print(json.RawMessage(data), func(w io.Writer) {
		for _, p := range providers {
			fmt.Fprintf(w, "%s: %s\n  auth: %s\n  URL: %s\n", p.Type, p.Description, p.Auth, p.ExampleURL)
		}
		next(w, step{"cpctl help webhook add", "configure a destination"}, step{"cpctl webhook events", "choose event subscriptions"})
	})
}
