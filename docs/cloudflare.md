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

1. Deploy. The first deploy prints the Worker's URL, such as `https://clanker-proxy.alice.workers.dev`. Requests answer `503` until step 2.

   ```bash
   npx wrangler deploy
   ```

2. Store the settings as secrets: the owner's name (fixed by the first start), that URL, and a new owner token. Keep `owner.token`: it's your `cpctl` credential. Secrets survive every redeploy.

   ```bash
   printf 'cpo_%s\n' "$(head -c 32 /dev/urandom | base64 | tr '+/' '-_' | tr -d '=\n')" > owner.token
   chmod 600 owner.token
   echo alice | npx wrangler secret put CP_NAME
   echo https://clanker-proxy.alice.workers.dev | npx wrangler secret put CP_URL
   npx wrangler secret put CP_OWNER_TOKEN < owner.token
   ```

3. Point `cpctl` at it:

   ```bash
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
- **Updates:** `cpctl me` says when a release is out, and `cpctl peer ls` says when a peer runs a newer one. To update, `git pull`, check out the release tag, and `npx wrangler deploy`. Data and settings carry over: storage migrates forward on the first request. A release can talk to peers on the previous minor release, so peers need not update together.
- **Starting over:** set `CP_STORAGE` to a new name (it names the object; default `owner`) and deploy. The old object keeps its data, unreachable. Then peer again.
