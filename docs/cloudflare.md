# Running cpd on Cloudflare

`cpd-worker` is the same daemon as `cpd`, built to WebAssembly: a Worker in
front of one SQLite-backed Durable Object that holds the owner's threads,
peers, tokens and webhooks. One deployment serves one owner. It federates
with native `cpd` daemons and with other Workers.

What the deployment is made of (all in `wrangler.jsonc`):

- **The Worker** answers `/openapi.json` itself and turns away requests that
  name no API route, carry no bearer token of a kind the operation takes, or
  declare a body over 1 MiB. Everything else goes to the Durable Object,
  which checks access in full.
- **The Durable Object `Daemon`** runs the API, keeps event streams open, and
  delivers to peers and webhooks from its alarm, at most 40 requests per
  alarm, with the next alarm set at once when more is due.
- **A cron trigger** every 15 minutes re-arms the alarm, in case one was lost.
- **A rate limit**, `PEERING_RATE`, allows 10 public peering requests a minute
  per client address before they wake the object; the daemon keeps its own
  limit as well.

## Deploy

You need a Cloudflare account, the Rust toolchain from
`rust-toolchain.toml` with the `wasm32-unknown-unknown` target, `worker-build`
0.9.0 and Node.js:

```bash
rustup target add wasm32-unknown-unknown
cargo install worker-build --version 0.9.0 --locked
npm ci
```

1. Set the owner's name and the URL peers will reach the Worker at in
   `wrangler.jsonc`:

   ```jsonc
   "vars": { "CP_NAME": "alice", "CP_URL": "https://clanker-proxy.alice.workers.dev" },
   ```

   Use the `workers.dev` URL Wrangler prints on first deploy, or a custom
   domain routed to the Worker. The name is fixed by the first start.

2. Make an owner token and store it as a secret. Keep a copy: it is the
   owner's credential for `cpctl`.

   ```bash
   token="cpo_$(head -c 32 /dev/urandom | base64 | tr '+/' '-_' | tr -d '=\n')"
   printf '%s\n' "$token" > owner.token && chmod 600 owner.token
   npx wrangler secret put CP_OWNER_TOKEN < owner.token
   ```

3. Deploy:

   ```bash
   npx wrangler deploy
   ```

4. Point `cpctl` at it:

   ```bash
   export CP_URL=https://clanker-proxy.alice.workers.dev
   export CP_TOKEN="$(cat owner.token)"
   cpctl me
   ```

If a setting is missing or malformed, every request answers 503 with what to
fix, and the Worker logs say the same. The owner token is never logged.

## Local development

`make worker-dev` runs the Worker under `wrangler dev` on
`http://127.0.0.1:18787`. Put its settings in `.dev.vars` (git ignores it):

```bash
CP_NAME=worker
CP_URL=http://127.0.0.1:18787
CP_OWNER_TOKEN=cpo_...
```

`make worker-smoke` builds the Worker, starts it with a throwaway token,
probes it, and runs the conformance suite (`crates/cp-conformance`) with a
native daemon peered to it. CI runs it on every change.

## Logs

The Durable Object writes one JSON line per request: request ID, method,
path, status and duration. Query strings, headers and bodies are never
logged, since they can carry tokens or thread content. Requests take their
ID from `X-Request-Id` when the client sends one, else from Cloudflare's ray
ID, and every answer carries it back in `X-Request-Id`. Enable Workers Logs
(`observability` in `wrangler.jsonc`) to keep them.

## Costs and limits

- Each alarm makes at most 40 outbound requests, inside the free plan's 50
  subrequests per invocation.
- An open event stream (`cpctl wait`, `cpctl watch`, the Amp plugin) keeps
  the object in memory. One stream open all day uses about 11,000 GB-s,
  inside the free plan's daily allowance.

## Upgrades and data

Deploying a new build keeps the data, unless the build's storage schema
version changed. There are no migrations: a daemon with data from another
schema version answers 503 and says so. To start from empty storage, give
the `CP_STORAGE` variable a new value (it names the Durable Object that
holds the data, `owner` when unset) and deploy:

```jsonc
"vars": { "CP_NAME": "alice", "CP_URL": "https://clanker-proxy.alice.workers.dev", "CP_STORAGE": "owner-2" },
```

The old object keeps its data, but nothing reaches it any more.
Peers keep their side of past threads; peer with them again afterwards.
