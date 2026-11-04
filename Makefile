# rhap CLI — task runner: build/test/lint/fmt/clean/install, a cheap `smoke`
# check, and an aggregate `verify` gate. Thin wrappers over cargo so a green
# local run predicts a green CI run.

BIN := rhap
CARGO := cargo

.PHONY: all build release test lint fmt fmt-check clean install deps smoke verify dev

all: verify

# Every target that produces a binary signs it. Anything you actually run
# against the keychain — including `./target/release/$(BIN)` during testing —
# needs the stable identity, or each rebuild mints a fresh ad-hoc one and macOS
# asks for "Always Allow" again.
build: SIGN_TARGET = target/debug/$(BIN)
build:
	$(CARGO) build
	@$(SIGN)

release: SIGN_TARGET = target/release/$(BIN)
release:
	$(CARGO) build --release --locked
	@$(SIGN)

test:
	$(CARGO) test --all --locked

lint:
	$(CARGO) clippy --all-targets --locked -- -D warnings

fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all -- --check

clean:
	$(CARGO) clean

# `cargo install` ad-hoc signs, which gives the binary a *new* code identity
# every time. macOS scopes keychain "Always Allow" grants to that identity, so
# an unsigned reinstall silently revokes them and the next run prompts again.
# Re-signing with the stable shared identity keeps one grant valid across every
# future install.
# `--locked` builds from Cargo.lock, the versions `verify` tested; without it
# `cargo install` re-resolves to the newest semver-compatible crates.
install: SIGN_TARGET = $${CARGO_INSTALL_ROOT:-$$HOME/.cargo}/bin/$(BIN)
install:
	$(CARGO) install --locked --path . --force
	@$(SIGN)

deps:
	$(CARGO) fetch

# Debug build re-signed with the same stable identity, so the dev loop doesn't
# re-prompt either.
dev: SIGN_TARGET = target/debug/$(BIN)
dev:
	cargo build
	@$(SIGN)

# Shared re-signing step. No-ops with a note when the helper or identity is
# absent (CI, Linux, a fresh machine that hasn't run setup-dev-signing.sh).
define SIGN
if [ -x "$$HOME/Dev/cli-common/scripts/dev-sign.sh" ]; then \
	"$$HOME/Dev/cli-common/scripts/dev-sign.sh" "$(SIGN_TARGET)"; \
else echo "cli-common/scripts/dev-sign.sh not found — $(SIGN_TARGET) left ad-hoc signed"; fi
endef

# Cheap sanity checks needing no config, network, or keychain. Deliberately no
# `auth status` here: it reads keychain entries, which on macOS puts up a
# permission dialog per item for a freshly-signed binary. The write path is
# exercised through `--dry-run`, which builds and prints the JSON-RPC request
# without a keychain read or a network call. Keychain/MCP behaviour is covered
# by dogfooding the installed binary, not by the offline gate.
smoke: release
	./target/release/$(BIN) --version
	./target/release/$(BIN) --help > /dev/null
	./target/release/$(BIN) info > /dev/null
	./target/release/$(BIN) mcp --help > /dev/null
	./target/release/$(BIN) mcp --list-tools > /dev/null
	./target/release/$(BIN) metrics list --json > /dev/null
	! ./target/release/$(BIN) market regime --metric harvest/1 --json > /dev/null 2>&1
	! ./target/release/$(BIN) ark score --metric harvest/1 --json > /dev/null 2>&1   # a score metric is refused (exit 2) before any network
	./target/release/$(BIN) order place --account SMOKE --symbol SAMPL --side buy --qty 1 --limit 1.00 --tif day --dry-run --json > /dev/null
	./target/release/$(BIN) order place --account SMOKE --symbol SAMPL --side sell --qty 0.5 --type market --tif day --dry-run --json > /dev/null
	./target/release/$(BIN) order place --account SMOKE --symbol SAMPL --side buy --dollar-amount 100.00 --type market --tif day --dry-run --json > /dev/null
	! ./target/release/$(BIN) order place --account SMOKE --symbol SAMPL --side sell --qty 0.5 --limit 1.00 --tif day --dry-run --json > /dev/null 2>&1   # fractional + limit is refused at argument time, not by a broker 400
	! ./target/release/$(BIN) order place --account SMOKE --symbol SAMPL --side buy --qty 1 --dollar-amount 100 --type market --tif day --dry-run --json > /dev/null 2>&1   # both sizing fields is a usage error
	./target/release/$(BIN) order cancel SAMPLE-ORDER-ID --account SMOKE --dry-run --json > /dev/null
	./target/release/$(BIN) option order place --account SMOKE --quantity 1 --instrument-id INST-1 --position-effect open --side buy --type limit --price 1.00 --dry-run --json > /dev/null
	./target/release/$(BIN) option order cancel SAMPLE-ORDER-ID --account SMOKE --dry-run --json > /dev/null
	./target/release/$(BIN) tools show get_portfolio --json > /dev/null
	./target/release/$(BIN) order advanced place --account SMOKE --symbol SAMPL --side sell --quantity 10 --take-profit 12.00 --stop-loss 8.00 --dry-run --json > /dev/null
	./target/release/$(BIN) crypto order place --account 12345 --symbol BTC --side buy --type market --dollar-amount 100.00 --dry-run --json > /dev/null
	./target/release/$(BIN) alerts create --symbol SAMPL --condition-type price_below --threshold 1.00 --dry-run --json > /dev/null
	./target/release/$(BIN) call place_equity_order --args '{"account_number":"SMOKE","symbol":"SAMPL","side":"buy","type":"limit"}' --dry-run --json > /dev/null

verify: fmt-check lint test smoke
