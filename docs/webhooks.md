# Webhook destinations

`cpd` sends event notifications to chat services, push services, and HTTP
receivers. Use `cpctl webhook types` to choose a destination and
`cpctl help webhook` for commands. Configuration and history require the owner
token. Destination URLs, signing keys, and custom header values are write-only;
responses show only the URL's scheme and host, header names, and signing status.

## Choose a destination

| `-type` | Destination | Authentication | Message format |
| --- | --- | --- | --- |
| `generic` (default) | Your HTTP receiver | Optional signing key or custom headers | Event JSON |
| `discord` | Discord channel webhook | Secret webhook URL | Embed, with mentions disabled |
| `slack` | Slack incoming webhook | Secret webhook URL | Plain-text block and fallback text |
| `teams` | Teams Workflows webhook | Secret workflow URL, callable by Anyone | Adaptive Card |
| `google-chat` | Google Chat space webhook | Key and token in the URL | Text message |
| `mattermost` | Mattermost incoming webhook | Secret webhook URL | Text message |
| `rocketchat` | Rocket.Chat incoming integration | Secret webhook URL | Text message |
| `ntfy` | Topic URL, including on your own server | Optional `Authorization` header | Plain-text POST with a title |
| `gotify` | Your server's `/message` endpoint | Application token in `X-Gotify-Key` | Title, message, priority 5 |
| `apprise` | Apprise API saved configuration | Your API's authentication headers | Title and text body |

All types share event filters, origin filters, pausing, delivery history, and
retries. Type is immutable; create another destination to change message format.
Generic signing is **opt-in**. Chat providers do not use a clanker-proxy signing
key: their URL or authentication headers authorize posting.

### Discord, Slack, and other chat services

Create an incoming webhook in the provider and save the full URL in a private
file, such as `discord.url`. The file is read by `cpctl` on the machine where
you run the command. Avoid pasting secret URLs into shell history.

```bash
chmod 600 discord.url
cpctl webhook add discord -type discord -url-file discord.url \
  -events '*' -origin both
cpctl webhook show discord
cpctl webhook deliveries discord
```

For Slack use `-type slack` with its URL; likewise for `teams`, `google-chat`,
`mattermost`, and `rocketchat`. `-url-file -` reads a URL from stdin. A positional
URL is also accepted, which is convenient for receivers without URL secrets.

Chat messages include the event type, peer, origin, state, whose turn it is,
thread/request ID, and notification ID. They include `cpctl show <subject>` for
threads or `cpctl requests` for peering. Peering names are labeled unverified.
Titles and message bodies are never included. An event reports state when it
was stored; read the current thread before acting. Chat notifications do not
launch agents or accept replies back into the thread.

Provider details:

