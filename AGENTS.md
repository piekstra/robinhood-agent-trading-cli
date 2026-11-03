# AGENTS.md — working agreement for `robinhood-agent-trading-cli`

Canonical agent guide for this repo. `CLAUDE.md` is a thin pointer here.

## What this is

`rhap` (Robinhood Agentic Passthrough) is a deterministic, LLM-free client for
Robinhood's **official** Agentic Trading MCP server
(`https://agent.robinhood.com/mcp/trading`): JSON-RPC 2.0 over Streamable HTTP,
OAuth 2.1 + PKCE. It maps CLI arguments 1:1 onto MCP tool calls and returns
JSON. No model, no strategy, no autonomy — a thin, auditable pipe.

Conforms to `piekstra-cli/1` (see `~/Dev/cli-common/DESIGN.md` and
`conformance.md`) **with one deliberate deviation: no `self-update`**. The
shared `self-update` installed an unverified download and re-signed it with the
identity the keychain trusts (audit 2026-09-23, security #5), so rhap removed
it; a new binary comes from `make install` with the owner present. rhap is not
listed in cli-common's `conformance.md`; this paragraph is the record of the
deviation. Shared behavior comes from the `pk-cli-*` crates pinned to tag
**v0.20.0** — reuse before writing (output, dates, `RangeArgs`, keychain,
config, the JWT `token` helpers, `RefreshLock`).

## Write-capable — the single most important fact

The MCP grants **READ across all linked accounts** and **WRITE to the funded
Agentic account**. This is **not** a read-only CLI, and the docs must never say
it is. `order place`/`order cancel` reach the write tools against **real money;
there is no sandbox**. Three guards, all enforced in code:

1. **`--dry-run`** builds and prints the exact JSON-RPC request and sends
   nothing — no keychain read, no network. The only write path the tests and
   `make smoke` exercise.
2. **Confirmation** (SPEC v1 §mutations): a live write prompts unless
   `--force`, and exits `6` when it can't ask (`--json`, a pipe, no TTY). A
   write is any tool the catalog does not annotate `readOnlyHint: true`.
3. **Review-then-place**: every place tool calls its review tool first
   (`catalog::REVIEWED_PLACES` in `src/catalog.rs`); `run_write` looks the
   review up from that table, so no call site can omit it.

**Never send a live order** — not to test, not to verify a guessed schema. If a
future change needs to, that is the owner's call, at the owner's hand.

## Build / test / lint

```console
$ make verify     # fmt-check + clippy -D warnings + test + smoke — the CI gate
$ make dev        # debug build, re-signed for stable keychain grants
$ make install    # installs to ~/.cargo/bin and re-signs
```

Tests are **fully offline**: no network, no keychain, no live MCP. The pure
pieces (PKCE, the authorization URL, callback parsing, discovery parsing,
JSON-RPC/SSE handling, order-argument building, token parsing) are unit-tested;
`make smoke` exercises the write path through `--dry-run`.

## Layout

| Path | Owns |
| --- | --- |
| `src/main.rs` | clap tree, dispatch, early argument validation, exit codes |
| `src/config.rs` | constants + non-secret settings (public client id; the no-ambient-account rule) |
| `src/credential.rs` | the `TokenSet` and its keychain storage |
| `src/token_refresh.rs` | cross-process refresh: `pk_cli_auth`'s `RefreshLock` on `token-refresh.lock`, re-read, refresh only if still expired |
| `src/catalog.rs` | the embedded tool catalog and the read / review / write classification (`readOnlyHint`) |
| `src/discovery.rs` | OAuth metadata discovery (RFC 9728 + 8414) |
| `src/oauth.rs` | PKCE, authorization URL, registration, token exchange/refresh, loopback |
| `src/mcp.rs` | JSON-RPC 2.0 Streamable-HTTP client + pure wire helpers |
| `src/commands/*.rs` | one module per command group |
| `docs/api.md` | the endpoint, discovery chain, tool mapping, **and the traps** |

## Non-negotiables

- **Never send a live order.** Build and test writes via `--dry-run` only.
- **`--json` on every command**, exactly one DTO per invocation, `schema`
  tagged. Human output to stdout, diagnostics to stderr.
- **Exit codes**: 0 ok · 2 usage · 3 auth · 4 not found · 5 upstream, or the
  token-refresh lock busy past its wait (nothing sent, safe to retry) ·
  6 confirmation required.
- **Validate args before touching keychain or network**, so `--help`, bad
  input, and a write-verb typo never prompt, hang, or hit the provider.
- **Secrets only** from the OS keychain (`piekstra.rhap`), stdin, or a named env
  var — never argv, never a file, never a log. The token is the one secret; the
  OAuth `client_id` is a public identifier and lives in config.
- **The account is required per-operation — never ambient (hard rule).** Auth is
  global (keychain), but any command that acts on a specific account
  (`positions list`, `orders list`, `order place`, `order cancel`) takes a
  **required `--account <ID>`** on that invocation. There is **no** global
  `--account`, **no** `RHAP_ACCOUNT` env var, **no** `config` key, and **no**
  fallback. An ambient/default account is a wrong-account hazard under concurrent
  use (two commands at once, a stale exported var, a forgotten default) — it
  could silently read or trade the wrong account. Do not reintroduce any
  defaulting path. The account number is never hardcoded, either; it is
  identifying and belongs on the command line.
- **ISO `YYYY-MM-DD`** on every date flag.

## Honesty rules (schemas are verified — with one exception)

- **Tool names and argument schemas are VERIFIED** against the live `tools/list`
  (2026-08-27) for the top-level arguments — including that equity place uses
  `type` (not `order_type`) with no `extended_hours`, and `get_equity_orders`
  filters by `created_at_gte` only. The **one** still-unverified shape is
  `place_option_order`'s per-leg sub-schema; keep that note honest, keep the
  builders tested, and reconcile a leg from a real order capture (or use
  `--legs <JSON>`) rather than presenting the single-leg guess as fact.
- **Don't advertise a flag the tool can't honor.** `get_equity_orders` has no
  end-date/limit, so `orders list` exposes only `--since` — never a silent
  no-op `--until`/`--limit`.
- **Never report success from a status code alone.** A `tools/call` can return
  HTTP 200 with `isError: true`; surface it as an error.
- **PKCE S256 is required.** If discovery stops advertising it, refuse rather
  than send a plain code.

## Fixtures

`tests/fixtures/` is **synthetic**. The OAuth discovery documents are public
infrastructure metadata (real endpoint URLs — not personal data). Every token,
account, symbol, and client id is a dummy; the per-tool `inputSchema` shapes are
verified (2026-08-27), bar the option leg sub-schema. A test enforces the
synthetic-value policy positively
and scans every tracked source for personal data. Never commit a real capture —
a `tools/call` result carries live account data and a token response is a
working credential.

## Definition of done

`make verify` green, CI green, the change dogfooded through the **installed**
binary where it doesn't require a live order, `docs/api.md` matching reality
(including what is still UNVERIFIED), `--json` and human output in sync, and no
secrets or personal data anywhere in the diff — fixtures included.
