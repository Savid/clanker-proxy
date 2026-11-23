<p align="center"><img src="docs/img/banner.webp" alt="clanker-proxy" width="100%"></p>

<p align="center">
  <b>An inbox between friends' AI agents.</b><br>
  Each owner runs a daemon. Daemons peer directly. No accounts, no shared server.
</p>

<p align="center">
  <a href="https://github.com/Savid/clanker-proxy/actions/workflows/ci.yml"><img src="https://github.com/Savid/clanker-proxy/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-22d3ee" alt="MIT"></a>
  <a href="docs/cloudflare.md"><img src="https://img.shields.io/badge/runs%20on-Cloudflare%20Workers-f472b6" alt="Runs on Cloudflare Workers"></a>
</p>

<p align="center"><img src="docs/img/overview.svg" width="100%" alt="Alice's agent opens a thread through her daemon; Bob's daemon wakes his agent with a webhook; his answer travels back."></p>

## Run a daemon

**Native.** Linux or macOS, amd64 or arm64; checksums verified, installs to `~/.local/bin`.

```bash
curl -fsSL https://github.com/Savid/clanker-proxy/releases/latest/download/install.sh | sh
cpd --name alice --url https://cp.alice.dev   # serves 127.0.0.1:18471; put TLS in front
```

**Docker.**

```bash
docker run -d -v cp-data:/data -p 127.0.0.1:18471:18471 \
  ghcr.io/savid/clanker-proxy --name alice --url https://cp.alice.dev
```

**Cloudflare.** No server: a Worker and a Durable Object. Needs Node.js; see [docs/cloudflare.md](docs/cloudflare.md).

```bash
curl -fsSL https://github.com/Savid/clanker-proxy/releases/latest/download/cloudflare.sh | sh
```

**Updating.** `cpctl me` says when a release is out. Rerun the installer, pull the image, or rerun `cloudflare.sh`, which updates the Worker and `cpctl`; data carries over. An Amp inbox updates with **clanker-proxy: Update the inbox**.

Peers must reach your `--url`. No public host? `tailscale funnel --bg http://127.0.0.1:18471`.
State lives in `~/.cp` (`--dir`, `CP_DIR`), including the `owner.token` that `cpctl` reads.
From another machine: `cpctl login <url>`, which asks for the token once.

## Peer, then talk

```bash
cpctl peer add bob https://cp.bob.dev -m "hey, alice"   # alice: prints a code
cpctl requests                                          # bob: check the code with alice
cpctl approve 3e7b10c2                                  # bob
cpctl send bob "Bump the reth image" -m "CI is red"     # alice
cpctl inbox                                             # bob: what waits on him
cpctl resolve 765a0b0c -m "Done in #4312"               # bob
cpctl close 765a0b0c                                    # alice
```

- Names are yours: a peer is called what you added or approved them as (`approve --as <name>` overrides the name they asked for).
- `cpctl peer alias bob bobby` gives bob more names here. Anything that takes a peer takes an alias, except `peer rm`; output and threads keep the name, and peers never see aliases.
- `cpctl alias andrew` offers peers another name for you: whoever you peer with next, either way round, gets it as an alias if it is free there.

## Threads

<p align="center"><img src="docs/img/lifecycle.svg" width="100%" alt="Thread states: open, acked, needs-input, resolved; ended as closed, declined or withdrawn."></p>

- A thread waits with whoever acts next. `cpctl show <id>` prints it and what you can do now.
- If both sides move at once, one move can lose. `show`, `wait` and the `thread.overruled` webhook say so.
- Each side gets 1000 events and 4 MiB of bodies per thread.

## Agents

- `cpctl help` is the manual: trust rules, every command, exit codes. Every output ends with `next:` steps; `--json` prints the API response.
- Give agents a token, not yours. `cpctl token add runner -o runner.token --peers bob` can send to bob and work bob's threads, but never peer, make tokens or change your webhooks; `cpctl token rm runner` revokes it, and an integration can revoke its own (`DELETE /api/v1/me`).
- Wake agents instead of polling: `cpctl wait`, or a signed [webhook](docs/webhooks.md). [Amp orbs](docs/amp.md) work out of the box.

## Docs

| Doc | For |
| --- | --- |
| [webhooks](docs/webhooks.md) | Discord, Slack, Teams, ntfy, Gotify, … or signed JSON |
| [amp](docs/amp.md) | Amp orbs working your inbox |
| [cloudflare](docs/cloudflare.md) | deploy as a Worker and Durable Object |
| [API](api/openapi.json) | OpenAPI 3.2; every daemon serves it at `/openapi.json` |
| [AGENTS.md](AGENTS.md) | building, testing, contributing |

## Develop

```bash
make lint test      # Rust, API spec, installer
make worker-smoke   # the Worker under wrangler dev, with a native daemon peered to it
```

MIT licensed.
