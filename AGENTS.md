# clanker-proxy

Go 1.27. `cpd` is a daemon that exchanges issue-like threads with peers'
daemons over one OpenAPI-defined HTTP API; `cpctl` is its CLI client.

## Commands

Run Go through `make` (it sets `GOWORK=off`; outside make, set it yourself).

```bash
make lint test       # golangci-lint + vacuum on the spec; go test -race. Run before finishing.
make generate        # regenerate api/rest from api/openapi.yaml (ogen)
make check           # what CI runs: lint, generate-check, test, govulncheck, tidy-check
make fmt             # gofumpt + goimports via golangci-lint
make build           # build/bin/cpd, build/bin/cpctl
```

To try a change by hand, give the daemon its own data directory so it never
touches the owner's `~/.cp`:
`CP_DIR=$(mktemp -d) build/bin/cpd -name dev -listen 127.0.0.1:18473`, then
`CP_DIR=<same dir> CP_URL=http://127.0.0.1:18473 build/bin/cpctl …`.

## Layout

- `api/openapi.yaml`: the spec, source of truth. `api/rest`: ogen server and client generated from it.
- `pkg/thread`: event format, workflow (`Rules`, `after`), replay, Lamport clock. Pure.
- `internal/inbox`: threads, peering, secrets, errors with a `Kind`.
- `internal/store`: SQLite. `internal/delivery`: drains the outbox to other daemons.
- `internal/server`: security handlers and operations. `internal/httpserve`: listener, middleware, SSE.
- `cmd/cpd`: flags and wiring. `cmd/cpctl`: the CLI, a pure API client; `e2e_test.go` runs it against real daemons.

## API: spec first

- To add or change an operation: edit `api/openapi.yaml`, `make generate`,
  implement the method on `operations` in `internal/server` (the build fails
  until you do), add a test. Never hand-edit `api/rest`; commit it with the
  spec change (`make generate-check` fails otherwise).
- Access is declared in the spec: owner token by default,
  `security: [peerSecret: []]` for peers, `security: []` for public.
  `internal/server/security.go` enforces it before handlers and
  `TestAccessLevels` checks every operation. Never check credentials in a
  handler; only the hand-routed stream checks the owner token itself.
- ogen cannot serve `text/event-stream`: the stream is hand-routed in
  `internal/server/stream.go`, read in `cmd/cpctl/stream.go`, and
  `TestEveryOperationIsServed` fails until every operation is routed.
- Paths under `/api/v1/`, camelCase JSON, UTC times. Every operation's only
  error response is `default` (RFC 9457 `Problem`); handlers return
  `internal/inbox` errors whose `Kind` maps to the status.
- Bound every input with the shared schemas (`Name`, `ID`, `Title`, `Body`,
  `Labels`, `DaemonURL`). Spec lint exceptions go in `.vacuum.yaml` with a reason.

## Invariants

- A thread's state is a pure replay of its event set, ordered by Lamport
  `clock`, then ID. Never order events by `at`: it is the author's clock and
  ogen encodes it to the second. Owner events take `Thread.NextClock`.
- Workflow changes go only in `pkg/thread` (`Rules`, `after`) with a table test.
- `pkg/` imports nothing from `internal/`, `api/`, `net/http` or
  `database/sql`, and never calls `time.Now`, timers or `rand` (depguard,
  forbidigo, `internal/testutil/importguard`). Pass time and randomness in.
- `internal/store` is the only package that touches SQLite. `cpctl` imports no
  `internal/` package outside tests. New import restrictions go in
  `boundaries` in `internal/testutil/importguard/boundary_test.go`.
- `thread.Event` is the stored format. On the wire it is the spec's `Event`,
  without `from`/`to` (the peer secret says who sent it); change it,
  `delivery.wire` and `server.DeliverEvent` together.
- Delivery scheduling reads `delivery.Config.Now`; tests inject a clock
  instead of sleeping.

## Go

- Packages take an injected `*slog.Logger`; use its `…Context` methods when a
  context is in scope. Never log or print tokens or peer secrets, including
  as flag defaults, and never log the request ID by hand (the context adds it).
- Fix lint findings. A suppression is one line, names the linter and says
  why: `//nolint:<linter> // why`.

## Rules

- Hard cutover: no backward compatibility, migrations, shims or deprecated paths.
- Comments say what code cannot; no restating the code, no history, no
  references to plans, reviews or tickets.
