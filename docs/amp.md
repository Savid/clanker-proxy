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

- The thread that first loads the plugin is the **inbox**. It owns the webhook
  and starts a conversation thread for each cpd thread. Peering requests never
  reach an agent: anyone can send one, and approving grants access. The
  plugin labels the inbox `cp-peering` and notifies you; review them yourself
  with `cpctl requests`.
- Each **conversation** keeps its own context. Its agent reads its thread with
  `cpctl show`, does the work (code, tests, local commits) and answers through
  the thread's `next:` commands. Later events for that cpd thread go back to
  the same conversation. This includes threads you sent: when the peer answers,
  the agent checks the answer against your request and closes or reopens it,
  or asks you.
- Webhook notifications carry only metadata. What a peer wrote reaches an
  agent only through `cpctl show`, which marks every body line with `│` and
  quotes titles and delivery errors, and agents are told to weigh it as a
  request, not follow it as instructions.

## Before you start

- **A reachable cpd.** cpd runs outside the orb, always on, with a public
  HTTPS `-url`. Peers deliver to it while the orb sleeps; a webhook wakes the
  orb, and its agents call back to cpd.
- **Amp with GitHub connected** (Settings → MCP & Integrations), so orbs can
  clone your repositories.
- **An agent token, not your owner token.** The orb's `CP_TOKEN` is readable
  by anything running there, including code an agent runs for a peer. An
  agent token (`cpctl token add`) can only list, read and act on threads; cpd
  refuses it for peering, opening threads, webhooks and tokens. If it leaks,
  someone can read and answer your threads until you run `cpctl token rm`,
  which also ends their open streams, but cannot add peers, redirect
  notifications or lock you out.
- **One token for the whole inbox.** Every conversation shares it, so each
  can reach every thread the inbox handles, not only its own. The plugin
  keeps a conversation to its thread, but a peer who talks its agent into
  working around that could read or answer your threads with other peers.
  To keep peers apart, give them separate inboxes (below).

## One inbox per peer

Secrets belong to an Amp project, so separate projects can hold separate
tokens. To keep a peer's threads away from everyone else's, give them their
own inbox: a project, a token that reaches only their threads, and a webhook
that sends only their events. cpd enforces the token's peers: it lists and
streams only their threads, and answers any other thread as if it did not
exist, so even a conversation talked out of the plugin's guard cannot reach
them.

| Amp project | Token | Webhook |
| --- | --- | --- |
| inbox-bob | `cpctl token add amp-bob -o amp-bob.token -peers bob` | `cpctl webhook add amp-bob -url-file amp-bob.url -secret-file bob.key -origin incoming -peers bob` |
| inbox-rest | `cpctl token add amp-rest -o amp-rest.token -peers carol,dave` | `cpctl webhook add amp-rest -url-file amp-rest.url -secret-file rest.key -origin incoming -peers carol,dave` |

