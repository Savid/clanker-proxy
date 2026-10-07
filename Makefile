.DEFAULT_GOAL := help
.PHONY: help build image run check lint lint-go lint-api lint-install generate generate-check vuln tidy-check fmt test test-install test-amp release-check clean

BIN_DIR ?= build/bin
VERSION ?= $(shell git describe --tags --always --dirty 2>/dev/null || echo dev)
# GOWORK=off: never pick up a go.work from a parent directory.
GO := GOWORK=off go
GO_BUILD := GOWORK=off CGO_ENABLED=0 go build -trimpath -ldflags="-s -w -X main.version=$(VERSION)"
LISTEN ?= 127.0.0.1:18471
# The spec linter, run with go run so CI needs only Go.
VACUUM := github.com/daveshanley/vacuum@v0.30.6

## help: list targets
help:
	@sed -n 's/^## //p' $(MAKEFILE_LIST)

## build: compile cpd and cpctl into build/bin
build:
	$(GO_BUILD) -o $(BIN_DIR)/cpd ./cmd/cpd
	$(GO_BUILD) -o $(BIN_DIR)/cpctl ./cmd/cpctl

## image: build the clanker-proxy:local container image (cpd and cpctl)
image:
	docker build -t clanker-proxy:local --build-arg VERSION=$(VERSION) .

## run: build and start cpd on LISTEN (default 127.0.0.1:18471)
run: build
	$(BIN_DIR)/cpd -listen $(LISTEN)

## check: lint, generated-code check, tests, govulncheck and tidy check
check: lint generate-check test vuln tidy-check

## lint: golangci-lint on Go, vacuum on the spec, shellcheck on the installer
lint: lint-go lint-api lint-install

lint-go:
	GOWORK=off golangci-lint run ./...

lint-api:
	$(GO) run $(VACUUM) lint -d -r .vacuum.yaml api/openapi.yaml

lint-install:
	shellcheck scripts/*.sh

## generate: regenerate api/rest (ogen) from api/openapi.yaml
generate:
	$(GO) generate ./api/...

## generate-check: fail if committed generated code differs from the spec's output
generate-check: generate
	@test -z "$$(git status --porcelain -- api/rest)" || \
		{ git status --short -- api/rest; echo "generated code is stale: run make generate and commit it"; exit 1; }

## vuln: report known vulnerabilities in the Go dependency graph
vuln:
	$(GO) tool govulncheck ./...

## tidy-check: fail if go.mod or go.sum would change under go mod tidy
tidy-check:
	$(GO) mod tidy -diff
	$(GO) mod verify

## fmt: format Go sources
fmt:
	GOWORK=off golangci-lint fmt ./...

## test: Go race tests and offline installer tests
test: test-install
	$(GO) test -race ./...

## test-install: exercise the installer without network access
test-install:
	sh scripts/install_test.sh

## test-amp: run Amp plugin regression tests (requires Node.js 24)
test-amp:
	node --test examples/amp/cp-inbox.test.mjs

## release-check: build release archives locally without publishing (requires GoReleaser)
release-check:
	GOWORK=off goreleaser check
	GOWORK=off goreleaser release --snapshot --clean

## clean: remove build output
clean:
	rm -rf build
