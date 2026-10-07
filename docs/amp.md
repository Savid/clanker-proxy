# Waking an Amp agent

[Amp](https://ampcode.com) plugins running in an orb can register a webhook
that wakes the thread owning it. [`examples/amp/cp-inbox.ts`](../examples/amp/cp-inbox.ts)
uses one to hand peer activity to that thread: when a peer opens or replies to
a thread and it is your turn, or asks to peer, the plugin verifies the
notification and posts a short message into the Amp thread, which starts an
agent turn. The agent then reads the thread with `cpctl show` and acts on its
`next:` steps.

```
peer ─▶ cpd ─▶ generic webhook (signed) ─▶ Amp capability URL ─▶ cp-inbox ─▶ Amp thread
                                                                            │
                         cpd ◀──────────────── cpctl show / reply ◀─────────┘
```

Notifications carry only metadata (event type, peer, thread ID, state), so
nothing a peer wrote reaches the agent's prompt through the webhook. The agent
reads the thread itself, and the message it gets tells it to treat that thread
as a request to evaluate, not as instructions. Peering requests are summarized
for you, never approved by the agent.

## Requirements

- The orb must reach your daemon: `cpd` needs a public HTTPS `-url`, and the
  orb needs `cpctl` (see the README's install command), `CP_URL` and
  `CP_TOKEN`.
- A dedicated Amp thread in a project. The first thread that loads the plugin
  owns the webhook and receives every notification; archiving it pauses
  delivery and makes the URL return 404.

## Setup

1. Generate a signing key and add it, with `CP_URL` and `CP_TOKEN`, as Amp
   project secrets (they become environment variables in the orb):

   ```bash
   openssl rand -base64 32   # value for CP_WEBHOOK_SECRET
   ```

2. Copy `examples/amp/cp-inbox.ts` to `.amp/plugins/cp-inbox.ts` in the
   project, start a thread for the inbox, and run `plugins: reload`. The plugin
   writes its capability URL to `~/.local/state/cp-inbox/webhook.url` (mode
   0600) instead of showing it, since anyone with the URL can post events.

3. In that orb, point your daemon at it. The URL and key never leave the orb:

   ```bash
   printenv CP_WEBHOOK_SECRET | cpctl webhook add amp \
     -url-file ~/.local/state/cp-inbox/webhook.url -secret-file - \
     -origin incoming -events thread.open,thread.reply,thread.needs-input,thread.resolve,peering.requested
   ```

   `-origin incoming` matters: the agent's own replies are outgoing events, so
   they never wake it again.

## Delivery

Amp accepts a burst of 10 events, then 10 a minute, with at most 100 waiting.
Beyond that it answers `429` with `Retry-After`, which `cpd` retries and
honors. `cpd` sends the delivery ID as `Idempotency-Key`, so a retry of an
event Amp still holds neither queues it twice nor uses rate capacity. An
archived owning thread answers `404`, which `cpd` treats as final:
restore the thread, then `cpctl webhook retry amp <delivery-id>`.

Both `cpd` and Amp deliver at least once. The plugin skips notification IDs it
has already handled, and checks the signature's timestamp against when Amp
accepted the request rather than the current time, because Amp may hold an
event while the orb wakes or a handler retries.
