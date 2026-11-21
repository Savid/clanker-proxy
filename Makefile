.DEFAULT_GOAL := help

CARGO ?= cargo
WASM := --target wasm32-unknown-unknown

.PHONY: help
help: ## List targets
	@grep -E '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  %-14s %s\n", $$1, $$2}'

.PHONY: build
build: ## Build the native workspace (release)
	$(CARGO) build --workspace --release --locked

.PHONY: check
check: lint test worker-smoke test-amp ## Everything CI runs

.PHONY: lint
lint: lint-rust lint-deps lint-api lint-install ## All linters

.PHONY: lint-rust
lint-rust: ## rustfmt and clippy, natively and for WebAssembly
	$(CARGO) fmt --all --check
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings
	$(CARGO) clippy --package cpd-worker $(WASM) --locked -- -D warnings

.PHONY: lint-deps
lint-deps: ## Advisories, licenses, sources, and the pure-crate dependency rule
	cargo deny check
	$(CARGO) xtask deps

.PHONY: generate
generate: ## Write api/openapi.json from cp-api
	$(CARGO) xtask openapi

.PHONY: lint-api
lint-api: ## The committed OpenAPI document is current and lints clean (needs npm ci)
	$(CARGO) xtask openapi --check
	npx vacuum lint --fail-severity warn -r .vacuum.yaml --ignore-file .vacuum-ignore.yaml api/openapi.json

.PHONY: lint-install
lint-install: ## shellcheck the scripts
	shellcheck scripts/*.sh .agents/setup

.PHONY: fmt
fmt: ## Format Rust code
	$(CARGO) fmt --all

.PHONY: test
test: test-rust test-install test-setup ## Rust, installer and setup-script tests

.PHONY: test-rust
test-rust: ## Rust tests (nextest, plus doctests)
	$(CARGO) nextest run --workspace --locked --no-tests=pass
	$(CARGO) test --workspace --doc --locked

.PHONY: test-install
test-install: ## Offline installer tests
	sh scripts/install_test.sh

.PHONY: test-setup
test-setup: ## Offline tests of the Cloudflare setup script (needs Node and python3)
	sh scripts/cloudflare_test.sh

.PHONY: test-amp
test-amp: ## Node tests for the Amp plugin
	node --test .amp/*.test.mjs

.PHONY: image
image: ## Build the clanker-proxy:local container image for this machine (needs Docker and musl-tools)
	sh scripts/image.sh

.PHONY: worker-build
worker-build: ## Build cpd-worker to WebAssembly (needs worker-build 0.9.0)
	cd crates/cpd-worker && worker-build --release

.PHONY: worker-bundle
worker-bundle: worker-build ## Pack build/dist/clanker-proxy_worker.tar.gz, deployable with Node alone
	sh scripts/worker_bundle.sh

.PHONY: worker-smoke
worker-smoke: worker-build ## Run cpd-worker under wrangler dev, probe it, and run conformance against it (needs npm ci)
	sh scripts/worker_smoke.sh

.PHONY: worker-dev
worker-dev: ## Run cpd-worker locally on 127.0.0.1:18787 (settings in .dev.vars; see docs/cloudflare.md)
	npx wrangler dev --port 18787 --ip 127.0.0.1 --test-scheduled

.PHONY: clean
clean: ## Remove build output
	$(CARGO) clean
	rm -rf build crates/cpd-worker/build .wrangler
