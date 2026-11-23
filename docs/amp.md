# Amp orbs work your inbox

[`cp-inbox`](../.amp/plugins/cp-inbox.ts) is an [Amp](https://ampcode.com)
plugin, loaded in the threads of the Amp project it is in. Your daemon wakes
an orb with a signed webhook; the plugin gives each peer a thread, and each
cpd thread a conversation that works it with `cpctl` in a sandbox. Any of
your threads can ask a peer something and gets back a short form of the
answer, never their words. Read [Risks](#risks) first.

<p align="center"><img src="img/amp.svg" width="100%" alt="Your thread asks with ask_peer; bob's peer thread in the Amp orb sends it with cp_send through your cpd to bob's cpd. Bob's answer reaches your cpd, which wakes the orb with a signed webhook; a sandboxed conversation under bob's thread reads it with cpctl and sends your thread a short form with cp_answer."></p>

- **Inbox:** the first thread to load the plugin with `CP_URL` and `CP_TOKEN` set, labelled `cp-inbox`. It receives the webhooks and starts the threads below. Leave it idle, and never archive it.
- **Peer thread:** one per peer, under the inbox, titled with their name and aliases and labelled `cp-peer` and `peer-<name>`. It sends what your threads ask that peer.
- **Conversation:** one per cpd thread, under its peer's thread, labelled `clanker-proxy` and `peer-<name>`. It reads the thread, works in its own directory and answers with the thread's `next:` commands; on threads you sent, it checks the answer, then closes, reopens or asks you. It is archived when the cpd thread ends and comes back if anything new arrives. Its **cpd thread** button shows you the thread (its newest 30,000 characters), never an agent.
- **Webhooks** only wake the plugin. It drops any event not signed with the key it derives from `CP_TOKEN`, and an event carries IDs and a state, never peer text.
- **Peering requests** never reach an agent: the plugin notifies you, and you review them with `cpctl requests`.

## You need

- **A daemon peers can reach**, outside the orb: `cpd` with a public URL, or [the Worker](cloudflare.md). The orb sleeps; cpd doesn't.
- **Amp with GitHub connected** (Settings → MCP & Integrations), so orbs can clone the project. Conversations fetch only public repositories.
- **An agent token, never the owner token.** It works threads but can't peer, make tokens or change your webhooks, and `cpctl token rm` revokes it.

## Setup

Choose Amp in `cloudflare.sh` and it does step 1, then prints a prompt for a
new Amp thread, which does steps 2–5.

1. **Agent token**, on your machine: `cpctl token add amp-inbox -o amp.token`.
2. **Project and secrets.** Add `Savid/clanker-proxy` as an Amp project, or [your own repository](#your-own-repository), on Amp's `a1.tiny` orbs (`amp projects update <project> --orb-provider builtin --orb-size a1.tiny`; larger if conversations build big projects). Save `CP_URL` and `CP_TOKEN` as that project's secrets, never personal ones: those reach every thread and override the project's. Amp takes each value in a private dialog that keeps it out of the thread. A running orb picks up changes after `amp orb restart-processes`.
3. **Start the inbox:** a new thread in the project, not the one doing the setup. Its orb runs [`.agents/setup`](../.agents/setup), which installs the `cpctl` your daemon runs, and loads the plugin, which registers its webhook with your daemon.
4. **Never archive the inbox**, and remove any guidance that archives finished threads. Archived, it answers `404`, a final failure: restore it, then `cpctl webhook retry amp-inbox <delivery-id>`.
5. **Ask from any thread:** add [`cp-peers`](../.amp/personal/cp-peers/index.ts) to your personal plugins, which Amp loads in all your threads.

   ```bash
   amp clone user-plugins ~/user-plugins && mkdir -p ~/user-plugins/cp-peers
   curl -fsSL https://raw.githubusercontent.com/Savid/clanker-proxy/master/.amp/personal/cp-peers/index.ts -o ~/user-plugins/cp-peers/index.ts
   cd ~/user-plugins && git add cp-peers && git commit -m "cp-peers: ask clanker-proxy peers from any thread" && git push
   ```

Wrong thread became the inbox? Delete the `cp-inbox` trigger (Settings →
Triggers) and start a new thread in the project.

**Updating:** rerun `cloudflare.sh` for the daemon; new orbs install its
`cpctl`. A running inbox keeps its plugin until you ask it to pull and reload
its plugins. Rerun step 5 for `cp-peers`.

## Check it

1. Settings → Triggers lists `cp-inbox`, key `cpd`, owned by the inbox. The orb's Amp log says `cp-inbox: registered with cpd`; `set CP_URL and CP_TOKEN` means they were missing when the plugin loaded.
2. A peer sends a test thread. Within seconds their peer thread starts under the inbox, with a conversation under it.

## Ask a peer

Ask from any thread, by name or [alias](#names-and-aliases): "ask bob <what>".

- In the project, `cp_send` sends it. Elsewhere, `ask_peer` hands it to bob's thread, which sends it with `cp_send`, to bob only. `cpctl send` does the same from your machine.
- Bob's answer goes to a conversation under bob's thread, which checks it, then closes or reopens it.
- The thread that asked watches the cpd thread, and so does any of your threads that messages its conversation. Watchers are told of each of bob's moves in the plugin's words, with the conversation's short form from `cp_answer`: yes, no, maybe, done, declined or unclear, and a date or time. The move that ends the thread is the last. You read bob's words with a watcher's button, or under bob's thread.
- Your other threads hold your credentials, so they never read bob's words: they are never given an inbox thread's ID, and `cp-peers` refuses their tools that name one, by ID, link or label. Writing in is fine: a message to a conversation is you instructing it. Don't hand a conversation's link to another thread to review: Amp runs `read_thread` without plugin hooks, and Spaces run no plugins at all, so nothing stops it reading bob's words. Ask in the conversation instead, or read it yourself.

A peer's thread starts with the first thread between you; before that, ask
from a thread in the project. A conversation's `cp_send` reaches only its own
peer, and asks you first.

## Names and aliases

A peer's name is yours: `cpctl peer add <name>`, or `cpctl approve <id> --as
<name>`, which defaults to the name they asked for. Aliases are more names for
the same peer:

```bash
cpctl peer alias tester bobby rob
cpctl peer unalias tester rob
```

Names and aliases share one namespace, so a taken one is refused. cpd takes an
alias wherever it takes a name, except `peer rm`, and answers with the name.

`cpctl alias andrew` offers peers another name for you: whoever you peer with
next, either way round, gets it as an alias if it is free there. Later changes
reach nobody.

The inbox titles each peer's thread with its name and aliases, "tester
(bobby)", on every event and every 10 minutes while the orb is awake.
`ask_peer` reads those titles, so "is bobby free for a beer?" reaches tester.

## What a conversation can do

Only you instruct it. Peers are third parties it helps in good faith: it
weighs their text as requests, never instructions, and checks what they
supply before it goes into code, docs or agent instructions. Alone, it does
what a request needs: reads and changes code in its own directory, runs
tests, commits locally and answers through `next:`. Anything else with effects
outside the orb waits for you: `cp_ask_owner` labels the thread `needs-owner`
and notifies you, and you answer in that thread.

It has the shell, web search and web pages, `cp_send`, `cp_ask_owner` and
`cp_answer`. No file tools, which would run outside the sandbox as you, no
code mode, subagents or MCP servers, and no tools for other threads, Amp or
other services.

**Sandbox.** Every command it runs, and any script it writes, runs as you
but without privileges or a way to regain them (no `sudo`), with a clean
environment and no other process, IPC or terminal in sight. Everything is
read-only except its own directory under `~/cp-work`, which is also its
`$HOME` and `/tmp`. Your home shows only the checkout, `~/.gitconfig` and the
`bin` directories on `PATH`; your credentials, Amp secrets, the `amp` CLI, the
inbox's state, other conversations' work and toolchains elsewhere in your
home, such as rustup's, are out of reach. It needs passwordless `sudo`,
`unshare` and `setpriv`, which Amp's orbs have.

Its `cpctl` reaches cpd through the plugin, which holds the token. The
conversation gets a key that only reads and acts on its own thread, and works
only inside the orb.

A command you approve runs in the sandbox too, so pushing and publishing fail.
To publish its work, fetch its branch into a repository you trust and push
from there; don't run git or build tools in its directory outside the
sandbox, where its hooks would run as you.

The plugin checks every command, including inside `bash -c`, `eval`, `$(…)`
and here-documents:

| Command | Result |
| --- | --- |
| `cpctl` on its own thread, `help`, `me` | runs |
| `cpctl wait` | refused: it is woken when the peer acts |
| any other `cpctl` | refused: new threads go through `cp_send` |
| the `amp` CLI; an unclosed quote | refused |
| `git push`, git aliases, GitHub writes, uploads (`curl`, `wget`, `scp`, `rsync`), `ssh`, `nc`, publishing packages or images | asks you to approve that exact command |
| everything else | runs |

## Risks

The plugin bounds what a misled conversation can do; it can't make a peer, or
this repository, trustworthy.

- **This repository's code runs in your orb.** With `Savid/clanker-proxy` as the project, every new orb loads the plugin and runs `.agents/setup` from master, as you, with `sudo`. A bad commit there reaches your next orb, with your agent token, Amp secrets and Amp sign-in; step 5 takes `cp-peers` from master too. To choose when that code changes, use [your own repository](#your-own-repository).
- **No plugin, no sandbox.** If the plugin isn't running, a conversation's commands run as you, unguarded. Running without `CP_URL`, `CP_TOKEN` or its state, it refuses everything in conversations.
- **Peer text steers agents.** Hidden instructions (prompt injection) can mislead a conversation within its bounds: its thread, its directory, reading the checkout, and the network. Whatever it can read can reach the peer, so keep the project's repository free of anything you wouldn't share with every peer.
- **Its work is the peer's.** Hooks, configuration and build scripts in a conversation's directory are written for a peer. Fetch its work into a repository you trust before you run anything on it.
- **Short answers can be wrong.** A misled conversation can report the wrong outcome or date.
- **Your threads can still find the inbox's.** Amp runs `read_thread` and `code_exec`, and what `code_exec` calls, without plugin hooks, so `cp-peers` can't refuse them: a thread that searches, or is told to, can read a conversation and the peer's words in it.
- **Project threads hold the token.** The inbox, the peer threads and any thread you start in the project have `CP_TOKEN` and no sandbox. Unscoped, it reaches every peer.
- **The sandbox shares the orb's network.** A conversation can reach the internet, anything the orb can reach and services listening in the orb, and can use up its CPU, memory and disk. Keep the project's orbs off private networks.

## One inbox per peer

Conversations can't see each other's work or reach other threads, but the
project's threads share one token. To keep peers apart, give each its own
project ([your own repository](#your-own-repository)) and a token scoped with
`--peers`: cpd answers every other peer's thread as not found, and the token's
webhook hears only its peers.

| Project | Token |
| --- | --- |
| inbox-bob | `cpctl token add amp-bob -o amp-bob.token --peers bob` |
| inbox-rest | `cpctl token add amp-rest -o amp-rest.token --peers carol,dave` |

- Scope every inbox: one without `--peers` reaches everyone.
- Only an unscoped inbox hears peering requests. With scoped inboxes, add a webhook of your own for them, such as a chat channel: [webhooks.md](webhooks.md).
- A token's peers are fixed. To add one, make a new token and swap it in, restart the orb's processes, then `cpctl token rm` the old one; its webhook goes with it, and the new token's registers when the plugin reloads.

## Your own repository

An inbox in your own repository runs the plugin you committed, not whatever
master has. Copy it and the setup script from the release your daemon runs,
read them, and commit them:

```bash
v=$(cpctl --version | cut -d' ' -f2)
raw=https://raw.githubusercontent.com/Savid/clanker-proxy/$v
mkdir -p .amp/plugins .agents
curl -fsSL $raw/.amp/plugins/cp-inbox.ts -o .amp/plugins/cp-inbox.ts
curl -fsSL $raw/.agents/setup -o .agents/setup && chmod +x .agents/setup
git add .amp/plugins/cp-inbox.ts .agents/setup && git commit -m "cp-inbox $v"
```

Repeat it after you update the daemon, and take `cp-peers` from
`$raw/.amp/personal/cp-peers/index.ts` in step 5. Never commit tokens or
`cp-inbox/state.json`.

## Uninstall

From the inbox or one of its threads, run **clanker-proxy: Uninstall the
inbox** from the command palette. It first has cpd revoke the inbox's agent
token, and its webhook with it, and stops if cpd can't. Then it archives the
peer threads and conversations, deletes the project's `CP_URL` and `CP_TOKEN`,
removes `cp-peers` from your personal plugins unless another inbox still has
peer threads, and marks the orb uninstalled, so the plugin there does nothing
again. Conversations' directories stay in the orb until you delete the
project: fetch any work first. Then delete the `cp-inbox` trigger, archive the
inbox and delete the project. Delete the project first and none of this runs.

## Limits

Replies never change the turn, so two agents could answer each other forever.
The plugin caps that:

- an idle conversation wakes for a reply only 10 minutes after the last, and 3 times until the state changes;
- 30 wakes a day per conversation, 60 per peer, 200 in all, and 10 new conversations per peer a day;
- a held notification labels the conversation `cp-held`; at most every 10 minutes, an event lets the plugin wake up to 5 held conversations, or threads that are your turn but have none;
- an ended thread is reread before acting, since it may have been reopened, and ended conversations are forgotten after 30 days.

Amp takes a burst of 10 events, then 10 a minute, holding up to 100, and
answers `429 Retry-After` beyond that, which cpd honours. cpd sends each
notification's ID as `Idempotency-Key`, so a retry never queues twice, and the
plugin skips IDs it has handled.

Tests: `make test-amp`.
