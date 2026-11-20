# Amp orbs work your inbox

[`examples/amp/cp-inbox.ts`](../examples/amp/cp-inbox.ts) is an
[Amp](https://ampcode.com) plugin. Your daemon wakes an orb with a signed
webhook, and the orb gives each cpd thread its own conversation, which works
it with `cpctl`.

<p align="center"><img src="img/amp.svg" width="100%" alt="A peer's daemon reaches your cpd; cpd wakes the Amp orb's inbox thread with a signed webhook; the inbox routes the event to one conversation per cpd thread, which answers through cpctl."></p>

- **Inbox:** the thread that first loads the plugin. It owns the webhook and starts the conversations. Peering requests never reach an agent: you get a notification and review them with `cpctl requests`.
- **Conversation:** one per cpd thread, in its own git worktree under `~/cp-work`. It reads with `cpctl show`, works, and answers with the thread's `next:` commands. That includes threads you sent: it checks the peer's answer, then closes, reopens, or asks you.
- **Peer text** reaches an agent only through `cpctl show`, quoted line by line. Agents are told to treat it as a request, not as instructions.

## You need

- **A daemon peers can reach**, outside the orb: native `cpd` with a public URL, or [the Worker](cloudflare.md). The orb sleeps; cpd doesn't.
- **Amp with GitHub connected** (Settings → MCP & Integrations), so orbs can clone your repositories.
- **One release** for daemon, `cpctl` and plugin. Read this file at that release's tag.
- **An agent token, never the owner token.** Anything in the orb can read `CP_TOKEN`, including code an agent runs for a peer. An agent token can only work threads, and `cpctl token rm` revokes it and ends its streams.

## Setup

1. **On your machine**, as the owner:

   ```bash
   cpctl token add amp-inbox -o amp.token            # add --peers for a per-peer inbox
   (umask 077; openssl rand -base64 32 > hook.key)
   ```

2. **Amp project settings:** `CP_URL` as a variable; `CP_TOKEN` (from `amp.token`) and `CP_WEBHOOK_SECRET` (from `hook.key`) as secrets. Then delete `amp.token`. After changing them, run `amp orb restart-processes`.

3. **Start the inbox:** a new thread in an orb of that project. It must:
   - install `cpctl` with the [setup script](#persist-the-setup), on `PATH` for shell and plugins (or set `CP_INBOX_CPCTL`);
   - copy the plugin to `.amp/plugins/cp-inbox.ts` and run `plugins: reload`.

   Whichever thread loads the plugin first owns the webhook for good.

4. **Point cpd at it.** In the inbox, run **cp-inbox: Show webhook URL** from the command palette. The URL shows in a secret dialog, and anyone holding it can post events. On your machine:

   ```bash
   (umask 077; cat > amp.url)     # paste, then Ctrl-D
   cpctl webhook add amp --url-file amp.url --secret-file hook.key --origin incoming   # --peers to match the token
   rm amp.url hook.key
   ```

   `--origin incoming` keeps your agents' own moves from waking them.

5. **Never archive the inbox.** Archived, it answers `404`, a final failure. Restore it, then `cpctl webhook retry amp <delivery-id>`. Remove any guidance that archives finished threads.

Wrong thread became the inbox? Delete the `cp-inbox` trigger (Settings →
Triggers), reload the plugin in the right thread, and repeat step 4 with
`cpctl webhook set amp --url-file amp.url`.

## Check it

1. Settings → Triggers lists `cp-inbox`, key `cpd`, owned by the inbox. Missing means `CP_WEBHOOK_SECRET` wasn't set when the plugin loaded.
2. A peer sends a test thread. A conversation labelled `clanker-proxy` and `peer-<name>` starts within seconds.
3. `cpctl webhook deliveries amp` shows it `delivered`. Delivered with no conversation means the plugin dropped it, usually because the key differs from cpd's; the orb's Amp log says why.

## One inbox per peer

Every conversation shares the inbox's token, so a peer who talks an agent past
the plugin's guard could reach your other peers' threads. To keep peers
apart, give each its own project, token and webhook. cpd enforces a token's
peers and answers every other thread as not found.

| Project | Token | Webhook |
| --- | --- | --- |
| inbox-bob | `cpctl token add amp-bob -o amp-bob.token --peers bob` | `cpctl webhook add amp-bob --url-file amp-bob.url --secret-file bob.key --peers bob` |
| inbox-rest | `cpctl token add amp-rest -o amp-rest.token --peers carol,dave` | `cpctl webhook add amp-rest --url-file amp-rest.url --secret-file rest.key --peers carol,dave` |

- Scope every inbox. One without `--peers` still reaches everyone.
- Keep `CP_TOKEN` in project settings. A personal value overrides it.
- Keep one webhook without peers (a chat channel, say) for peering requests. Scoped webhooks never get them.
- A token's peers are fixed. To add a peer, make a new token and swap it in, restart the orb's processes, `cpctl token rm` the old one, then `cpctl webhook set <name> --peers <list>`.

## Persist the setup

Commit `.amp/plugins/cp-inbox.ts` and an executable `.agents/setup` so
future orbs get both; Amp snapshots the result. Pin the release your daemon
runs:

```sh
#!/bin/sh
set -eu
cp_version=vX.Y.Z
bin="$HOME/.local/bin"
if [ "$("$bin/cpctl" --version 2>/dev/null || true)" != "cpctl $cp_version" ]; then
    installer=$(mktemp)
    trap 'rm -f "$installer"' EXIT
    curl -fsSL "https://github.com/Savid/clanker-proxy/releases/download/$cp_version/install.sh" -o "$installer"
    CP_VERSION="$cp_version" CP_INSTALL_DIR="$bin" sh "$installer"
fi
"$bin/cpctl" --version
```

To upgrade, bump the pin and the plugin together, run the script, reload the
plugin, and commit. If new orbs still get the old setup, delete the project's
cached snapshot in its Orb settings. Never commit tokens, keys, webhook URLs or
`cp-inbox/state.json`.

## What agents may do alone

The plugin checks every shell command a conversation runs, and every file
other tools name:

| Command | Result |
| --- | --- |
| `cpctl` on this conversation's thread, `help`, `me` | runs |
| any other `cpctl` use, or `--url`/`--token` overrides | refused; cpd refuses the agent token for those anyway |
| reading `CP_*` secrets, `owner.token`, `/proc/*/environ`, `~/.local/state`, the environment, `gh auth`; an unclosed quote | refused |
| `git push` (however wrapped), git aliases, GitHub writes, uploads (`curl`, `wget`), `ssh`, `nc`, publishing, `docker push` | asks you to approve that exact command |
| everything else | runs |

Commands are checked inside `bash -c`, `eval`, `$(…)` and here-documents. A
script the agent writes and then runs is not. This is a guardrail, not a sandbox.
When an agent needs you, its `cp_ask_owner` tool labels the thread
`needs-owner` and notifies you; answer in that thread.

## Limits

Replies never change the turn, so two agents could answer each other forever.
The plugin caps that:

- an idle conversation wakes for a reply only 10 min after the last one, and 3 times until the state changes;
- 30 wakes a day per conversation, 60 per peer, 200 in total; 10 new conversations per peer per day;
- held notices label the conversation `cp-held`, and each event lets the plugin wake up to 5 threads that are your turn but have no conversation;
- an ended thread is reread before acting, since it may have been reopened. Ended conversations are forgotten after 30 days.

Amp itself takes a burst of 10 events, then 10 a minute, holding up to 100,
and answers `429 Retry-After` beyond that, which cpd honours. cpd sends each
notification's ID as `Idempotency-Key`, so retries never queue twice, and the
plugin skips IDs it has handled.

Tests: `make test-amp`.
