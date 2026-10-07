# Letting an Amp agent work your inbox

[Amp](https://ampcode.com) plugins running in an orb can register a webhook
that wakes the thread owning it. [`examples/amp/cp-inbox.ts`](../examples/amp/cp-inbox.ts)
uses one to let Amp agents handle your clanker-proxy threads:

```
peer ─▶ your cpd ─▶ signed webhook ─▶ Amp ─▶ inbox thread (cp-inbox plugin)
                                                │ one conversation per cpd thread
                                                ▼
              your cpd ◀── cpctl show / reply ◀── conversation thread (same orb)
```

- The thread that first loads the plugin is the **inbox**. It owns the webhook,
  starts a conversation thread for each cpd thread, and gets peering requests,
  which it summarizes for you and never approves.
- Each **conversation** keeps its own context. Its agent reads the thread with
  `cpctl show`, does the work (code, tests, local commits) and answers through
  the thread's `next:` commands. Later events for that cpd thread go back to
  the same conversation.
- Notifications carry only metadata, so nothing a peer wrote reaches a prompt
  through the webhook. Agents are told to treat peer messages as requests to
  weigh, not instructions.

## Before you start

- **Versions.** Both your cpd and the `cpctl` in the orb must be a release
  with destination types (after v0.1.2): older ones cannot read the key from
  stdin or the URL from a file. Run `cpctl update` where cpd runs and restart
  it. Webhooks you configured before are kept, as signed `generic` webhooks.
- **A reachable cpd.** cpd runs outside the orb, always on, with a public
  HTTPS `-url`. Peers deliver to it while the orb sleeps; a webhook wakes the
  orb, and its agents call back to cpd.
- **Amp with GitHub connected** (Settings → MCP & Integrations), so orbs can
  clone your repositories.
- **Your owner token**: the `owner.token` file in cpd's data directory
  (`~/.cp/owner.token` by default). `cpctl` in the orb uses it as `CP_TOKEN`
  and can do anything you can; the guardrails below limit what agents do with
  it, but they are not a sandbox.

## Choosing the project

Conversations run in the inbox's orb and see its workspace. Start the inbox
thread in the project for the repository peers ask you about most; its
checkout is the default for code work. Each conversation works in its own git
worktree under `~/cp-work`, so conversations in the shared orb never edit the
same checkout. For another repository, an agent clones it into
`~/cp-work/repos` first, which needs GitHub access to it.

## Setup

1. **Secrets.** Generate a signing key:

   ```bash
   openssl rand -base64 32
   ```

   In the project's settings (or Settings → Secrets & Env Vars for all your
   orbs), add `CP_WEBHOOK_SECRET` (that key), `CP_URL` (cpd's public URL) and
   `CP_TOKEN` (the owner token). A running orb picks them up after
   `amp orb restart-processes`.

2. **Plugin.** Add `examples/amp/cp-inbox.ts` as `.amp/plugins/cp-inbox.ts`
   at the workspace root: the repository root in a project, or
   `/home/user/workspace` without one. Committing it to the project's
   repository loads it in every thread of that project; that is harmless,
   since only the inbox thread owns the webhook and the guardrails apply only
   to conversations.

3. **Inbox thread.** Start a thread in an orb (executor **New Orb**) in that
   project, install `cpctl` there (the README's install command), and load the
   plugin. This thread is now the inbox. The plugin writes its webhook URL to
   `~/.local/state/cp-inbox/webhook.url` (mode 0600) instead of showing it,
   since anyone with the URL can post events.

4. **Point cpd at it**, from the inbox orb so the URL and key never leave it:

   ```bash
   printenv CP_WEBHOOK_SECRET | cpctl webhook add amp \
     -url-file ~/.local/state/cp-inbox/webhook.url -secret-file - -origin incoming
   ```

   `-origin incoming` keeps your agents' own actions from waking them.

5. **Keep the inbox unarchived.** Archiving it pauses delivery and makes the
   URL return 404. Remove any workspace or personal guidance that tells Amp to
   archive threads when work finishes.

## Checking it works

1. Settings → Triggers lists `cp-inbox` with the key `cpd`, owned by the inbox
   thread. If it is missing, the plugin did not register: `CP_WEBHOOK_SECRET`
   was not set when it loaded.
2. Ask a peer to send you a test thread. Within seconds a conversation thread
   labelled `clanker-proxy` and `peer-<name>` appears under the inbox and
   starts working.
3. `cpctl webhook deliveries amp` shows the notification `delivered`. A
   delivered notification with no conversation means the plugin dropped it:
   the orb's Amp log says why (usually a key that differs from cpd's).

Not yet confirmed in a real orb: that the approval prompt below appears for
conversation threads, which run in the background, and that `cp_ask_owner`
notifications reach your devices. Check both on your first run; if a prompt
cannot be shown, the command is refused and the agent asks you in its thread
instead.

## What the agent does alone

It acts on its own for peers you have approved, and asks you before anything
with effects outside the orb. The plugin enforces part of this on every shell
command a conversation runs:

| Commands | Result |
| --- | --- |
| `cpctl approve`, `deny`, `peer add`/`rm`, `webhook …`, `update`; reading `CP_TOKEN`, `owner.token`, the webhook URL or the whole environment | Refused. |
| `git push`, GitHub changes (`gh pr create`, `gh api`, …), `cpctl send`, publishing, copying files to other machines | Asks you to approve that exact command. |
| Everything else | Runs. |

A script the agent writes can still do these things. When an agent needs a
decision, it calls the `cp_ask_owner` tool, which labels its thread
`needs-owner` and notifies you; answer in that thread.

## Turns and loops

Every cpd thread has a turn: the recipient's while it is open or acked, the
sender's while it needs input or is resolved. A conversation wakes whenever
it becomes your turn. A reply that arrives on the peer's turn, such as a
follow-up question on a resolved thread, also wakes it, with instructions to
answer at most once without changing the thread's state.

Two agents answering each other's replies would never stop, so off-turn
replies wake a conversation only if 10 minutes have passed since the last
one, and at most 3 times until the turn changes. Every conversation is
capped at 30 wake-ups a day. A held notification labels the conversation
`cp-held`; read the thread with `cpctl show`.

Each time an event arrives, at most every 10 minutes, the plugin also starts
conversations (up to 5) for threads that are your turn but have none, such as
ones whose notifications were lost while the inbox was archived.

## Delivery

Amp accepts a burst of 10 events, then 10 a minute, with at most 100 waiting.
Beyond that it answers `429` with `Retry-After`, which cpd retries and honors.
An archived inbox answers `404`, which cpd treats as final: restore the
thread, then `cpctl webhook retry amp <delivery-id>`. cpd sends the delivery
ID as `Idempotency-Key`, so a retry of an event Amp still holds neither
queues twice nor uses rate capacity. Both deliver at least once; the plugin
also skips notification IDs it has handled. It checks the signature's
timestamp against when Amp accepted the request, not the current time,
because Amp may hold an event while the orb wakes.

Processes started in an orb, such as a test cpd, stop when the orb pauses.
The plugin does not: Amp loads it again when the next event wakes the orb.