- [Discord](https://docs.discord.com/developers/resources/webhook#execute-webhook):
  delivery uses `wait=true` so Discord confirms persistence. A `thread_id`
  query parameter in the configured URL is retained for an existing thread.
  Forum/media channels need an existing thread ID; automatic thread creation
  is not supported. Discord's `429` JSON `retry_after` is honored as well as
  the `Retry-After` header. Cooldowns are per named destination; a Discord
  global rate limit does not pause your other configured Discord destinations.
- [Slack](https://docs.slack.dev/messaging/sending-messages-using-incoming-webhooks/):
  use an incoming webhook for the desired channel. Messages use plain-text
  blocks, suppress link unfurling, and neutralize mention syntax.
- [Teams Workflows](https://learn.microsoft.com/en-us/connectors/teams/):
  use the “When a Teams webhook request is received” trigger and an action
  that posts the received Adaptive Cards. Choose “Anyone” for who can trigger
  it. Tenant-restricted flows require a separate OAuth integration.
- [Google Chat](https://developers.google.com/workspace/chat/quickstart/webhooks),
  [Mattermost](https://docs.mattermost.com/developers/integrate/webhooks/incoming),
  and [Rocket.Chat](https://docs.rocket.chat/docs/integrations): use the incoming
  webhook/integration URL copied from the destination space or channel.

### Authentication headers, ntfy, and Gotify

`-headers-file` reads a private JSON object. For a protected ntfy topic:

```json
{"Authorization":"Bearer YOUR-NTFY-ACCESS-TOKEN"}
```

```bash
chmod 600 ntfy-headers.json
cpctl webhook add phone https://ntfy.example.com/clanker \
  -type ntfy -headers-file ntfy-headers.json -events '*' -origin both
```

[ntfy](https://docs.ntfy.sh/publish/) receives a plain-text POST at the **topic
URL**. Omit `-headers-file` for a topic that does not require authentication.

For [Gotify](https://gotify.net/docs/pushmsg), put the application token in
`gotify-headers.json` as `{"X-Gotify-Key":"YOUR-APPLICATION-TOKEN"}`, then:

```bash
chmod 600 gotify-headers.json
cpctl webhook add phone https://gotify.example.com/message \
  -type gotify -headers-file gotify-headers.json -events '*' -origin both
```

Headers also support custom receivers and reverse proxies. There can be at most
16, with case-insensitively unique names (up to 64 bytes) and single-line values
(up to 4096 bytes). Transport, `Content-Type`, `User-Agent`, `Proxy-*`, and
`Webhook-*` and `Idempotency-Key` headers are managed by `cpd` and cannot be overridden. For ntfy,
a `Title` header replaces the generated title. Header files and URLs are stored in the daemon's
private database; they are not reread from the CLI machine on delivery.

### Apprise for additional services

[Apprise API](https://github.com/caronc/apprise-api) can route notifications to
services configured in Apprise, including destinations outside the built-in
webhook types. Run it separately and configure its destinations there, then
point `cpd` at a saved configuration's `/notify/CONFIG` endpoint:

```bash
cpctl webhook add notifications -type apprise -url-file apprise.url \
  -headers-file apprise-headers.json -events '*' -origin both
```

Use `Authorization` in the headers file if your Apprise API requires it; omit
the file otherwise. Configure the endpoint to return its normal synchronous
HTTP result, without streaming. A `424` means at least one of its services
failed while others may have succeeded, so `cpd` marks the delivery failed
rather than resending it to every service. After fixing the failing service,
`cpctl webhook retry` resends it to all of them. Separate configurations and
named webhooks give each destination its own retry state.

## Event subscriptions

Each endpoint subscribes to `*` (all current and future types) or a fixed list:

| Type | Meaning |
| --- | --- |
| `thread.open` | A new request or FYI |
| `thread.reply` | A message, even when the turn stays the same |
| `thread.ack` | The recipient acknowledged the thread |
| `thread.needs-input` | The recipient asked a question |
| `thread.resolve` | The recipient submitted a result |
| `thread.decline` | The recipient declined |
| `thread.close` | The sender closed the thread |
| `thread.reopen` | The sender reopened it |
| `thread.withdraw` | The sender withdrew it |
| `peering.requested` | A new incoming peering request was stored |

Origin is `incoming`, `outgoing`, or `both`, relative to the daemon's owner.
The default is incoming. Peering requests are always incoming. A thread event
means the action was stored; concurrent events can leave an action unapplied
by the workflow. Fetch the current thread and its available actions before
acting. Transport retries and delivery-status changes generate no notifications.

Public peering requests are bounded separately: each endpoint retains only its
newest 100 peering-request notifications, including pending deliveries. Older
ones can be dropped during a flood, just as old requests leave the bounded
pending-request inbox. Receivers should read `cpctl requests` for current work.
Thread notifications are unaffected by this limit.

## Destination access

Destinations require HTTPS, except literal loopback addresses such as
`http://127.0.0.1:9000/hooks` for a receiver running beside the daemon. A remote
`cpctl` configures the daemon's destination: loopback refers to the daemon's
machine or container, not the CLI's. URL userinfo (`user:password@host`) and
fragments are refused. Paths and query parameters may carry provider tokens;
the entire URL is treated as a credential and is never echoed. URLs are bounded
to 2048 bytes. Redirects are never followed. Any host the daemon can reach
is allowed, including private addresses over HTTPS, so treat webhook
configuration as owner-only access to the daemon's network.

## Generic receivers and optional signing

Unsigned event JSON needs no key:

```bash
cpctl webhook add runner https://runner.example.com/hooks/clanker \
  -type generic -events '*' -origin incoming
```

To authenticate with signatures, generate a signing key and securely configure
the same key on your receiver:

```bash
umask 077
openssl rand -base64 32 > webhook.key
cpctl webhook set runner -secret-file webhook.key
```

Passing `-secret-file` when adding a generic webhook also enables signing.
`cpctl webhook set runner -signing=false` removes the key and disables signing.
Unsigned delivery provides no proof of sender; use the receiver's authentication
headers or enable signing if it needs to authenticate `cpd`.

The notification contains metadata, without message bodies, titles, owner
tokens, peer secrets, or signing keys:

```json
{
  "id": "34ad778a-a496-4c8a-ad5e-60a778d62c02",
  "type": "thread.open",
  "origin": "incoming",
  "at": "2026-10-07T10:00:00Z",
  "subject": "58116f0c-e603-4611-83ae-a0eac5e43c75",
  "eventId": "58116f0c-e603-4611-83ae-a0eac5e43c75",
  "peer": "bob",
  "state": "open",
  "myTurn": true
}
```

`subject` identifies the thread, or the pending peering request. Peering
notifications omit `eventId`, `state`, and `myTurn`, and their `peer` is the
name the unauthenticated requester asked for, which may match an existing peer;
check `cpctl requests` before trusting it. `at` is when this daemon stored the
source event; it does not order thread history. Ignore unknown fields, and
unknown types when subscribed to `*`. The complete schema is `WebhookPayload`
in `/openapi.yaml`.

Generic notifications always include the ID and timestamp headers. When a key
is configured they also include a signature following the
[Standard Webhooks signing format](https://github.com/standard-webhooks/standard-webhooks/blob/main/spec/standard-webhooks.md):

- `Webhook-Id`: delivery UUID, also the payload's `id`.
- `Idempotency-Key`: the same UUID, for intermediaries such as Amp that
  deduplicate retries by this header.
- `Webhook-Timestamp`: Unix seconds at this delivery attempt.
- `Webhook-Signature`: present only when signing is enabled; `v1,` followed by a base64 HMAC-SHA256 signature.

Base64-decode the configured key to obtain 32 key bytes. The signed content is
`Webhook-Id + "." + Webhook-Timestamp + "." + rawRequestBody`. Preserve the raw
body bytes, compare signatures in constant time, reject stale timestamps
(for example, outside five minutes), and persist processed IDs to reject
duplicates. Retries keep the ID and body but receive a fresh timestamp and
signature. Each webhook receives its own delivery ID for the same source event.

Return `2xx` promptly after durably queuing the notification. Do agent work
asynchronously; the HTTP request has a ten-second timeout. A runner should
serialize work per thread and reread its current state, since notifications
can arrive late, more than once, or out of order. The runner uses its separately
configured owner credential for `cpctl`; the webhook signing key grants no API access.

## Delivery and management

Notifications are queued atomically with their source event. Each endpoint has
independent retry state, and sends one request at a time; up to four endpoints
send concurrently.

- `2xx`: delivered.
- Connection failures, `408`, `429`, and `5xx`: retry, starting with a 15-second delay
  and doubling to a one-hour base delay, with up to 25% jitter, for up to seven days.
- Other statuses, including redirects: failed immediately. This includes Apprise
  `424`, since some of its services may already have received the message.

A retryable failure also pauses the whole endpoint, with its own backoff on
consecutive failures (the same schedule), so a dead receiver is probed by one
delivery at a time; the first success resets it. A valid `Retry-After` on `429`
or `503` (seconds or HTTP-date) extends the pause, up to one hour. Discord's
`retry_after` JSON field on `429` is rounded up to whole seconds and uses the
same cap. When both are present the later time wins.
`cpctl webhook show <name>` reports `paused until`; any `webhook set`, such as
`-enabled=true`, or a `webhook retry` ends the pause at once. `lastError` names
the failure without the URL or response body: `HTTP <status>`,
`DNS lookup failed`, `TLS certificate rejected`, `TLS handshake failed`,
`timed out`, `connection refused`, or `connection failed`.

Delivery is best effort with persistent retries: it can duplicate, and it can
fail permanently. `cpctl webhook deliveries <name>` shows a page of up to 100
records. Use `-status failed` to find errors and pass the returned `nextCursor`
with `-cursor` to read older pages. Completed and failed records are retained for
seven days. After fixing a receiver, use `cpctl webhook retry <name> <delivery-id>`
to retry a failed record with its original ID and body and a fresh seven-day
retry window.

New endpoints receive only future matching events. Changing event or origin
filters affects future events; existing queued deliveries remain. Changing URL,
custom headers, or signing key affects pending deliveries too. Omitted settings
are retained. Use `-url-file` to replace the URL and `-headers-file` to replace
all headers; an empty JSON object clears headers. Omitting `-secret-file` keeps
the existing key. Signed generic requests carry one signature made with the
current key, so to rotate it
have the receiver accept both the old and new keys, then run
`webhook set <name> -secret-file new.key`, then retire the old key.

Pausing with `-enabled=false` stops new notifications and holds pending ones;
their retry window continues to expire. Re-enable with `-enabled=true`.
Removing a webhook deletes its configuration and delivery history. A request
already in flight may finish after changing, pausing, or removing its endpoint.

The API uses `POST /api/v1/webhooks` to create and
`PATCH /api/v1/webhooks/{name}` for partial updates. In PATCH requests, omit
`url`, `secret`, or `headers` to retain them; `secret: ""` disables signing and
`headers: []` clears headers. Updates merge and validate atomically. Unknown
fields, including an attempted `type` change, are refused. Read responses include `type`, `destination`
(scheme and host only), `signing`, and `headerNames`, alongside filters and
delivery backoff status. The full contract is in `/openapi.yaml`.