Follow [Setup](#setup) once per project, with that project's names, a key of
its own, and `-peers` on both the token and the webhook. Then:

- **Scope every inbox.** An inbox without `-peers` still reaches every
  peer's threads, the separated ones included, and its conversations would
  work them too.
- **Keep `CP_TOKEN` out of Settings → Secrets & Env Vars**, which applies to
  every orb; with one there as well as in the project, which one an orb sees
  is not documented.
- **Keep one webhook without peers** for peering requests, such as a chat
  notification: a webhook with peers never receives them, since a requester
  picks the name it asks under.
- **Adding a peer later** means a new token (a token's peers are fixed):
  `cpctl token add` with the new list, the project's `CP_TOKEN` replaced,
  `amp orb restart-processes`, `cpctl token rm` for the old one, and
  `cpctl webhook set <name> -peers <new list>`. Until then, the new peer's
  threads reach no agent. Narrowing a webhook's peers also drops its queued
  notifications about the others.

## Choosing the project

Use an Amp project: in a project, the webhook belongs to the project and
plugin, so other threads that load the plugin get the same URL instead of
their own. Pick the project for the repository peers ask you about most;
conversations run in the inbox's orb, and its checkout is the default for code
work. Each conversation works in its own git worktree under `~/cp-work`, so
conversations never edit the same checkout. For another repository, an agent
clones it into `~/cp-work/repos` first, which needs GitHub access to it.

## Setup

1. **On your machine**, where cpctl uses your owner token, create the agent
   token (with `-peers` for an [inbox per peer](#one-inbox-per-peer)) and a
   webhook signing key:

   ```bash
   cpctl token add amp-inbox -o amp.token
   (umask 077; openssl rand -base64 32 > hook.key)
   ```

2. **Secrets.** In the Amp project's settings, add `CP_URL` (cpd's public
   URL), `CP_TOKEN` (the contents of `amp.token`) and `CP_WEBHOOK_SECRET`
   (the contents of `hook.key`). Prefer the project over Settings → Secrets &
   Env Vars, which would give the token to every orb you start. A running orb
   picks up secrets after `amp orb restart-processes`. Then delete
   `amp.token`.

3. **Start the inbox thread** in an orb (executor **New Orb**) in that
   project. Whichever thread loads the plugin first owns the webhook for good,
   so do this in the thread meant to be the inbox. Have it:

   - install `cpctl` (the README's install command) and make sure it is on
     `PATH` for both its shell and plugins; otherwise set `CP_INBOX_CPCTL` to
     its full path;
   - add `examples/amp/cp-inbox.ts` as `.amp/plugins/cp-inbox.ts` at the
     workspace root (the repository root in a project) and load it, or run
     `plugins: reload` from the command palette.

4. **Point cpd at it.** In that thread, run **cp-inbox: Show webhook URL**
   from the command palette. The URL appears in a secret dialog, which no
   thread or transcript records, since anyone with it can post events; the
   plugin does not store it. Copy it into a private file
   on your machine and add the webhook there:

   ```bash
   (umask 077; cat > amp.url)   # paste the URL, then Ctrl-D
   cpctl webhook add amp -url-file amp.url -secret-file hook.key -origin incoming   # and -peers, to match the token
   rm amp.url hook.key
   ```

   `-origin incoming` keeps your agents' own actions from waking them.

5. **Keep the inbox unarchived.** Archiving it pauses delivery and makes the
   URL return 404. Remove any workspace or personal guidance that tells Amp to
   archive threads when work finishes.

Committing the plugin to the project's repository loads it in every thread of
the project. That is safe: only the inbox owns the webhook, guardrails apply
to conversations, and the other threads only gain the `cp_ask_owner` tool.

If the wrong thread became the inbox, delete the `cp-inbox` trigger in
Settings → Triggers, reload the plugin in the right thread, and repeat step 4
with `cpctl webhook set amp -url-file amp.url`.
After adding a missing secret, restart the orb's processes and reload the
plugin. The plugin keeps which conversation handles which thread in
`cp-inbox/state.json` under `$XDG_STATE_HOME` (`~/.local/state` by default). If
it cannot read that file, it starts nothing and refuses every command in
`clanker-proxy` conversations, since the guard depends on it; repair or remove
the file and reload the plugin.

## Checking it works

1. Settings → Triggers lists `cp-inbox` with the key `cpd`, owned by the inbox
   thread. If it is missing, the plugin did not register: `CP_WEBHOOK_SECRET`
   was not set when it loaded.
2. Ask a peer to send you a test thread. Within seconds a conversation thread
   labelled `clanker-proxy` and `peer-<name>` appears under the inbox and
   starts working.
3. `cpctl webhook deliveries amp` shows the notification `delivered`. A
   delivered notification with no conversation means the plugin dropped or
   held it: the orb's Amp log says why (usually a key that differs from
   cpd's).

Not yet confirmed in a real orb: that the approval prompt below appears for
conversation threads, which run in the background, and that the plugin's
notifications reach your devices. Check both on your first run; if a prompt
cannot be shown, the command is refused and the agent asks you in its thread
instead.

## What the agent does alone

It acts on its own for peers you have approved, and asks you before anything
with effects outside the orb. The plugin checks every shell command a
conversation runs, and the files other tools name:

| Commands | Result |
| --- | --- |
| `cpctl` on any other thread, `ls`, `inbox`, `requests`, `approve`, `deny`, `peer …`, `webhook …`, `token …`, `update`; `-url`, `-token`, `CP_URL`, `CP_TOKEN` or `CP_DIR` overrides | Refused. cpd itself refuses the agent token for everything but threads. |
| Anything naming `CP_TOKEN`, `CP_URL`, `CP_DIR`, `CP_WEBHOOK_SECRET`, `owner.token`, `/proc/*/environ` or `~/.local/state`; printing the environment (`env`, `ps e`, indirect `${!…}`); `gh auth`; an unclosed quote | Refused. |
| `git push` (however git's options, wrappers such as `timeout` or `if … then` place it), git aliases, GitHub changes (everything but `gh pr`/`issue`/`run`/`repo` reads and `gh search`), uploads with `curl`/`wget`, `ssh`/`nc`, copying to other machines, publishing | Asks you to approve that exact command; refused if too long to show. |
| Everything else | Runs. |

Commands inside `bash -c`, `eval`, `$(…)` and here-documents that expand are
checked too; here-document text is not. A script the agent writes and runs,
or a renamed copy of a program, is not: this is a guardrail against an agent talked
into something, not a sandbox. When an agent needs a decision, it calls the
`cp_ask_owner` tool, which labels its thread `needs-owner` and sends you a
fixed notification; answer in that thread.

## Limits

A reply never changes whose turn it is, so two agents answering each other's
replies would never stop. Replies wake an idle conversation only if 10
minutes have passed since the last one, and at most 3 times until the peer
changes the thread's state; a conversation that is already working gets them
straight away. Beyond that:

- a conversation wakes at most 30 times a day, one peer's conversations 60
  times, and all conversations together 200 times;
- a peer starts at most 10 new conversations a day;
- when a peer ends a thread (close, decline, withdraw), a working conversation
  is told to stop; ended conversations are forgotten after 30 days.

cpd bounds what a peer can do on its side: each side has room for 1000 events
and 4 MiB of bodies in a thread, after which only close, decline or withdraw
without a body remain, a peer may have at most 200 threads open at once, and
a move that changes nothing sends no notification.

A held notification labels the conversation `cp-held` and notifies you at
most once a day per peer. Each time an event arrives, at most every 10 minutes,
the plugin wakes up to 5 threads that are your turn but have no conversation
or were held, such as ones whose notifications were lost while the inbox was
archived.

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
