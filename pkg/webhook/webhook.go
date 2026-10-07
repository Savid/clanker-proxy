// Package webhook defines notification filters, payloads and signatures.
package webhook

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"errors"
	"net/netip"
	"net/textproto"
	"net/url"
	"slices"
	"strings"
	"time"

	"github.com/savid/clanker-proxy/pkg/thread"
)

// Types is the event catalog. A wildcard also subscribes to future types.
func Types() []string {
	return []string{"thread.open", "thread.reply", "thread.ack", "thread.needs-input", "thread.resolve", "thread.decline", "thread.close", "thread.reopen", "thread.withdraw", "peering.requested"}
}

// Config describes one owner-controlled destination. URL, Secret and header
// values are credentials and must never appear in API responses or logs.
type Config struct {
	Name    string
	Type    string
	URL     string
	Events  []string
	Origin  string
	Enabled bool
	Secret  string
	Headers []Header

	// PausedUntil and Failures are endpoint backoff kept by delivery; owner
	// updates reset them, and Validate ignores them.
	PausedUntil time.Time
	Failures    int
}

// Header is an owner-supplied header; names are public and values are secret.
type Header struct {
	Name  string `json:"name"`
	Value string `json:"value"`
}

// Update changes only supplied fields. Type is immutable so queued events
// cannot silently switch to another provider's message format.
type Update struct {
	URL     *string
	Events  []string
	Origin  *string
	Enabled *bool
	Secret  *string
	Headers *[]Header
}

// Apply merges a partial update before the resulting configuration is validated.
func (u Update) Apply(c Config) Config {
	if u.URL != nil {
		c.URL = *u.URL
	}
	if u.Events != nil {
		c.Events = u.Events
	}
	if u.Origin != nil {
		c.Origin = *u.Origin
	}
	if u.Enabled != nil {
		c.Enabled = *u.Enabled
	}
	if u.Secret != nil {
		c.Secret = *u.Secret
	}
	if u.Headers != nil {
		c.Headers = *u.Headers
	}
	return c
}

// Validate checks the same constraints for HTTP and in-process callers.
func (c Config) Validate() error {
	if n, ok := thread.NormalizeName(c.Name); !ok || n != c.Name {
		return errors.New("invalid webhook name")
	}
	if !slices.ContainsFunc(Providers(), func(p Provider) bool { return p.Type == c.Type }) {
		return errors.New("unsupported webhook type; see cpctl webhook types")
	}
	u, err := url.Parse(c.URL)
	if err != nil || len(c.URL) > 2048 || u.Hostname() == "" || u.User != nil || strings.Contains(c.URL, "#") || u.Opaque != "" {
		return errors.New("webhook URL must be an absolute URL without credentials or fragment")
	}
	ip, _ := netip.ParseAddr(u.Hostname())
	if u.Scheme != "https" && (u.Scheme != "http" || !ip.IsLoopback()) {
		return errors.New("webhook URL requires HTTPS, or HTTP on a literal loopback address")
	}
	if c.Origin != "incoming" && c.Origin != "outgoing" && c.Origin != "both" {
		return errors.New("webhook origin must be incoming, outgoing or both")
	}
	if len(c.Events) == 0 || len(c.Events) > len(Types()) {
		return errors.New("select '*' or a nonempty list of webhook event types")
	}
	for i, event := range c.Events {
		if event == "*" && len(c.Events) == 1 {
			continue
		}
		if !slices.Contains(Types(), event) || slices.Contains(c.Events[:i], event) {
			return errors.New("invalid or repeated webhook event; use '*' alone or supported event types")
		}
	}
	if err = c.validateSigning(); err != nil {
		return err
	}
	return validateHeaders(c.Headers)
}

func (c Config) validateSigning() error {
	if c.Secret == "" {
		return nil
	}
	if c.Type != "generic" {
		return errors.New("signing keys apply only to generic webhooks; provider destinations use their URL or authentication headers")
	}
	key, err := base64.StdEncoding.Strict().DecodeString(c.Secret)
	if err != nil || len(key) != 32 || len(c.Secret) != 44 {
		return errors.New("webhook secret must be the base64 encoding of exactly 32 bytes")
	}
	return nil
}

func validateHeaders(headers []Header) error {
	if len(headers) > 16 {
		return errors.New("at most 16 webhook headers are allowed")
	}
	seen := map[string]bool{}
	for _, h := range headers {
		name := strings.ToLower(h.Name)
		if len(name) == 0 || len(name) > 64 || strings.ContainsFunc(name, func(r rune) bool {
			return (r < 'a' || r > 'z') && (r < '0' || r > '9') && !strings.ContainsRune("!#$%&'*+-.^_`|~", r)
		}) {
			return errors.New("invalid webhook header name")
		}
		if seen[name] {
			return errors.New("webhook header names must be unique ignoring case")
		}
		seen[name] = true
		if reservedHeader(name) {
			return errors.New("transport, content-type, user-agent and webhook headers are managed by the daemon")
		}
		if len(h.Value) == 0 || len(h.Value) > 4096 || strings.ContainsFunc(h.Value, func(r rune) bool { return r < 32 || r == 127 }) {
			return errors.New("webhook header values must be 1 to 4096 bytes without control characters")
		}
	}
	return nil
}

func reservedHeader(name string) bool {
	return strings.HasPrefix(name, "webhook-") || strings.HasPrefix(name, "proxy-") ||
		slices.Contains([]string{"host", "content-length", "content-type", "connection", "transfer-encoding", "trailer", "te", "upgrade", "expect", "user-agent"}, name)
}

// Destination omits paths and queries, which may contain provider credentials.
func (c Config) Destination() string {
	u, err := url.Parse(c.URL)
	if err != nil {
		return ""
	}
	return u.Scheme + "://" + u.Host
}

// HeaderNames returns the public names without their values.
func (c Config) HeaderNames() []string {
	names := make([]string, 0, len(c.Headers))
	for _, h := range c.Headers {
		names = append(names, textproto.CanonicalMIMEHeaderKey(h.Name))
	}
	slices.Sort(names)
	return names
}

// Payload deliberately omits bodies and credentials; consumers fetch current state.
type Payload struct {
	ID      string       `json:"id"`
	Type    string       `json:"type"`
	Origin  string       `json:"origin"`
	At      time.Time    `json:"at"`
	Subject string       `json:"subject"`
	EventID string       `json:"eventId,omitempty"`
	Peer    string       `json:"peer"`
	State   thread.State `json:"state,omitempty"`
	MyTurn  *bool        `json:"myTurn,omitempty"`
}

// ThreadType maps the stored comment action to the CLI's reply vocabulary.
func ThreadType(action thread.Action) string {
	if action == thread.ActionComment {
		return "thread.reply"
	}
	return "thread." + string(action)
}

// Signature uses the Standard Webhooks v1 signed content and header format.
// The caller supplies the attempt's timestamp; retries keep the delivery ID.
func Signature(secret, id, timestamp string, body []byte) (string, error) {
	key, err := base64.StdEncoding.Strict().DecodeString(secret)
	if err != nil || len(key) != 32 {
		return "", errors.New("invalid webhook signing key")
	}
	mac := hmac.New(sha256.New, key)
	_, _ = mac.Write([]byte(strings.Join([]string{id, timestamp, ""}, ".")))
	_, _ = mac.Write(body)
	return "v1," + base64.StdEncoding.EncodeToString(mac.Sum(nil)), nil
}
