# cpd on Cloudflare

`cpd-worker` is the same daemon as `cpd`, built to WebAssembly. It's a Worker
in front of one SQLite-backed Durable Object per owner. It peers with native
daemons and with other Workers, and needs no server.

<p align="center"><img src="img/cloudflare.svg" width="100%" alt="The Worker checks routes, token kind, body size and the peering rate limit before waking the Durable Object, which holds the daemon's SQLite, streams and delivery alarm; a cron trigger re-arms the alarm."></p>

- **Worker:** serves `/openapi.json`. It refuses unknown routes, the wrong kind of token and bodies over 1 MiB, and rate-limits peering requests (10/min per IP, binding `PEERING_RATE`), all before the object wakes. The object then checks access in full.
- **Durable Object:** the daemon itself. Each core call is one SQLite transaction; event streams live in memory.
- **Delivery** to peers and webhooks runs from the alarm, never inside a request: at most 40 calls per alarm (the free plan allows 50), with the next alarm set at once if more is due.
- **Cron**, every 15 min, re-arms a lost alarm.

## Set up

Needs Node.js 20+ and a Cloudflare account (the free plan is enough):

```bash
curl -fsSL https://github.com/Savid/clanker-proxy/releases/latest/download/cloudflare.sh | sh
```

It does the following:
- logs you in to Cloudflare if needed, and asks for a Worker name (default `clanker`, showing the URL it gets) and your name;
- deploys the prebuilt Worker and stores its settings;
- installs or updates `cpctl` here (`~/.local/bin`, or `CP_INSTALL_DIR`) at the same release;
- prints your URL and your owner token: save that in your password manager, and never give it to an agent. It's also saved to `~/.config/clanker-proxy/<worker>.token` (`clanker.token` by default), readable only by you;
- asks how your agents run (Amp, on this machine, or not sure yet) and says what to do next;
- prints a line to share, `cpctl peer add <you> <your URL>`: whoever runs it asks to connect with you.

Run it again any time to update; your data and settings carry over, unless a release changed storage: then the script says so, and `--start-over` starts on empty storage under the same owner token.

**With an agent:** hand it this page. With no terminal to ask on, the script takes its answers as flags, `sh -s -- --name <owner> [--alias <name>] [--worker <name>] [--agent amp|local|none]`, and never prints secrets, only where it saved them. Before running it, the owner either runs `npx wrangler login`, or provides `CLOUDFLARE_API_TOKEN` (from the **Edit Cloudflare Workers** template) and `CLOUDFLARE_ACCOUNT_ID`. Run it on the owner's own machine: the owner token belongs there.

**Amp inbox:** choose Amp (or pass `--agent amp`). The script makes the agent token and prints one prompt for a new Amp thread: it adds this repository as an Amp project on `a1.tiny` orbs, asks you for two values, starts the inbox, and adds `cp-peers` so any thread can ask your peers. Read [the risks](amp.md#risks) first.

## Local

From a clone of the repository, `make worker-bundle` packs what the release
ships, and `sh scripts/cloudflare.sh --bundle build/dist/clanker-proxy_worker.tar.gz`
deploys it. `make worker-dev` serves `http://127.0.0.1:18787`, configured from
`.dev.vars` (git ignores it):

```bash
CP_NAME=worker
CP_URL=http://127.0.0.1:18787
CP_OWNER_TOKEN_HASH=<sha256 of your owner token, in hex>
```

`make worker-smoke` starts it with a throwaway token, probes it, and runs the
conformance suite against it with a native daemon. CI runs it on every change.

## Operate

- **Logs:** one JSON line per request (request ID, method, path, status, ms), never queries, headers or bodies. The ID comes from `X-Request-Id` or Cloudflare's ray ID and is echoed back. Keep logs with `observability` in `wrangler.jsonc`.
- **Cost:** an open stream (`cpctl wait`, `watch`) keeps the object in memory. A full day of one stream is about 11,000 GB-s, inside the free daily allowance.
- **Starting over:** rerun the script with `--start-over`, which gives the object a new name (`CP_STORAGE`; default `owner`) when this release cannot read its data. The old object keeps its data, unreachable. Then peer again, and make agent tokens again: an Amp inbox needs its new `CP_TOKEN`.
