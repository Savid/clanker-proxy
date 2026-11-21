# clanker-proxy

Rust (edition 2024; toolchain pinned in `rust-toolchain.toml`). `cpd` is a
daemon that exchanges issue-like threads with peers' daemons over one HTTP
API; `cpctl` is its CLI. One core runs on two hosts: a native binary and a
Cloudflare Worker with a Durable Object (`cpd-worker`).

The project is mid-rewrite from Go. Crates land one at a time; the Go
implementation is the behavioural reference at tag `go-final`
(`git show go-final:<path>`), except where the rewrite changes behaviour on
purpose.

## Commands

```bash
make lint test      # fmt, clippy (native + wasm32), cargo-deny, xtask deps, shellcheck; tests
make worker-smoke   # cpd-worker under wrangler dev, plus conformance against it (needs npm ci, worker-build 0.9.0)
make check          # everything CI runs
make fmt
make test-amp       # node tests for the Amp plugin; run when changing it
make generate       # write api/openapi.json from cp-api
make image          # clanker-proxy:local for this machine (needs Docker, musl-tools)
cargo xtask deps    # the pure-crate dependency rule
```

## Trying it by hand

Give each daemon its own data directory, never the owner's `~/.cp`. Peering
needs two, each with a `--url` the other can reach (http only on loopback):

```bash
cargo build -p cpd
CP_DIR=/tmp/a target/debug/cpd --name alice --url http://127.0.0.1:18471 --listen 127.0.0.1:18471 &
CP_DIR=/tmp/b target/debug/cpd --name bob --url http://127.0.0.1:18472 --listen 127.0.0.1:18472 &
curl -s -H "Authorization: Bearer $(cat /tmp/a/owner.token)" http://127.0.0.1:18471/api/v1/me
```

`make worker-dev` serves `cpd-worker` on `http://127.0.0.1:18787`, configured
from `.dev.vars` (see `docs/cloudflare.md`); it peers with native daemons on
loopback like any other.

## Layout

- `crates/cp-thread`: event format, workflow, replay, clocks. Pure.
- `crates/cp-api`: API types, bounded newtypes, the operation table, OpenAPI generation.
- `crates/cp-core`: daemon logic over a synchronous `Sql` trait. Pure, sans-IO.
  `crates/cp-sqlite`: that `Sql` on rusqlite, for `cpd` and tests.
- `crates/cp-http`: the API over the core for any host: routing from the operation
  table, access before the body is read, dispatch, peer calls, delivery.
- `crates/cpd`: native host. `crates/cpd-worker`: Worker + Durable Object host,
  built only for wasm32 (empty elsewhere); lint it with the wasm32 clippy.
- `crates/cpctl`: the CLI. `crates/cp-conformance`: black-box tests over HTTP.
- `xtask`: repository automation. `api/openapi.json`: generated, committed.
- `.amp/plugins/cp-inbox.ts`: the Amp inbox plugin, loaded in this repository's
  Amp threads; `.agents/setup` installs `cpctl` in their orbs.

## API: code first

- `crates/cp-api` is the source of truth: request and response types,
  bounded newtypes, and `Op` (every operation with its method, path and
  access). `api/openapi.json` is generated from it (`make generate`) and
  committed; CI fails when it is stale or vacuum finds a warning.
- To add or change an operation: change the types, the `Op` table in
  `access.rs` and its `#[utoipa::path]` description in `doc.rs`, then
  `make generate`. Access is declared only in `Op::access`; the spec's
  `security` is written from it, and a test fails if the table and the
  documented operations differ.
- Paths under `/api/v1/`, camelCase JSON, times as `YYYY-MM-DDTHH:MM:SSZ`.
  Every error is an RFC 9457 problem from an `ErrorKind`. Inputs are bounded
  types that cannot hold an invalid value; secrets are write-only types
  whose `Debug` is redacted.
- Spec lint exceptions go in `.vacuum.yaml` (whole rules) or
  `.vacuum-ignore.yaml` (one finding), each with a reason.

## Releasing

Bump `version` in the root `Cargo.toml`, merge, then tag `vX.Y.Z` to match.
`.github/workflows/release.yml` checks the tag, runs `make lint test`, builds
static Linux and macOS archives on native runners, publishes them with
`checksums.txt` and `install.sh`, and pushes `ghcr.io/savid/clanker-proxy`
for amd64 and arm64, all attested. A PR that touches the release path runs
all of it except publishing.

## Docs

- README and `docs/` stay compact: what to run, and what it does, in the
  fewest words. No restating `cpctl help`, no history, no filler.
- Diagrams are hand-written SVGs in `docs/img`, on a dark card in the
  shared palette (cyan sender/native, magenta recipient/Worker, amber
  peering/webhooks). Animate only with CSS, stopped under
  `prefers-reduced-motion`; no scripts or external resources.

## Rules

- Pure crates (listed in `xtask/src/main.rs`) never depend on tokio, reqwest,
  rusqlite, worker, getrandom or rand, and never read the clock or
  randomness: hosts pass them in. They must build for
  `wasm32-unknown-unknown`.
- No panics in library code: clippy denies `unwrap`, `expect`, `panic!` and
  indexing outside tests. A panic in the Worker hangs the request and resets
  the Durable Object.
- The core never awaits. Each `Core` call runs in one transaction the host
  opens (`cp_sqlite::run` natively); network I/O happens in the host between
  calls, so multi-step flows re-validate when they commit.
- Thread state changes go in `cp-thread` (`Action::rule`, `Fold`) with a
  table test; `cp-core/tests/simulation.rs` must keep converging.
- Never log or print tokens or the daemon's private key; hold secrets as
  `secrecy::SecretString`.
- Before 1.0, a storage change bumps `BASELINE` in `cp-core/src/schema.rs`
  and earlier data is refused (hard cutover); `MIGRATIONS` starts at 1.0,
  append-only. Patch releases change neither storage nor the wire format.
- Comments say what the code cannot: why, constraints, non-obvious
  consequences. No restating code, no history, no references to plans,
  reviews or tickets.
