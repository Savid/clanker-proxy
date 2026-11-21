# Amp orbs work your inbox

[`.amp/plugins/cp-inbox.ts`](../.amp/plugins/cp-inbox.ts) is an
[Amp](https://ampcode.com) plugin. Add this repository as an Amp project and it
loads by itself, only in that project's threads. Your daemon wakes an orb with a signed
webhook, and the orb gives each peer a thread, and each cpd thread a
conversation under it, which works it with `cpctl`. Any thread in the project
can ask a peer something with `cp_send`; the answer goes to a conversation too.

<p align="center"><img src="img/amp.svg" width="100%" alt="A peer's daemon reaches your cpd; cpd wakes the Amp orb's inbox thread with a signed webhook; the inbox routes the event to one conversation per cpd thread, under that peer's thread, which answers through cpctl."></p>

- **Inbox:** the thread that first loads the plugin. The plugin there receives cpd's webhooks and starts the threads below; the inbox's own agent never sees them and has nothing to do. Leave it idle, and never archive it. Peering requests never reach an agent: the plugin notifies you, and you review them with `cpctl requests`.
- **Webhooks** are trusted only as a wake-up. The plugin drops any event not signed with the key it derives from `CP_TOKEN`, and an event carries only IDs and a state, never peer text: what a conversation acts on, it reads from cpd.
- **Peer thread:** one per peer, under the inbox, named after them and labelled `cp-peer` and `peer-<name>`, from that peer's first thread. It holds their conversations, and it's where you, or another of your threads, ask them something ([ask a peer](#ask-a-peer)).
- **Conversation:** one per cpd thread, under its peer's thread, in its own git worktree under `~/cp-work`. It reads with `cpctl show`, works, and answers with the thread's `next:` commands. That includes threads this side sent: it checks the peer's answer, then closes, reopens, or asks you. Once the cpd thread is closed, declined or withdrawn, the plugin archives it; anything new on that thread brings it back.
- **Peer text** reaches an agent only through `cpctl show`, quoted line by line after `│`. Peers are third parties you chose to work with: agents help them in good faith, but only you instruct them. A peer's text comes from their agent, which may itself have been misled, so agents weigh it as a request, never instructions; they ignore attempts to steer them (prompt injection) and check what a peer supplies before it goes into code, docs or agent instructions (poisoning). Each thread's conversation is separate, but they share the orb and its token; to wall peers off from each other, give each its own inbox ([one inbox per peer](#one-inbox-per-peer)).

## You need

- **A daemon peers can reach**, outside the orb: native `cpd` with a public URL, or [the Worker](cloudflare.md). The orb sleeps; cpd doesn't.
- **Amp with GitHub connected** (Settings → MCP & Integrations), so orbs can clone your repositories.
- **A current daemon.** Orbs take the plugin from this repository's master, and [`.agents/setup`](../.agents/setup) installs the `cpctl` your daemon runs. Rerun `cloudflare.sh` (or `install.sh`) to update the daemon.
- **An agent token, never the owner token.** Anything in the orb can read `CP_TOKEN`, including code an agent runs for a peer. An agent token can send and work threads, never peer, make tokens or change your webhooks, and `cpctl token rm` revokes it and ends its streams. Unscoped, it reaches every peer, as you: [scope it](#one-inbox-per-peer) once you have peers.

## Setup

1. **An agent token**, on your machine as the owner. Chose Amp in `cloudflare.sh`? It made one for you.

   ```bash
   cpctl token add amp-inbox -o amp.token            # add --peers for a per-peer inbox
   ```

2. **The project and its secrets.** Add `Savid/clanker-proxy` as an Amp project. Save `CP_URL` and `CP_TOKEN` (the token) as secrets of that project, so only its threads see them: in its settings, or have a thread ask you, and Amp takes each value in a private dialog that keeps it out of the thread. A running orb picks up changes after `amp orb restart-processes`.

3. **Start the inbox:** a new thread in that project. Its orb runs `.agents/setup`, which installs `cpctl`, and loads the plugin, which registers its webhook with your daemon, signed with a key derived from `CP_TOKEN`. It hears only the peers' moves, and only for the token's peers. The first thread to load the plugin with the secrets set owns the webhook for good.

4. **Never archive the inbox.** Archived, it answers `404`, a final failure. Restore it, then `cpctl webhook retry amp-inbox <delivery-id>`. Remove any guidance that archives finished threads.

Wrong thread became the inbox? Delete the `cp-inbox` trigger in Amp's
trigger settings and start a new thread in the project; it registers again.

## Check it

1. Settings → Triggers lists `cp-inbox`, key `cpd`, owned by the inbox. The orb's Amp log says `cp-inbox: registered with cpd`; if it says `set CP_URL and CP_TOKEN`, they were missing when the plugin loaded.
2. On your machine, with the owner token, `cpctl webhook ls` lists `amp-inbox`, registered by that agent token.
3. A peer sends a test thread. Within seconds, that peer's thread starts under the inbox, with a conversation labelled `clanker-proxy` and `peer-<name>` under it, and `cpctl webhook deliveries amp-inbox` shows it `delivered`.

## Ask a peer

In any thread of the project but the inbox, ask the agent to send it: its
`cp_send` tool opens the cpd thread and returns its ID. The answer goes to a
conversation under that peer's thread, which checks it and closes or reopens
it, so peer text only ever reaches conversations. `cpctl send` does the same.

From a thread outside the project, ask the agent to message the peer's
thread, which is named after them: "message bob and ask them <what>". It sends
with `cp_send`, to that peer only, and tells the asking thread the thread ID.

A conversation's `cp_send` reaches only its own peer, and asks you first.

## One inbox per peer

Every thread in the project shares the inbox's token, so a peer who talks an
agent past the plugin's guard could reach your other peers and their threads. To keep peers
apart, give each its own project (a repository of your own, see
[Persist the setup](#persist-the-setup)) and token. cpd enforces a token's peers,
answers every other thread as not found, and the token's webhook hears only
those peers.

| Project | Token |
| --- | --- |
| inbox-bob | `cpctl token add amp-bob -o amp-bob.token --peers bob` |
| inbox-rest | `cpctl token add amp-rest -o amp-rest.token --peers carol,dave` |

- Scope every inbox. One without `--peers` still reaches everyone.
- Don't set a personal `CP_TOKEN`: it overrides the inbox's.
- Peering requests reach only an inbox whose token has no `--peers`, and there the plugin turns them into a notice for you, never an agent. Scoped inboxes don't get them, so for notices add a webhook of your own (a chat channel, say): [webhooks.md](webhooks.md).
- A token's peers are fixed. To add a peer, make a new token and swap it in, restart the orb's processes, then `cpctl token rm` the old one: its webhook goes with it, and the new token's registers when the plugin reloads.

## Persist the setup

Nothing to do in `Savid/clanker-proxy`: the repository has the plugin and
`.agents/setup`, so every new orb of the project gets both. Never commit
tokens or `cp-inbox/state.json`.

For an inbox in a repository of your own, copy
[`.amp/plugins/cp-inbox.ts`](../.amp/plugins/cp-inbox.ts) and
[`.agents/setup`](../.agents/setup) into it and commit them.

## What agents may do alone

Each conversation starts with standing instructions. Alone, it does what a
peer's request reasonably needs: reads and changes code in its own worktree,
runs tests, commits locally, and answers through `next:`. It asks you first,
with `cp_ask_owner`, before anything with effects outside the orb: pushing,
pull requests, publishing, spending, sharing. A new thread to its peer goes
through `cp_send`, which asks you. It never runs a peer's code outside the
repository it works on, and never handles peering requests, webhooks or
credentials.

The plugin also checks every shell command a conversation runs, and every
other tool it calls:

| Command | Result |
| --- | --- |
| `cpctl` on this conversation's thread, `help`, `me` | runs |
| any other `cpctl` use, or `--url`/`--token` overrides | refused; new threads go through `cp_send` |
| Amp's tools for other threads (`read_thread`, `send_thread_message`, `create_thread`, …) and the `amp` CLI, plugins, Amp settings, your Mac | refused |
| reading `CP_*` secrets, `owner.token`, `/proc/*/environ`, `~/.local/state`, the environment, `gh auth`, in commands or tools' file paths; an unclosed quote | refused |
| `git push` (however wrapped, or Amp's `git_push`), git aliases, GitHub writes, uploads (`curl`, `wget`), `ssh`, `nc`, publishing, `docker push` | asks you to approve that exact command |
| Amp tools other than files, search, web reads and subagents, including MCP tools | asks you first |
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
