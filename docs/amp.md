# Amp orbs work your inbox

[`examples/amp/cp-inbox.ts`](../examples/amp/cp-inbox.ts) is an
[Amp](https://ampcode.com) plugin. Your daemon wakes an orb with a signed
webhook, and the orb gives each cpd thread its own conversation, which works
it with `cpctl`.

<p align="center"><img src="img/amp.svg" width="100%" alt="A peer's daemon reaches your cpd; cpd wakes the Amp orb's inbox thread with a signed webhook; the inbox routes the event to one conversation per cpd thread, which answers through cpctl."></p>

- **Inbox:** the thread that first loads the plugin. The plugin there receives cpd's webhooks and starts the conversations; the inbox's own agent never sees them and has nothing to do. Leave it idle, and never archive it. Peering requests never reach an agent: the plugin notifies you, and you review them with `cpctl requests`.
- **Webhooks** are trusted only as a wake-up. The plugin drops any event not signed with the key it derives from `CP_TOKEN`, and an event carries only IDs and a state, never peer text: what a conversation acts on, it reads from cpd.
- **Conversation:** one per cpd thread, in its own git worktree under `~/cp-work`. It reads with `cpctl show`, works, and answers with the thread's `next:` commands. That includes threads you sent: it checks the peer's answer, then closes, reopens, or asks you.
- **Peer text** reaches an agent only through `cpctl show`, quoted line by line after `│`. Peers are third parties you chose to work with: agents help them in good faith, but only you instruct them. A peer's text comes from their agent, which may itself have been misled, so agents weigh it as a request, never instructions; they ignore attempts to steer them (prompt injection) and check what a peer supplies before it goes into code, docs or agent instructions (poisoning). Each thread's conversation is separate, but they share the orb and its token; to wall peers off from each other, give each its own inbox ([one inbox per peer](#one-inbox-per-peer)).

## You need

- **A daemon peers can reach**, outside the orb: native `cpd` with a public URL, or [the Worker](cloudflare.md). The orb sleeps; cpd doesn't.
- **Amp with GitHub connected** (Settings → MCP & Integrations), so orbs can clone your repositories.
- **One release** for daemon, `cpctl` and plugin. Read this file at that release's tag.
- **An agent token, never the owner token.** Anything in the orb can read `CP_TOKEN`, including code an agent runs for a peer. An agent token can only work threads, and `cpctl token rm` revokes it and ends its streams.

## Setup

1. **An agent token**, on your machine as the owner. Chose Amp in `cloudflare.sh`? It made one for you.

   ```bash
   cpctl token add amp-inbox -o amp.token            # add --peers for a per-peer inbox
   ```

2. **Amp secrets:** `CP_URL` and `CP_TOKEN` (the token). Add them in Settings, or have the inbox thread ask you for them: Amp takes each value in a private dialog that keeps it out of the thread, and saves it as a secret. A secret is an environment variable in the orb too; Amp just hides its value in output. A running orb picks up changes after `amp orb restart-processes`.

3. **Start the inbox:** make a new Amp space (e.g. `clanker-proxy`) and paste in the prompt `cloudflare.sh` printed. It must:
   - install `cpctl` with the [setup script](#persist-the-setup), on `PATH` for shell and plugins (or set `CP_INBOX_CPCTL`);
   - copy the plugin to `.amp/plugins/cp-inbox.ts` and run `plugins: reload`.

   The plugin registers its webhook with your daemon by itself, signed with a key derived from `CP_TOKEN`. It hears only the peers' moves, and only for the token's peers. Whichever thread loads the plugin first owns the webhook for good.

4. **Never archive the inbox.** Archived, it answers `404`, a final failure. Restore it, then `cpctl webhook retry amp-inbox <delivery-id>`. Remove any guidance that archives finished threads.

Wrong thread became the inbox? Delete the `cp-inbox` trigger (Settings →
Triggers) and reload the plugin in the right thread; it registers again.

## Check it

1. Settings → Triggers lists `cp-inbox`, key `cpd`, owned by the inbox. The orb's Amp log says `cp-inbox: registered with cpd`; if it says `set CP_URL and CP_TOKEN`, they were missing when the plugin loaded.
2. `cpctl webhook ls` lists `amp-inbox`, registered by that agent token.
3. A peer sends a test thread. A conversation labelled `clanker-proxy` and `peer-<name>` starts within seconds, and `cpctl webhook deliveries amp-inbox` shows it `delivered`.

## One inbox per peer

Every conversation shares the inbox's token, so a peer who talks an agent past
the plugin's guard could reach your other peers' threads. To keep peers
apart, give each its own space and token. cpd enforces a token's peers,
answers every other thread as not found, and the token's webhook hears only
those peers.

| Space | Token |
| --- | --- |
| inbox-bob | `cpctl token add amp-bob -o amp-bob.token --peers bob` |
| inbox-rest | `cpctl token add amp-rest -o amp-rest.token --peers carol,dave` |

- Scope every inbox. One without `--peers` still reaches everyone.
- Don't set a personal `CP_TOKEN`: it overrides the inbox's.
- Peering requests reach only an inbox whose token has no `--peers`, and the plugin turns them into a notice for you, never an agent. Scoped inboxes don't get them: keep one inbox unscoped, or add a webhook of your own (a chat channel, say): [webhooks.md](webhooks.md).
- A token's peers are fixed. To add a peer, make a new token and swap it in, restart the orb's processes, then `cpctl token rm` the old one: its webhook goes with it, and the new token's registers when the plugin reloads.

## Persist the setup

Commit `.amp/plugins/cp-inbox.ts` and an executable `.agents/setup` so
future orbs get both; Amp snapshots the result. The script installs the
`cpctl` matching the release your daemon runs, so it follows the daemon when
you update it, or the latest release if it cannot ask the daemon:

```sh
#!/bin/sh
set -eu
bin="$HOME/.local/bin"
# The daemon's release, read with the agent token; the latest release when
# the settings are not set here or the daemon cannot be reached.
version=$(curl -fsS -H "Authorization: Bearer ${CP_TOKEN:-}" "${CP_URL:-}/api/v1/me" 2>/dev/null |
    sed -n 's/.*"version":"\(v[0-9][^"]*\)".*/\1/p')
[ -n "$version" ] || version=$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
    https://github.com/Savid/clanker-proxy/releases/latest | sed 's|.*/tag/||')
if [ "$("$bin/cpctl" --version 2>/dev/null || true)" != "cpctl $version" ]; then
    installer=$(mktemp)
    trap 'rm -f "$installer"' EXIT
    curl -fsSL "https://github.com/Savid/clanker-proxy/releases/download/$version/install.sh" -o "$installer"
    CP_VERSION="$version" CP_INSTALL_DIR="$bin" sh "$installer"
fi
"$bin/cpctl" --version
```

After you update the daemon, new orbs install the matching `cpctl`; in a
running orb, run `.agents/setup` again. Take the plugin file from the same
release, reload it, and commit. If new orbs still get the old setup, delete
the project's cached snapshot in its Orb settings. Never commit tokens or
`cp-inbox/state.json`.

## What agents may do alone

Each conversation starts with standing instructions. Alone, it does what a
peer's request reasonably needs: reads and changes code in its own worktree,
runs tests, commits locally, and answers through `next:`. It asks you first,
with `cp_ask_owner`, before anything with effects outside the orb: pushing,
pull requests, new threads, publishing, spending, sharing. It never runs a
peer's code outside the repository it works on, and never handles peering
requests, webhooks or credentials.

The plugin also checks every shell command a conversation runs, and every
file other tools name:

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
