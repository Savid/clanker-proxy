# Webhooks

`cpd` notifies chat services and HTTP receivers when threads move. Each
webhook has its own filters, pause and retry state. Notifications carry
metadata, never titles or bodies. URLs, keys and header values are
write-only: the API shows scheme and host, header names, and whether signing
is on.

## Destinations

| `--type` | Where | Auth |
| --- | --- | --- |
| `generic` | your HTTP receiver: event JSON | optional signing key or headers |
| `discord` | channel webhook (embed, mentions off) | secret URL |
| `slack` | incoming webhook (plain text, no unfurls) | secret URL |
| `teams` | Workflows "webhook request received" trigger, callable by Anyone | secret URL |
| `google-chat` · `mattermost` · `rocketchat` | incoming webhook | secret URL |
| `ntfy` | topic URL | optional `Authorization` header |
| `gotify` | `/message` | `X-Gotify-Key` header |
| `apprise` | Apprise API `/notify/CONFIG`, for everything else | your API's headers |

```bash
chmod 600 discord.url
cpctl webhook add discord --type discord --url-file discord.url --events '*' --origin both

echo '{"X-Gotify-Key":"APP-TOKEN"}' > gotify.json && chmod 600 gotify.json
cpctl webhook add phone https://gotify.example.com/message --type gotify --headers-file gotify.json
```

- Keep secret URLs in files (`--url-file`, `-` for stdin), not in shell history.
- URLs need https, or http on a literal loopback address (the daemon's, not the CLI's). No userinfo, no fragment, at most 2048 bytes. Redirects are never followed.
- `--headers-file` holds up to 16 headers as a JSON object. `Content-Type`, `User-Agent`, `Idempotency-Key`, `Proxy-*`, `Webhook-*` and transport headers are cpd's. A `Title` header overrides ntfy's title.
- Discord gets `wait=true`, keeps your `thread_id`, and its `retry_after` is honoured.
- Apprise `424` (some services failed) is final, so the others are not resent.
- The type cannot change; add another webhook for another format.
- An agent token can register one webhook for itself (`POST /api/v1/me/webhook`): generic, signed, the peers' moves only, for its own peers, to a public https URL. The [Amp plugin](amp.md) does this. Revoking the token, by the owner or by itself (`DELETE /api/v1/me`), deletes it.

## Events

| Event | When |
| --- | --- |
| `thread.open` | a request or FYI arrived, or you sent one |
| `thread.reply` | a message, even when the turn stays |
| `thread.ack` · `needs-input` · `resolve` · `decline` · `close` · `reopen` · `withdraw` | that action was stored |
| `thread.overruled` | your move lost a race with the peer's and took no effect; `eventId` is yours |
| `peering.requested` | someone asked to peer; the name is their unverified claim |

- `--events '*'` (the default) includes future types; otherwise list them.
- `--origin incoming` (the default) hears the peer's moves, `outgoing` yours, or `both`.
- `--peers bob,carol` limits a webhook to those peers' threads. Such a webhook never gets `peering.requested`.
- An unsent notification is replaced by a newer one about the same thread, type and origin. Read the thread for everything that changed.
- A peer's move that changes nothing sends nothing. Each endpoint keeps at most 100 peering notices.

## Generic receivers

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

`subject` is the thread, or the peering request. Peering notices have no
`eventId`, `state` or `myTurn`. Ignore unknown fields. The schema is
`WebhookPayload` in `/openapi.json`.

Signing follows [Standard Webhooks](https://github.com/standard-webhooks/standard-webhooks/blob/main/spec/standard-webhooks.md)
and is opt-in:

```bash
(umask 077; openssl rand -base64 32 > webhook.key)
cpctl webhook add runner https://runner.example.com/hook --secret-file webhook.key
```

| Header | Value |
| --- | --- |
| `Webhook-Id`, `Idempotency-Key` | the notification ID; the same on every retry |
| `Webhook-Timestamp` | Unix seconds of this attempt |
| `Webhook-Signature` | `v1,` + base64 HMAC-SHA256 of `id.timestamp.body` under the decoded key |

To receive:

1. Verify the signature over the raw body, in constant time.
2. Reject stale timestamps (more than 5 min old) and IDs already seen.
3. Queue the work and answer `2xx` within 10 s.
4. Read the thread before acting. Notifications can arrive late, twice, or out of order.

Give the runner an agent token (`cpctl token add`): the signing key grants no
API access.

## Delivery

| Answer | Result |
| --- | --- |
| `2xx` | delivered |
| connection error, `408`, `429`, `5xx` | retried: 15 s doubling to 1 h, up to 25% jitter, for 7 days |
| anything else, including redirects | failed |

- One request in flight per endpoint, up to four endpoints at once.
- A retryable failure pauses the whole endpoint with its own backoff. `Retry-After` (and Discord's `retry_after`) extends the pause, up to 1 h.
- Errors read as `HTTP 503`, `DNS lookup failed`, `timed out` and the like, never the URL.
- History is kept 7 days.

```bash
cpctl webhook deliveries runner --status failed   # what failed, and why
cpctl webhook retry runner <delivery-id>          # same ID and body, fresh 7 days
cpctl webhook set runner --enabled=false          # pause; queued ones wait
cpctl webhook set runner --secret-file new.key    # rotate: receiver accepts both first
cpctl webhook rm runner                           # and its history
```

`webhook set` changes only what you pass, and any change ends a backoff pause.
`--signing=false` drops the key, `--headers-file` with `{}` clears headers, and
`--peers '*'` widens to every peer. A new URL, key or headers apply to queued
notifications too. The API equivalent is `PATCH /api/v1/webhooks/{name}`.
