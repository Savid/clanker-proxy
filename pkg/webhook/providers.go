package webhook

import (
	"encoding/json"
	"errors"
	"fmt"
	"slices"
	"strings"
	"time"
)

// Provider describes a destination for agent-facing discovery and help, and
// how delivery talks to it.
type Provider struct {
	Type        string `json:"type"`
	Description string `json:"description"`
	Auth        string `json:"auth"`
	ExampleURL  string `json:"exampleUrl"`

	// Signed destinations get Webhook-Id and Webhook-Timestamp headers and
	// accept an optional signing key.
	Signed bool `json:"-"`
	// Query parameters are set on every request.
	Query map[string]string `json:"-"`
	// RetryAfterBody means a 429 response carries retry_after, in seconds,
	// in its JSON body.
	RetryAfterBody bool `json:"-"`
}

var providers = []Provider{
	{Type: "generic", Description: "Event JSON for automation; optional Standard Webhooks signing", Auth: "Optional signing key or custom headers", ExampleURL: "https://runner.example.com/hooks/clanker", Signed: true},
	// Discord can acknowledge a non-persisted message unless wait is enabled.
	{Type: "discord", Description: "Discord channel embed; mentions disabled", Auth: "Secret webhook URL", ExampleURL: "https://discord.com/api/webhooks/ID/TOKEN", Query: map[string]string{"wait": "true"}, RetryAfterBody: true},
	{Type: "slack", Description: "Slack incoming webhook with plain-text blocks", Auth: "Secret webhook URL", ExampleURL: "https://hooks.slack.com/services/TEAM/CHANNEL/TOKEN"},
	{Type: "teams", Description: "Teams Workflows Adaptive Card; use a webhook callable by Anyone", Auth: "Secret workflow URL", ExampleURL: "https://WORKFLOW-HOST/WORKFLOW-PATH?sig=TOKEN"},
	{Type: "google-chat", Description: "Google Chat space message", Auth: "Secret webhook URL", ExampleURL: "https://chat.googleapis.com/v1/spaces/SPACE/messages?key=KEY&token=TOKEN"},
	{Type: "mattermost", Description: "Mattermost incoming webhook", Auth: "Secret webhook URL", ExampleURL: "https://mattermost.example.com/hooks/TOKEN"},
	{Type: "rocketchat", Description: "Rocket.Chat incoming integration", Auth: "Secret webhook URL", ExampleURL: "https://chat.example.com/hooks/ID/TOKEN"},
	{Type: "ntfy", Description: "Plain-text notification to a topic URL", Auth: "Optional Authorization header", ExampleURL: "https://ntfy.example.com/TOPIC"},
	{Type: "gotify", Description: "Gotify application message", Auth: "X-Gotify-Key header or token query parameter", ExampleURL: "https://gotify.example.com/message"},
	{Type: "apprise", Description: "Apprise API saved configuration; routes to additional providers", Auth: "Configured API authentication headers", ExampleURL: "https://apprise.example.com/notify/CONFIG"},
}

// Providers is the destination catalog, separate from event Types.
func Providers() []Provider {
	return slices.Clone(providers)
}

// LookupProvider returns the catalog entry for a destination type.
func LookupProvider(kind string) (Provider, bool) {
	i := slices.IndexFunc(providers, func(p Provider) bool { return p.Type == kind })
	if i < 0 {
		return Provider{}, false
	}
	return providers[i], true
}

// Message is a provider's wire payload, without transport or credentials.
type Message struct {
	Body        []byte
	ContentType string
	Headers     []Header
}

