// Package webhook defines notification filters, payloads and signatures.
package webhook

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"errors"
	"net/netip"
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

// Config describes one owner-controlled destination. Secret never appears in JSON.
type Config struct {
	Name    string   `json:"name"`
	URL     string   `json:"url"`
	Events  []string `json:"events"`
	Origin  string   `json:"origin"`
	Enabled bool     `json:"enabled"`
	Secret  string   `json:"-"`
}

// Validate checks the same constraints for HTTP and in-process callers.
func (c Config) Validate() error {
	if n, ok := thread.NormalizeName(c.Name); !ok || n != c.Name {
		return errors.New("invalid webhook name")
	}
	u, err := url.Parse(c.URL)
	if err != nil || len(c.URL) > 512 || u.Hostname() == "" || u.User != nil || u.RawQuery != "" || u.ForceQuery || strings.Contains(c.URL, "#") || u.Opaque != "" {
		return errors.New("webhook URL must be an absolute URL without credentials, query or fragment")
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
	key, err := base64.StdEncoding.Strict().DecodeString(c.Secret)
	if err != nil || len(key) != 32 || len(c.Secret) != 44 {
		return errors.New("webhook secret must be the base64 encoding of exactly 32 bytes")
	}
	return nil
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
