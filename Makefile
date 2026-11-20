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
lint: lint-rust lint-deps lint-install ## All linters

.PHONY: lint-rust
lint-rust: ## rustfmt and clippy, natively and for WebAssembly
	$(CARGO) fmt --all --check
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings
	$(CARGO) clippy --package cpd-worker $(WASM) --locked -- -D warnings

.PHONY: lint-deps
lint-deps: ## Advisories, licenses, sources, and the pure-crate dependency rule
	cargo deny check
	$(CARGO) xtask deps

.PHONY: lint-install
lint-install: ## shellcheck the scripts
	shellcheck scripts/*.sh

.PHONY: fmt
fmt: ## Format Rust code
	$(CARGO) fmt --all

.PHONY: test
test: test-rust test-install ## Rust and installer tests

.PHONY: test-rust
test-rust: ## Rust tests
	$(CARGO) nextest run --workspace --locked --no-tests=pass

.PHONY: test-install
test-install: ## Offline installer tests
	sh scripts/install_test.sh

.PHONY: test-amp
test-amp: ## Node tests for the Amp plugin example
	node --test examples/amp/cp-inbox.test.mjs

.PHONY: worker-smoke
worker-smoke: ## Run cpd-worker under wrangler dev and probe it (needs npm ci)
	sh scripts/worker_smoke.sh

.PHONY: worker-dev
worker-dev: ## Run cpd-worker locally on 127.0.0.1:18787
	npx wrangler dev --port 18787 --ip 127.0.0.1 --test-scheduled

.PHONY: clean
clean: ## Remove build output
	$(CARGO) clean
	rm -rf build crates/cpd-worker/build .wrangler
