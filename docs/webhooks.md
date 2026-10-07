# Webhook receivers

`cpd` sends signed JSON POST requests to destinations configured by its owner.
Use `cpctl help webhook` for commands. Configuration and history require the
owner token; signing keys are write-only and are never included in responses.

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

## Receiving

Destinations require HTTPS, except literal loopback addresses such as
`http://127.0.0.1:9000/hooks` for a receiver running beside the daemon. A remote
`cpctl` configures the daemon's destination: loopback refers to the daemon's
machine or container, not the CLI's. URL credentials and fragments are refused.
Non-secret query parameters can select a project or route; use the signing key
for authentication. Redirects are never followed.

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
notifications omit `eventId`, `state`, and `myTurn`. `at` is when this daemon
stored the source event; it does not order thread history. The complete schema
is `WebhookPayload` in `/openapi.yaml`.

Signatures follow the [Standard Webhooks signing format](https://github.com/standard-webhooks/standard-webhooks/blob/main/spec/standard-webhooks.md):

- `Webhook-Id`: delivery UUID, also the payload's `id`.
- `Webhook-Timestamp`: Unix seconds at this delivery attempt.
- `Webhook-Signature`: `v1,` followed by a base64 HMAC-SHA256 signature.

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
independent retry state. Transient delivery errors can be retried while later
notifications proceed; endpoint backpressure pauses all its deliveries. Up to
four endpoints send concurrently.

- `2xx`: delivered.
- Connection failures, `408`, `429`, and `5xx`: retry, starting with a 15-second delay
  and doubling to a one-hour base delay, with up to 25% jitter, for up to seven days.
  `429` and `503` pause the endpoint's other deliveries too; valid `Retry-After`
  delays (seconds or HTTP-date) are respected within that retry window.
- Other statuses, including redirects: failed immediately.

Delivery is best effort with persistent retries: it can duplicate, and it can
fail permanently. `cpctl webhook deliveries <name>` shows a page of up to 100
records. Use `-status failed` to find errors and pass the returned `nextCursor`
with `-cursor` to read older pages. Completed and failed records are retained for
seven days. After fixing a receiver, use `cpctl webhook retry <name> <delivery-id>`
to retry a failed record with its original ID and body and a fresh seven-day
retry window.

New endpoints receive only future matching events. Changing event or origin
filters affects future events;
existing queued deliveries remain. Changing URL or signing key affects pending
deliveries too. Omitting `-secret-file` on `webhook set` keeps the existing key.

Pausing with `-enabled=false` stops new notifications and holds pending ones;
their retry window continues to expire. Re-enable with `-enabled=true`.
Removing a webhook deletes its configuration and delivery history. A request
already in flight may finish after changing, pausing, or removing its endpoint.
