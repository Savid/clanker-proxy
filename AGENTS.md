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
make worker-smoke   # cpd-worker under wrangler dev (needs npm ci and worker-build 0.9.0)
make check          # everything CI runs
make fmt
make test-amp       # node tests for examples/amp; run when changing it
make generate       # write api/openapi.json from cp-api
cargo xtask deps    # the pure-crate dependency rule
```

## Layout

- `crates/cp-thread`: event format, workflow, replay, clocks. Pure.
- `crates/cp-api`: API types, bounded newtypes, the operation table, OpenAPI generation.
- `crates/cp-core`: daemon logic over a synchronous `Sql` trait. Pure, sans-IO.
- `crates/cp-http`: host-agnostic HTTP service built from the operation table.
- `crates/cpd`: native host. `crates/cpd-worker`: Worker + Durable Object host.
- `crates/cpctl`: the CLI. `crates/cp-conformance`: black-box tests over HTTP.
- `xtask`: repository automation. `api/openapi.json`: generated, committed.

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

## Rules

- Pure crates (listed in `xtask/src/main.rs`) never depend on tokio, reqwest,
  rusqlite, worker, getrandom or rand, and never read the clock or
  randomness: hosts pass them in. They must build for
  `wasm32-unknown-unknown`.
- No panics in library code: clippy denies `unwrap`, `expect`, `panic!` and
  indexing outside tests. A panic in the Worker hangs the request and resets
  the Durable Object.
- The core never awaits. Network I/O happens in the host between core calls,
  so multi-step flows re-validate when they commit.
- Never log or print tokens or peer secrets; hold them as
  `secrecy::SecretString`.
- Hard cutover: no backward compatibility, migrations, shims or deprecated
  paths. A schema change bumps the schema version; old data is refused.
- Comments say what the code cannot: why, constraints, non-obvious
  consequences. No restating code, no history, no references to plans,
  reviews or tickets.
