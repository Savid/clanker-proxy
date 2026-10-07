# clanker-proxy

Go 1.27. `cpd` is a daemon that exchanges issue-like threads with peers'
daemons over one OpenAPI-defined HTTP API; `cpctl` is its CLI client.

## Commands

Run Go through `make`; it sets `GOWORK=off`, which you must set yourself
outside it.

```bash
make lint test   # golangci-lint, vacuum on the spec, go test -race. Run before finishing.
make generate    # regenerate api/rest from api/openapi.yaml (ogen)
make check       # CI: lint, generate-check, test, govulncheck, tidy-check
make fmt         # gofumpt + goimports
make build       # build/bin/cpd, build/bin/cpctl
```

## Trying it by hand

Give each daemon its own data directory, never the owner's `~/.cp`. Peering
needs two, each with a `-url` the other can reach:

```bash
CP_DIR=/tmp/a build/bin/cpd -name alice -url http://127.0.0.1:18471 -listen 127.0.0.1:18471 &
CP_DIR=/tmp/b build/bin/cpd -name bob -url http://127.0.0.1:18472 -listen 127.0.0.1:18472 &
CP_DIR=/tmp/a CP_URL=http://127.0.0.1:18471 build/bin/cpctl peer add bob http://127.0.0.1:18472
```

## Layout

- `api/openapi.yaml`: the spec, source of truth. `api/rest`: generated from it.
- `pkg/thread`: event format, workflow (`Rules`, `after`), replay, clocks. Pure.
- `internal/inbox`: threads, peering, secrets; errors carry a `Kind`.
- `internal/store`: SQLite. `internal/delivery`: drains the outbox to peers.
- `internal/server`: security and operations. `internal/httpserve`: listener, middleware, SSE.
- `cmd/cpd`: flags and wiring. `cmd/cpctl`: the CLI; `e2e_test.go` runs it against real daemons.

## API: spec first

- To change an operation: edit `api/openapi.yaml`, `make generate`,
  implement it on `operations` in `internal/server` (the build fails until
  you do), add a test. Never hand-edit `api/rest`; commit it with the spec.
- Access is declared in the spec: owner token by default,
  `security: [peerSecret: []]` for peers, `security: []` for public.
  `internal/server/security.go` enforces it before handlers;
  `TestAccessLevels` checks every operation. Never check credentials in a
  handler; only the hand-routed stream checks the owner token itself.
- ogen cannot serve `text/event-stream`, so the stream is hand-routed in
  `internal/server/stream.go` and read in `cmd/cpctl/stream.go`.
- Paths under `/api/v1/`, camelCase JSON, UTC times. Every operation's only
  error response is `default` (RFC 9457 `Problem`); handlers return
  `internal/inbox` errors whose `Kind` sets the status. Delivery treats a
  peer's 400, 409, 413 and 422 as final and retries anything else.
- Bound every input with the shared schemas (`Name`, `ID`, `Title`, `Body`,
  `Labels`, `DaemonURL`). Spec lint exceptions go in `.vacuum.yaml` with a reason.

## cpctl is for agents

- Its users are coding agents. `cpctl help` (the `overview` in `main.go` and
  the command table in `commands.go`) is their only manual: a new command
  goes in the table with a summary, an example, and `about` for anything an
  agent would otherwise guess.
- Text output ends with `next:` steps (`output.go`); failures carry an exit
  code and a hint (`classify` in `client.go`). `TestHelp` and
  `TestAgentOutput` hold this.
- API responses let a client act without knowing the workflow: each thread
  has `role` and `actions`, derived in `server.summary` from
  `thread.Allowed`, and refusals say what is allowed instead.

## Invariants

- A thread's state is a pure replay of its events, ordered by Lamport
  `clock`, then ID, never by `at` (the author's clock, to the second).
  Owner events take `Thread.NextClock`; peer events must pass
  `Thread.CheckClock`.
- Workflow changes go only in `pkg/thread` (`Rules`, `after`), with a table test.
- `pkg/` never imports `internal/`, `api/`, `net/http` or `database/sql`,
  and never reads the clock or randomness; pass them in. Only
  `internal/store` touches SQLite; `cpctl` imports no `internal/` package
  outside tests. Import rules live in
  `internal/testutil/importguard/boundary_test.go` and `.golangci.yml`.
- `thread.Event` is the stored format. On the wire it is the spec's `Event`
  without `from`/`to`; change it, `delivery.wire` and
  `server.DeliverEvent` together.
- Delivery scheduling reads `delivery.Config.Now`; tests inject a clock
  instead of sleeping.

## Rules

- Never log or print tokens or peer secrets, including as flag defaults.
  Never log the request ID by hand; the context adds it.
- Hard cutover: no backward compatibility, migrations, shims or deprecated paths.
- Comments say what the code cannot: why, constraints, non-obvious
  consequences. No restating code, no history, no references to plans,
  reviews or tickets.
