package webhook

import (
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"time"
)

// Provider describes a destination for agent-facing discovery and help.
type Provider struct {
	Type        string `json:"type"`
	Description string `json:"description"`
	Auth        string `json:"auth"`
	ExampleURL  string `json:"exampleUrl"`
}

// Providers is the destination catalog, separate from event Types.
func Providers() []Provider {
	return []Provider{
		{"generic", "Event JSON for automation; optional Standard Webhooks signing", "Optional signing key or custom headers", "https://runner.example.com/hooks/clanker"},
		{"discord", "Discord channel embed; mentions disabled", "Secret webhook URL", "https://discord.com/api/webhooks/ID/TOKEN"},
		{"slack", "Slack incoming webhook with plain-text blocks", "Secret webhook URL", "https://hooks.slack.com/services/TEAM/CHANNEL/TOKEN"},
		{"teams", "Teams Workflows Adaptive Card; use a webhook callable by Anyone", "Secret workflow URL", "https://WORKFLOW-HOST/WORKFLOW-PATH?sig=TOKEN"},
		{"google-chat", "Google Chat space message", "Secret webhook URL", "https://chat.googleapis.com/v1/spaces/SPACE/messages?key=KEY&token=TOKEN"},
		{"mattermost", "Mattermost incoming webhook", "Secret webhook URL", "https://mattermost.example.com/hooks/TOKEN"},
		{"rocketchat", "Rocket.Chat incoming integration", "Secret webhook URL", "https://chat.example.com/hooks/ID/TOKEN"},
		{"ntfy", "Plain-text notification to a topic URL", "Optional Authorization header", "https://ntfy.example.com/TOPIC"},
		{"gotify", "Gotify application message", "X-Gotify-Key header or token query parameter", "https://gotify.example.com/message"},
		{"apprise", "Apprise API saved configuration; routes to additional providers", "Configured API authentication headers", "https://apprise.example.com/notify/CONFIG"},
	}
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

func summary(p Payload) (string, string) {
	labels := map[string]string{
		"thread.open": "New thread", "thread.reply": "New reply", "thread.ack": "Thread acknowledged",
		"thread.needs-input": "Input requested", "thread.resolve": "Result ready", "thread.decline": "Thread declined",
		"thread.close": "Thread closed", "thread.reopen": "Thread reopened", "thread.withdraw": "Thread withdrawn",
		"peering.requested": "Peering request",
	}
	label := labels[p.Type]
	if label == "" {
		label = "Thread update"
	}
	// Metadata names are bounded identifiers, but neutralizing mention and markup
	// delimiters also keeps imported events from pinging a channel unexpectedly.
	clean := strings.NewReplacer("@", "@\u200b", "<", "‹", ">", "›", "&", "＆", "\r", " ", "\n", " ")
	peer := clean.Replace(p.Peer)
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
