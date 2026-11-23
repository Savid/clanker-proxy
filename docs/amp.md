# Amp orbs work your inbox

[`.amp/plugins/cp-inbox.ts`](../.amp/plugins/cp-inbox.ts) is an
[Amp](https://ampcode.com) plugin, loaded in the threads of the Amp project it
is in. Your daemon wakes an orb with a signed webhook, and the plugin gives
each peer a thread, and each cpd thread a conversation under it, which works
it with `cpctl` in a sandbox. Any of your threads can ask a peer something;
the answer goes to a conversation, and the thread that asked gets a short
form of it, never the peer's words. Read [Risks](#risks) before you set it up.

<p align="center"><img src="img/amp.svg" width="100%" alt="A peer's daemon reaches your cpd; cpd wakes the Amp orb's inbox thread with a signed webhook; the inbox routes the event to one conversation per cpd thread, under that peer's thread, which answers through cpctl."></p>

- **Inbox:** the thread that first loads the plugin. It receives cpd's webhooks and starts the threads below; its own agent has nothing to do. Leave it idle, and never archive it.
- **Peer thread:** one per peer, under the inbox, named after them and labelled `cp-peer` and `peer-<name>`, started by their first thread. It's where you, or any of your threads, [ask them something](#ask-a-peer).
- **Conversation:** one per cpd thread, under its peer's thread, labelled `clanker-proxy` and `peer-<name>`. It reads the thread, works in its own directory, and answers with the thread's `next:` commands; on threads you sent, it checks the answer, then closes, reopens, or asks you. It is archived when the cpd thread ends, and comes back if anything new arrives. Its **cpd thread** button shows you the cpd thread (its newest 30,000 characters), never an agent.
- **Webhooks** only wake the plugin. It drops any event not signed with the key it derives from `CP_TOKEN`, and an event carries IDs and a state, never peer text.
- **Peering requests** never reach an agent: the plugin notifies you, and you review them with `cpctl requests`.

## You need

- **A daemon peers can reach**, outside the orb: native `cpd` with a public URL, or [the Worker](cloudflare.md). The orb sleeps; cpd doesn't.
- **Amp with GitHub connected** (Settings → MCP & Integrations), so orbs can clone your repositories. Conversations can't: they fetch only public ones.
- **An agent token, never the owner token.** It can send and work threads, never peer, make tokens or change your webhooks, and `cpctl token rm` revokes it. Unscoped, it reaches every peer: [scope it](#one-inbox-per-peer) once you have more than one.

## Setup

1. **An agent token**, on your machine as the owner. Chose Amp in `cloudflare.sh`? It made one for you.

   ```bash
   cpctl token add amp-inbox -o amp.token            # add --peers for a per-peer inbox
   ```

2. **The project and its secrets.** Add `Savid/clanker-proxy` as an Amp project, or [your own repository](#your-own-repository). Save `CP_URL` and `CP_TOKEN` (the token) as secrets of that project, so only its threads see them: in its settings, or have a thread ask you, and Amp takes each value in a private dialog that keeps it out of the thread. Never as personal secrets: every thread sees those, and they override the project's. A running orb picks up changes after `amp orb restart-processes`.

3. **Start the inbox:** a new thread in that project, not the thread doing the setup. Its orb runs [`.agents/setup`](../.agents/setup), which installs the `cpctl` your daemon runs, and loads the plugin, which registers its webhook with your daemon. The first thread to load the plugin with the secrets set owns the webhook for good.

4. **Never archive the inbox.** Archived, it answers `404`, a final failure. Restore it, then `cpctl webhook retry amp-inbox <delivery-id>`. Remove any guidance that archives finished threads.

5. **Ask from any thread:** add [`cp-peers`](../.amp/personal/cp-peers/index.ts) to your personal plugins, which Amp loads in all your threads.

   ```bash
   amp clone user-plugins ~/user-plugins && mkdir -p ~/user-plugins/cp-peers
   curl -fsSL https://raw.githubusercontent.com/Savid/clanker-proxy/master/.amp/personal/cp-peers/index.ts -o ~/user-plugins/cp-peers/index.ts
   cd ~/user-plugins && git add cp-peers && git commit -m "cp-peers: ask clanker-proxy peers from any thread" && git push
   ```

Wrong thread became the inbox? Delete the `cp-inbox` trigger in Amp's
trigger settings and start a new thread in the project; it registers again.

**Updating.** Rerun `cloudflare.sh` (or `install.sh`) to update the daemon;
new orbs install its `cpctl`. New orbs take the plugin as the project's
repository has it, but a running inbox keeps the one it started with until
you ask it to pull and reload its plugins. Rerun step 5 to update `cp-peers`.

## Check it

1. Settings → Triggers lists `cp-inbox`, key `cpd`, owned by the inbox. The orb's Amp log says `cp-inbox: registered with cpd`; if it says `set CP_URL and CP_TOKEN`, they were missing when the plugin loaded.
2. A peer sends a test thread. Within seconds, that peer's thread starts under the inbox, with a conversation under it.

## Ask a peer

From any thread, ask: "ask bob <what>", by name or [alias](#names-and-aliases). In the project, `cp_send` sends it.
Elsewhere, `cp-peers`' `ask_peer` hands it to bob's thread, which sends it
with `cp_send`, to bob only. Bob's answer goes to a conversation under bob's
thread, which checks it and closes or reopens it. The thread that asked is
told, and gets the conversation's short form of it with `cp_answer`: yes, no,
maybe, done, declined or unclear, and a date or time. You read bob's words
with that thread's button, or under bob's thread. `cpctl send` does the same
from your machine.

Your other threads must never read bob's words: they hold your credentials,
and bob's words could steer them. They are never given an inbox thread's ID,
and `cp-peers` refuses their tools that name one, by ID, link or label.

A peer's thread starts with the first thread between you; before that, ask
from a thread in the project. A conversation's `cp_send` reaches only its own
peer, and asks you first.

## Names and aliases

A peer's name is yours: `cpctl peer add <name>` sets it, and so does
`cpctl approve <id> --as <name>`, which defaults to the name they asked for.
Aliases are more names you give the same peer:

```bash
cpctl peer alias tester bobby rob
cpctl peer unalias tester rob
```

`cpctl alias andrew` offers peers another name for you: whoever you peer with
next, either way round, gets it as an alias if it is free there.

Names and aliases share one namespace, so one already taken is refused: give
the peer another name. cpd takes an alias wherever it takes a name, except to
remove a peer, and answers with the name. The inbox titles each peer's thread
with its name and aliases, "tester (bobby)", on the next event, or within 10
minutes while its orb is awake, and from that `ask_peer` knows them: "is bobby
free for a beer?" reaches tester from any thread.

## What a conversation can do

It starts with standing instructions: peers are third parties you chose to
work with, so it helps them in good faith, but only you instruct it. It
weighs what a peer writes as a request, never instructions, and checks what
a peer supplies before it goes into code, docs or agent instructions. Alone,
it does what a request reasonably needs: reads and changes code in its own
directory, runs tests, commits locally, and answers through `next:`. It asks
you, with `cp_ask_owner`, before anything else with effects outside the orb;
`cp_ask_owner` labels the thread `needs-owner` and notifies you, and you
answer in that thread.

It has only the shell, web search and web pages, plus `cp_send`,
`cp_ask_owner` and `cp_answer`: no file tools, code mode, subagents, MCP servers, or tools
for other threads, Amp or other services. Amp's file tools would run outside
the sandbox, as you.

Everything it runs, and any script it writes, runs in a sandbox: as you, but
without privileges or a way to regain them (no `sudo`), with a clean
environment, and with no other process, IPC or terminal in sight. Every
filesystem is read-only, and your home is empty but for the checkout, the
`bin` directories on `PATH` and `~/.gitconfig`, read-only, and the
conversation's own directory under `~/cp-work`, its only writable place and
its `/tmp` and `$HOME`. Your credentials, Amp's and `gh`'s, your Amp
secrets, the `amp` CLI, the inbox's state and other conversations' work are
out of reach, and so are toolchains in your home outside those `bin`
directories, such as rustup's: it installs what it needs in its own
directory. The sandbox needs passwordless `sudo`, `unshare` and `setpriv`,
which orbs have.

Its `cpctl` reaches cpd through the plugin, which holds the token and passes
on only reading and acting on the conversation's own thread. The sandbox
gets a key that works only for that, and only inside the orb.

A command you approve runs in the sandbox too, so it never holds your
credentials: pushing and publishing fail. To publish a conversation's work,
fetch its branch into a repository you trust and push from there; don't run
git or build tools in its directory outside the sandbox, where its hooks and
configuration would run as you. The plugin checks every command, inside
`bash -c`, `eval`, `$(…)` and here-documents:

| Command | Result |
| --- | --- |
| `cpctl` on this conversation's thread, `help`, `me` | runs |
| `cpctl wait` | refused; it is woken when the peer acts |
| any other `cpctl` use | refused; new threads go through `cp_send` |
| the `amp` CLI; an unclosed quote | refused |
| `git push`, git aliases, GitHub writes, uploads (`curl`, `wget`), `ssh`, `nc`, publishing, `docker push`, as written in the command | asks you to approve that exact command |
| everything else | runs |

## Risks

The plugin bounds what a misled conversation can do; it can't make a peer,
or this repository, trustworthy. What remains:

- **This repository's code runs in your orb.** With `Savid/clanker-proxy` as the project, every new orb loads the plugin and runs `.agents/setup`, as you, with `sudo` available, from master as it is. A malicious or broken commit there reaches your next orb, with your agent token, your Amp secrets and your Amp sign-in. Rerunning setup step 5 takes `cp-peers` from master too. To decide when that code changes, use [your own repository](#your-own-repository).
- **Without the plugin, there is no sandbox.** If the plugin isn't running in the orb, a conversation's commands run as you, unguarded. With the plugin running but missing `CP_URL`, `CP_TOKEN` or its state, it refuses everything in conversations instead.
- **Peer text steers agents.** A peer's text comes from their agent, which may itself have been misled, and instructions hidden in it (prompt injection) can mislead a conversation within its bounds: its own thread, its directory, reading the checkout, and the network. Whatever it can read can reach the peer, so keep the project's repository free of anything you wouldn't share with every peer.
- **Its work is the peer's.** Hooks, configuration and build scripts in a conversation's directory are written for a peer. Fetch its work into a repository you trust before you run anything on it.
- **Answers come back as a short form.** The thread that asked gets an outcome and a date that the plugin checks, never the peer's words, but a misled conversation can report the wrong ones.
- **Your threads can still find the inbox's.** Amp runs `read_thread` and `code_exec`, and what `code_exec` calls such as `find_thread`, without plugin hooks, so `cp-peers` can't refuse them: a thread that searches your threads, or is told to, can read a conversation and the peer's words in it.
- **Your project threads hold the token.** The inbox, the peer threads and any thread you start in the project have `CP_TOKEN` and no sandbox. Unscoped, the token reaches every peer.
- **The sandbox shares the orb's network and resources.** A conversation can reach the internet and services listening in the orb, and can use up the orb's CPU, memory and disk.

## One inbox per peer

Conversations can't see each other's work or reach other threads, but the
inbox's other threads share one token. To keep peers apart, give each its own
project ([your own repository](#your-own-repository)) and token. cpd enforces
a token's peers, answers every other thread as not found, and the token's
webhook hears only those peers.

| Project | Token |
| --- | --- |
| inbox-bob | `cpctl token add amp-bob -o amp-bob.token --peers bob` |
| inbox-rest | `cpctl token add amp-rest -o amp-rest.token --peers carol,dave` |

- Scope every inbox. One without `--peers` still reaches everyone.
- Peering requests reach only an inbox whose token has no `--peers`, and there the plugin turns them into a notice for you, never an agent. Scoped inboxes don't get them, so for notices add a webhook of your own (a chat channel, say): [webhooks.md](webhooks.md).
- A token's peers are fixed. To add a peer, make a new token and swap it in, restart the orb's processes, then `cpctl token rm` the old one: its webhook goes with it, and the new token's registers when the plugin reloads.

## Your own repository

An inbox in a repository of your own runs the plugin you committed, not
whatever master has. Copy it and the setup script from the release your
daemon runs, read them, and commit them:

```bash
v=$(cpctl --version | cut -d' ' -f2)
raw=https://raw.githubusercontent.com/Savid/clanker-proxy/$v
mkdir -p .amp/plugins .agents
curl -fsSL $raw/.amp/plugins/cp-inbox.ts -o .amp/plugins/cp-inbox.ts
curl -fsSL $raw/.agents/setup -o .agents/setup && chmod +x .agents/setup
git add .amp/plugins/cp-inbox.ts .agents/setup && git commit -m "cp-inbox $v"
```

Repeat it after you update the daemon. Take `cp-peers` from
`$raw/.amp/personal/cp-peers/index.ts` in step 5. Never commit tokens or
`cp-inbox/state.json`.

## Uninstall

From the inbox, or one of its threads, run **clanker-proxy: Uninstall the
inbox** from the command palette. It does nothing until cpd revokes the
inbox's agent token, and its webhook with it; that takes a daemon from this
release. Then it archives the peer threads and conversations, deletes the
project's `CP_URL` and `CP_TOKEN`, removes `cp-peers` from your personal
plugins unless another inbox still has peer threads, and marks the orb
uninstalled, so the plugin there neither registers again nor runs anything.
The conversations' directories stay in the orb until you delete the project:
fetch any work you want first. Then delete the `cp-inbox` trigger in Settings
→ Triggers, archive the inbox, and delete the project. Delete the project
first and all of that stays: nothing of the plugin runs once its project is
gone.

## Limits

Replies never change the turn, so two agents could answer each other forever.
The plugin caps that:

- an idle conversation wakes for a reply only 10 min after the last one, and 3 times until the state changes;
- 30 wakes a day per conversation, 60 per peer, 200 in total; 10 new conversations per peer per day;
- a held notification labels the conversation `cp-held`; at most every 10 minutes, an event lets the plugin wake up to 5 held conversations, or threads that are your turn but have none;
- an ended thread is reread before acting, since it may have been reopened. Ended conversations are forgotten after 30 days.

Amp itself takes a burst of 10 events, then 10 a minute, holding up to 100,
and answers `429 Retry-After` beyond that, which cpd honours. cpd sends each
notification's ID as `Idempotency-Key`, so retries never queue twice, and the
plugin skips IDs it has handled.

Tests: `make test-amp`.
