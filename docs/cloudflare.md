# cpd on Cloudflare

`cpd-worker` is the same daemon as `cpd`, built to WebAssembly. It's a Worker
in front of one SQLite-backed Durable Object per owner. It peers with native
daemons and with other Workers, and needs no server.

<p align="center"><img src="img/cloudflare.svg" width="100%" alt="The Worker checks routes, token kind, body size and the peering rate limit before waking the Durable Object, which holds the daemon's SQLite, streams and delivery alarm; a cron trigger re-arms the alarm."></p>

- **Worker:** serves `/openapi.json`. It refuses unknown routes, the wrong kind of token and bodies over 1 MiB, and rate-limits peering requests (10/min per IP, binding `PEERING_RATE`), all before the object wakes. The object then checks access in full.
- **Durable Object:** the daemon itself. Each core call is one SQLite transaction; event streams live in memory.
- **Delivery** to peers and webhooks runs from the alarm, never inside a request: at most 40 calls per alarm (the free plan allows 50), with the next alarm set at once if more is due.
- **Cron**, every 15 min, re-arms a lost alarm.

## Deploy

Needs a Cloudflare account, the toolchain in `rust-toolchain.toml`, and Node.

```bash
rustup target add wasm32-unknown-unknown
cargo install worker-build --version 0.9.0 --locked
npm ci
```

1. Name the owner and the URL peers will use, in `wrangler.jsonc`. The name is fixed by the first start.

   ```jsonc
   "vars": { "CP_NAME": "alice", "CP_URL": "https://clanker-proxy.alice.workers.dev" },
   ```

2. Make the owner token and store it as a secret. Keep the file: it's your `cpctl` credential.

   ```bash
   printf 'cpo_%s\n' "$(head -c 32 /dev/urandom | base64 | tr '+/' '-_' | tr -d '=\n')" > owner.token
   chmod 600 owner.token
   npx wrangler secret put CP_OWNER_TOKEN < owner.token
   ```

3. Deploy, then point `cpctl` at it:

   ```bash
   npx wrangler deploy
   export CP_URL=https://clanker-proxy.alice.workers.dev CP_TOKEN="$(cat owner.token)"
   cpctl me
   ```

A missing or malformed setting makes every request answer `503` saying what
to fix. The Worker logs say the same, and never contain the token.

## Local

`make worker-dev` serves `http://127.0.0.1:18787`, configured from
`.dev.vars` (git ignores it):

```bash
CP_NAME=worker
CP_URL=http://127.0.0.1:18787
CP_OWNER_TOKEN=cpo_...
```

`make worker-smoke` starts it with a throwaway token, probes it, and runs the
conformance suite against it with a native daemon. CI runs it on every change.

## Operate

- **Logs:** one JSON line per request (request ID, method, path, status, ms), never queries, headers or bodies. The ID comes from `X-Request-Id` or Cloudflare's ray ID and is echoed back. Keep logs with `observability` in `wrangler.jsonc`.
- **Cost:** an open stream (`cpctl wait`, `watch`, the Amp plugin) keeps the object in memory. A full day of one stream is about 11,000 GB-s, inside the free daily allowance.
- **Upgrades** keep the data unless the storage schema version changed. There are no migrations: an old object answers `503` and says so. To start empty, set `CP_STORAGE` to a new name (it names the object; default `owner`) and deploy. The old object keeps its data, unreachable. Then peer again.