// Render formats stored metadata without fetching mutable thread content.
// A retry therefore describes the same event even if the thread has moved on.
func Render(kind string, raw []byte) (Message, error) {
	if kind == "generic" {
		return Message{Body: raw, ContentType: "application/json"}, nil
	}
	var p Payload
	if err := json.Unmarshal(raw, &p); err != nil {
		return Message{}, errors.New("invalid notification payload")
	}
	title, body := summary(p)
	if kind == "ntfy" {
		return Message{Body: []byte(body), ContentType: "text/plain; charset=utf-8", Headers: []Header{{Name: "Title", Value: title}}}, nil
	}
	v, err := providerJSON(kind, p, title, body)
	if err != nil {
		return Message{}, err
	}
	data, err := json.Marshal(v)
	return Message{Body: data, ContentType: "application/json"}, err
}

func providerJSON(kind string, p Payload, title, body string) (any, error) {
	text := title + "\n" + body
	switch kind {
	case "discord":
		return map[string]any{
			"username":         "clanker-proxy",
			"allowed_mentions": map[string]any{"parse": []string{}},
			"embeds": []any{map[string]any{
				"title": title, "description": body, "color": eventColor(p.Type),
				"timestamp": p.At.UTC().Format(time.RFC3339),
				"footer":    map[string]string{"text": "clanker-proxy"},
			}},
		}, nil
	case "slack":
		return map[string]any{
			"text":         text,
			"blocks":       []any{map[string]any{"type": "section", "text": map[string]any{"type": "plain_text", "text": text, "emoji": false}}},
			"unfurl_links": false, "unfurl_media": false,
		}, nil
	case "teams":
		return map[string]any{"type": "message", "attachments": []any{map[string]any{
			"contentType": "application/vnd.microsoft.card.adaptive", "contentUrl": nil,
			"content": map[string]any{
				"$schema": "http://adaptivecards.io/schemas/adaptive-card.json", "type": "AdaptiveCard", "version": "1.2",
				"body": []any{
					map[string]any{"type": "TextBlock", "text": title, "weight": "Bolder", "wrap": true},
					map[string]any{"type": "TextBlock", "text": body, "wrap": true},
				},
			},
		}}}, nil
	case "google-chat", "mattermost", "rocketchat":
		return map[string]string{"text": text}, nil
	case "gotify":
		return map[string]any{"title": title, "message": body, "priority": 5}, nil
	case "apprise":
		return map[string]string{"title": title, "body": body, "type": "info", "format": "text"}, nil
	default:
		return nil, errors.New("unsupported webhook type")
	}
}

var eventLabels = map[string]string{
	"thread.open": "New thread", "thread.reply": "New reply", "thread.ack": "Thread acknowledged",
	"thread.needs-input": "Input requested", "thread.resolve": "Result ready", "thread.decline": "Thread declined",
	"thread.close": "Thread closed", "thread.reopen": "Thread reopened", "thread.withdraw": "Thread withdrawn",
	"peering.requested": "Peering request",
}

// Metadata names are bounded identifiers, but neutralizing mention and markup
// delimiters also keeps imported events from pinging a channel unexpectedly.
var neutralize = strings.NewReplacer("@", "@\u200b", "<", "‹", ">", "›", "&", "＆", "\r", " ", "\n", " ")

func summary(p Payload) (string, string) {
	label := eventLabels[p.Type]
	if label == "" {
		label = "Thread update"
	}
	peer := neutralize.Replace(p.Peer)
	title := "clanker-proxy: " + label
	body := fmt.Sprintf("Event: %s (%s)\nPeer: %s", p.Type, p.Origin, peer)
	if p.Type == "peering.requested" {
		body += " (unverified)\nRequest: " + p.Subject + "\nRead: cpctl requests"
	} else {
		body += "\nState: " + string(p.State)
		if p.MyTurn != nil && *p.MyTurn {
			body += " — your turn"
		}
		body += "\nThread: " + p.Subject + "\nRead: cpctl show " + p.Subject
	}
	body += "\nNotification: " + p.ID
	return title, body
}

func eventColor(event string) int {
	switch event {
	case "thread.close", "thread.resolve":
		return 0x16a34a
	case "thread.needs-input", "peering.requested":
		return 0xd97706
	case "thread.decline", "thread.withdraw":
		return 0x64748b
	default:
		return 0x6366f1
	}
}
