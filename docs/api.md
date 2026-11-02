# The Robinhood Agentic Trading MCP, as rhap uses it

`rhap` is a deterministic passthrough to Robinhood's **official** Agentic
Trading MCP server. This file records the endpoint, the OAuth 2.1 discovery
chain, the JSON-RPC transport, the CLI-command → MCP-tool mapping, and the
traps — including exactly which argument shapes are **UNVERIFIED** because no
token was available to introspect the live server.

## 1. Endpoint and transport

- **MCP endpoint:** `https://agent.robinhood.com/mcp/trading`
- **Protocol:** MCP over **Streamable HTTP**, JSON-RPC 2.0.
- **Handshake, per invocation.** Each `rhap` run is a fresh process, so every
  networked command runs the full MCP handshake before its real call:
  1. `POST` `initialize` — offers `protocolVersion` `2025-06-18`, `clientInfo`
     `{name: "rhap"}`. Captures any `Mcp-Session-Id` response header.
  2. `POST` the `notifications/initialized` notification (best-effort).
  3. `POST` the real `tools/list` or `tools/call`.
- **Request headers:** `Authorization: Bearer <access_token>`,
  `Content-Type: application/json`,
  `Accept: application/json, text/event-stream`,
  `MCP-Protocol-Version: 2025-06-18`, and `Mcp-Session-Id` once known.
- **Response shape:** a POST may answer with a single JSON object *or* a
  `text/event-stream`. `rhap` handles both — for SSE it reads the `data:`
  payloads and picks the JSON-RPC response whose `id` matches the request.

## 2. OAuth 2.1 + PKCE — discovery chain

Endpoints are **discovered at runtime**, never hardcoded, so a Robinhood-side
move is picked up without a release. Values below were captured from the public,
unauthenticated well-known documents on **2026-08-27** (reproduced verbatim in
`tests/fixtures/`); the code reads them live.

1. **RFC 9728 — protected-resource metadata.**
   `GET https://agent.robinhood.com/.well-known/oauth-protected-resource`
   ```json
   {
     "resource": "https://agent.robinhood.com/mcp/trading",
     "authorization_servers": ["https://agent.robinhood.com/mcp/trading"],
     "scopes_supported": ["internal"],
     "bearer_methods_supported": ["header"]
   }
   ```
2. **RFC 8414 — authorization-server metadata** (served at the host root).
   `GET https://agent.robinhood.com/.well-known/oauth-authorization-server`
   ```json
   {
     "issuer": "https://agent.robinhood.com/mcp/trading",
     "authorization_endpoint": "https://robinhood.com/oauth",
     "token_endpoint": "https://api.robinhood.com/oauth2/token/",
     "registration_endpoint": "https://agent.robinhood.com/oauth/trading/register",
     "code_challenge_methods_supported": ["S256"],
     "grant_types_supported": ["authorization_code", "refresh_token"],
     "response_types_supported": ["code"],
     "scopes_supported": ["internal"],
     "token_endpoint_auth_methods_supported": ["none"]
   }
   ```

Key facts the code depends on:

- **PKCE S256 is mandatory.** rhap generates a 64-byte URL-safe verifier and its
  `base64url(sha256(verifier))` challenge. If the server ever stops advertising
  `S256`, discovery **refuses** rather than sending a plain code.
- **Public client — no secret.** `token_endpoint_auth_methods_supported` is
  `["none"]`, so there is no client secret to store. The `client_id` from
  dynamic registration is a *public* identifier and lives in `config.json`, not
  the keychain.
- **Resource indicator (RFC 8707).** The MCP endpoint URL is sent as `resource`
  on both the authorization and token requests, binding the token to this
  resource. Discovery also asserts the metadata's `resource` matches the
  endpoint rhap intends to reach, and aborts on a mismatch.
- **Discovered URLs are pinned to Robinhood.** `authorization_servers[0]`, the
  metadata's `issuer`, and the authorization, token and registration endpoints
  must each be `https` on `robinhood.com` or a subdomain (no userinfo, default
  port), and the metadata's `issuer` must equal `authorization_servers[0]`
  (RFC 8414 §3.3). Anything else aborts before a credential is sent.

## 3. The login flow (owner-run)

`rhap auth login` runs the authorization-code-with-PKCE flow:

1. Bind a loopback listener on `127.0.0.1:<port>` (`--port 0` = ephemeral);
   the redirect is `http://127.0.0.1:<port>/callback`.
2. **RFC 7591 dynamic client registration** — `POST` the `registration_endpoint`
   with `{client_name, redirect_uris, token_endpoint_auth_method: "none",
   grant_types, response_types, scope}`; store the returned `client_id`.
3. Build the authorization URL (`response_type=code`, `client_id`,
   `redirect_uri`, `code_challenge`, `code_challenge_method=S256`, `state`,
   `scope=internal`, `resource`). Print it; open the browser best-effort.
4. The browser redirects to the loopback with `?code=…&state=…`. rhap validates
   `state` (CSRF guard) and extracts the code.
5. **Token exchange** — `POST` the `token_endpoint`
   (`grant_type=authorization_code`, `code`, `redirect_uri`, `client_id`,
   `code_verifier`, `resource`). Store the token bundle in the keychain.
6. Verify with one `tools/list` call.

Refresh (`grant_type=refresh_token`, `client_id`, `scope`, `resource`) happens
automatically when a stored access token is at/near expiry. Robinhood's
refresh-token lifetime is **undocumented and not carried in the response**, so —
unlike the Schwab CLI — rhap fabricates no local refresh deadline; a rejected
refresh surfaces as an auth error pointing back at `auth login`.

Refresh is serialised across processes: an exclusive `flock` on
`~/.config/rhap/token-refresh.lock` (under `$XDG_CONFIG_HOME` when set), then
the tokens are re-read, and the refresh runs only if they are still expired. A
process that waited on the lock uses the token the holder just stored instead
of spending a rotated refresh token.

## 4. Tool surface and the CLI mapping

The live server exposes **54 tools** (introspected 2026-08-27). **All 54 are
reachable through a typed command**, and the generic `rhap call <tool>` is a
backstop for anything a future server adds. The full tool → command map is in
**§7**; the core surface:

| CLI command | MCP tool(s) | `--account` | Mutating |
| --- | --- | --- | --- |
| `rhap tools list` | `tools/list` (live) | no | no |
| `rhap tools show <NAME>` | — (embedded catalog, offline) | no | no |
| `rhap accounts list` | `get_accounts` | no | no |
| `rhap positions list` | `get_equity_positions` | **required** | no |
| `rhap orders list` | `get_equity_orders` | **required** | no |
| `rhap quotes get <SYM>` | `get_equity_quotes` | no | no |
| `rhap order place` | `review_equity_order` then `place_equity_order` | **required** | **yes** |
| `rhap order cancel <id>` | `cancel_equity_order` | **required** | **yes** |
| `rhap option …` | the option tools (§7) | reads/writes as noted | writes |
| `rhap call <tool>` | any tool by name | per `--args` | guarded |

The access grant is asymmetric: **READ spans every linked account; WRITE reaches
only the funded "Agentic" account.** Live reads return the tool's payload under
`result.data` (with a sibling `result.guide`); rhap surfaces the tool result
verbatim under its DTO's `result`.

### The account is required per-operation — never ambient (hard rule)

Authentication is global (one OAuth session in the keychain serves the machine),
but **the account is not**. Any command that acts against a specific account —
`positions list`, `orders list`, `order place`, `order cancel` — takes a
**required `--account <ID>`** on that invocation. There is **no** global
`--account`, **no** `RHAP_ACCOUNT` env var, **no** `config` key, and **no**
fallback of any kind. `accounts list`, `quotes get`, and `tools list` are not
account-scoped and take no account.

This is a deliberate safety rule. An ambient/default account is a wrong-account
hazard under concurrent CLI use: two commands running at once, a stale exported
env var, or a forgotten config default could silently act on the wrong account —
read the wrong positions, or place/cancel an order there. Requiring the account
per-operation makes every account-scoped call self-describing and impossible to
misdirect by ambient state. It is also why the account number is never
hardcoded: it is identifying, and it belongs on the command line.

`order cancel` also carries the account (not just the order id) so a cancel is
scoped to the intended account and cannot be misdirected.

## 5. Tool argument schemas (VERIFIED against the live `tools/list`, 2026-08-27)

The live server was introspected after the owner's first `auth login`. The
top-level argument schemas below are **verified**; the one exception is the
per-leg sub-schema of `place_option_order` (see the note).

### Equity

| Tool | `arguments` (VERIFIED 2026-08-27) | Built in |
| --- | --- | --- |
| `get_accounts` | `{}` | `commands/reads.rs` |
| `get_equity_positions` | `{ "account_number", "cursor"? }` | `commands/reads.rs::account_scoped_args` |
| `get_equity_orders` | `{ "account_number", "created_at_gte"? }` | `commands/reads.rs::orders_args` |
| `get_equity_quotes` | `{ "symbols": ["SYM"] }` | `commands/reads.rs::quote_args` |
| `place_equity_order` | `{ "account_number", "symbol", "side", "type": "limit", "quantity", "limit_price", "time_in_force" }` | `commands/order.rs::place_args` |
| `cancel_equity_order` | `{ "account_number", "order_id" }` | `commands/order.rs::cancel_args` |

Corrections from the earlier build's guesses, now verified:

- **The order-kind field is `type`, not `order_type`**, and there is **no
  `extended_hours`** field (the optional hours toggle is `market_hours`).
- **`get_equity_orders` filters by `created_at_gte` only** — no `start_date`, no
  end-date filter, no server-side limit. So `orders list` exposes only `--since`
  (→ `created_at_gte`); it does not offer a `--until` or `--limit` that would
  silently do nothing.
- **`--tif day` maps to `gfd`** ("good for day"); `gtc`/`ioc`/`fok` pass through.
- Money and quantity stay strings. No float ever touches an order value.

### Options

| Tool | `arguments` (VERIFIED 2026-08-27) | Built in |
| --- | --- | --- |
| `get_option_positions` | required `[account_number]`; optional `nonzero`, `option_type`, `type`, `expiration_date{,_gte,_lte}`, `chain_ids`, `option_ids`, `cursor` | `commands/options.rs::option_positions_args` |
| `get_option_orders` | required `[account_number]`; optional `created_at_gte`, `state`, `placed_agent`, `chain_ids`, `underlying_type`, `order_id`, `cursor` | `commands/options.rs::option_orders_args` |
| `get_option_quotes` | required `[instrument_ids]` (option instrument ids, **not** symbols) | `commands/options.rs::option_quotes_args` |
| `get_option_chains` | optional `[underlying_symbol, ids]` | `commands/options.rs::option_chains_args` |
| `cancel_option_order` | `{ account_number, order_id }` | `commands/options.rs::cancel_option_args` |
| `place_option_order` | required `[account_number, legs, quantity]`; optional `direction`, `price`, `time_in_force`, `type`, `market_hours`, `ref_id`, `stop_price` | `commands/options.rs::place_option_args` |

CLI mapping notes:

- `option positions list` defaults `nonzero=true` (open positions); pass
  `--nonzero false` to include closed/zero rows.
- `option orders list` maps `--since` → `created_at_gte`, plus `--state` and
  `--placed-agent`.
- `option quotes get --instrument-ids A,B` → `{ instrument_ids: [A, B] }`.
  Option quotes key on **instrument ids**, which `option chains get` resolves.
- **`place_option_order.legs` is the one part not fully verified.** The live
  introspection gave the top-level shape (required `[account_number, legs,
  quantity]`) but not the per-leg fields. `option order place` offers a
  single-leg convenience (`--instrument-id` + `--position-effect open|close` +
  `--side buy|sell` + `--ratio`) that builds a one-element `legs` array in the
  leg shape the server's schema names — VERIFIED 2026-09-02 via
  `tools show place_option_order`: `{option_id: string, position_effect: string,
  side: string, ratio_quantity: integer}` (a live order was refused while the
  ratio went out as text under an `instrument_id` key) — and a raw
  `--legs <JSON>` passthrough for full control (multi-leg). Top-level
  `quantity`, `price`, `stop_price` are strings per the same schema.

## 6. Traps (paid for elsewhere in the family, and specific to this one)

- **Never send a live order to verify a schema.** Use `--dry-run`, which builds
  and prints the exact JSON-RPC request and sends nothing. This is the only
  write path the tests and `make smoke` exercise. A live place/cancel is gated
  by a typed confirmation and exits `6` non-interactively without `--force`. The
  one still-unverified shape is `place_option_order`'s per-leg fields (§5) —
  confirm the leg schema from a real order capture, or use `--legs <JSON>`,
  before relying on the single-leg convenience for a live option order.
- **This is real money with no sandbox.** Robinhood exposes no paper endpoint
  for the Agentic MCP. There is no safe way to end-to-end test a write; treat
  the whole write path as owner-only.
- **Don't trust a `200` alone.** An MCP `tools/call` can succeed at the HTTP
  layer and still carry `isError: true` in the result; rhap surfaces that as an
  upstream error rather than reporting success.
- **SSE vs JSON is per-response.** The same endpoint answers some POSTs with a
  single JSON object and others with an event stream. Parse by `Content-Type`,
  not by assumption.
- **The token is a bearer credential.** It reads across every account and writes
  to the Agentic account. It lives only in the keychain (`piekstra.rhap`), is
  never logged, and `--verbose` prints methods and ids, never the token.

## 7. Complete tool → command map (all 54 tools, schemas VERIFIED-live 2026-08-27)

Every tool below was introspected from the live `tools/list`; the flag names map
to the real `inputSchema` fields. Account-scoped commands take a **required
`--account`**. WRITE commands use the dry-run + confirmation guard. The only
sub-schema still not fully introspected is `place_option_order.legs` (§5); the
single-leg leg fields are a best guess, with `--legs <JSON>` as the passthrough.

### Accounts / equity core
| Command | Tool | Account | Write |
| --- | --- | --- | --- |
| `accounts list` | `get_accounts` | no | no |
| `positions list` | `get_equity_positions` | req | no |
| `orders list [--since→created_at_gte]` | `get_equity_orders` | req | no |
| `quotes get <SYM>` | `get_equity_quotes` | no | no |
| `order place` | `review_equity_order`→`place_equity_order` | req | yes |
| `order cancel <id>` | `cancel_equity_order` | req | yes |

### Equity market data (`equity …`)
| Command | Tool | Account | Write |
| --- | --- | --- | --- |
| `equity fundamentals --symbols` | `get_equity_fundamentals` | no | no |
| `equity historicals --symbols --start-time` | `get_equity_historicals` | no | no |
| `equity price-book --symbols` | `get_equity_price_book` | no | no |
| `equity tax-lots --account --symbol` | `get_equity_tax_lots` | req | no |
| `equity technicals --symbol --type --interval --start-time` | `get_equity_technical_indicators` | no | no |
| `equity tradability --account --symbols` | `get_equity_tradability` | req | no |

### Options (`option …`)
| Command | Tool | Account | Write |
| --- | --- | --- | --- |
| `option positions list [--nonzero]` | `get_option_positions` | req | no |
| `option orders list [--since --state --placed-agent]` | `get_option_orders` | req | no |
| `option quotes get --instrument-ids` | `get_option_quotes` | no | no |
| `option chains get --underlying` | `get_option_chains` | no | no |
| `option instruments list` | `get_option_instruments` | no | no |
| `option historicals get --instrument-ids --start-time` | `get_option_historicals` | no | no |
| `option watchlist` | `get_option_watchlist` | no | no |
| `option score --underlying --expiration [--type --width --class --metric …]` | `get_option_chains` + `get_equity_quotes`/`get_equity_historicals` (equities) or `get_indexes`/`get_index_quotes`/`get_index_historicals` (index underlyings: XSP, SPX, XND …) + `get_option_instruments` (paged) + `get_option_quotes` (×20) | no | no |
| `metrics list` | — (offline registry of metric codenames) | no | no |

### 2026-09-17 surface additions (`news`, `sec`, `alerts`, `order advanced`, `crypto`, `market politician-trades`)

The catalog was re-captured 2026-09-17: the live surface had grown from 54
tools to **78**. Every addition has a typed command (`src/commands/extras.rs`).

| Command | Tool | Account | Write |
| --- | --- | --- | --- |
| `news get --symbol [--limit --cursor]` | `get_equity_news` | no | no |
| `sec index --symbol [--form-type --since --until --cursor]` | `get_sec_filing_index` | no | no |
| `sec filing --filing-id [--section]` | `get_sec_filing` | no | no |
| `sec facts --filing-ids --concepts` | `get_sec_filing_facts` | no | no |
| `sec facts-catalog --filing-id [--concept-contains --axis-name-in --offset]` | `get_sec_filing_facts_catalog` | no | no |
| `alerts list [--symbol --asset-class --cursor]` | `get_alerts` | no | no |
| `alerts log [--asset-class --since --cursor --limit]` | `get_alert_log` | no | no |
| `alerts create --symbol --condition-type [--threshold --asset-class --indicator-json]` | `create_alert` | no | yes |
| `alerts update --alert-id [--enabled --condition-type --threshold --indicator-json]` | `update_alert` | no | yes |
| `alerts delete <ALERT_ID>` | `delete_alert` | no | yes |
| `alerts mark-read [--ids --all-through]` | `mark_alerts_read` | no | yes |
| `order advanced list --account [--order-id --contingency-type --since --cursor]` | `get_advanced_orders` | req | no |
| `order advanced review --account --symbol --side --quantity --take-profit --stop-loss` | `review_advanced_order` | req | no |
| `order advanced place …` | `review_advanced_order`→`place_advanced_order` | req | yes |
| `order advanced cancel <ORDER_ID> --account` | `cancel_advanced_order` | req | yes |
| `market politician-trades [--politician --symbol]` | `get_politician_trades` | no | no |
| `crypto onboarding` | `get_crypto_account_onboarding_info` | no | no |
| `crypto pairs [--cursor --limit]` | `get_currency_pairs` | no | no |
| `crypto quotes --symbols [--timezone --account]` | `get_crypto_quotes` | no | no |
| `crypto positions --account [--cursor]` | `get_crypto_positions` | req* | no |
| `crypto order list --account [--order-id --state --side --symbol --since --cursor]` | `get_crypto_orders` | req* | no |
| `crypto order preview …` | `preview_crypto_order` | req* | no |
| `crypto order place …` | `preview_crypto_order`→`place_crypto_order` | req* | yes |
| `crypto order cancel <ORDER_ID> --account` | `cancel_crypto_order` | req* | yes |

\* crypto tools take the **numeric `rhs_account_number`**, not the
alphanumeric account id (`--account` carries it).

**OCO is the headline.** `place_advanced_order` is a one-cancels-the-other
equity bracket: a take-profit limit leg and a stop-loss stop-market leg on the
same symbol, side and whole-share quantity, `regular_hours` only, both legs
sharing `time_in_force`. This retires the "no advanced-order tools" gap that
forced stops and targets to be watched by session cadence.
| `ark score [--metric ark/2 --window N --funds … --tickers … --top N --collateral $]` | external `arkfunds.io` holdings + `get_equity_quotes` + `get_equity_historicals` | no | no |
| `market regime [--metric regime/1\|2\|3\|4 --symbol SPX --vol-symbol VIX --history N]` | `get_indexes` + `get_index_quotes` + `get_index_historicals` (×2) | no | no |
| `option order place` | `review_option_order`→`place_option_order` | req | yes |
| `option order cancel <id>` | `cancel_option_order` | req | yes |
| `option exercise --account --option-id --quantity` | `exercise_option` | req | yes |
| `option exercise-cancel <option_id> --account` | `cancel_option_exercise` | req | yes |

### Portfolio / P&L / margin
| Command | Tool | Account | Write |
| --- | --- | --- | --- |
| `portfolio get --account` | `get_portfolio` | req | no |
| `pnl trade-history --account` | `get_pnl_trade_history` | req | no |
| `pnl realized --account` | `get_realized_pnl` | req | no |
| `margin equity-upgrade-info --account` | `get_limited_margin_upgrade_info` | req | no |
| `margin option-upgrade-info --account` | `get_option_level_upgrade_info` | req | no |

### Indexes / market / research
| Command | Tool | Account | Write |
| --- | --- | --- | --- |
| `index list` | `get_indexes` | no | no |
| `index quotes --instrument-ids` | `get_index_quotes` | no | no |
| `index historicals --instrument-ids --start-time --interval` | `get_index_historicals` | no | no |
| `market earnings-calendar` | `get_earnings_calendar` | no | no |
| `market earnings-results --symbol` | `get_earnings_results` | no | no |
| `market financials --symbols` | `get_financials` | no | no |
| `search <query>` | `search` | no | no |

### Watchlists (`watchlists …`)
| Command | Tool | Account | Write |
| --- | --- | --- | --- |
| `watchlists list` | `get_watchlists` | no | no |
| `watchlists items --list-id` | `get_watchlist_items` | no | no |
| `watchlists popular` | `get_popular_watchlists` | no | no |
| `watchlists create --display-name` | `create_watchlist` | no | yes |
| `watchlists update --list-id` | `update_watchlist` | no | yes |
| `watchlists add --list-id` | `add_to_watchlist` | no | yes |
| `watchlists remove --list-id` | `remove_from_watchlist` | no | yes |
| `watchlists add-option --option-ids` | `add_option_to_watchlist` | no | yes |
| `watchlists remove-option --option-ids` | `remove_option_from_watchlist` | no | yes |
| `watchlists follow --list-id` | `follow_watchlist` | no | yes |
| `watchlists unfollow --list-id` | `unfollow_watchlist` | no | yes |

### Scanners (`scans …`)
| Command | Tool | Account | Write |
| --- | --- | --- | --- |
| `scans list` | `get_scans` | no | no |
| `scans filter-specs` | `get_scanner_filter_specs` | no | no |
| `scans run --scan-id` | `run_scan` | no | run¹ |
| `scans create` | `create_scan` | no | yes |
| `scans update-config --scan-id` | `update_scan_config` | no | yes |
| `scans update-filters --scan-id --filters` | `update_scan_filters` | no | yes |

¹ `run_scan` returns matches and changes no account state, and the server
annotates it `readOnlyHint: true`, so it is a read on both the typed `scans run`
and the generic `call run_scan`.

## 8. The generic `call`, discoverability, and the embedded catalog

- **`rhap call <TOOL> [--args '<json>'] [--dry-run] [--force]`** invokes any tool
  by name and emits `call/v1` wrapping `result`. Safety, all preserved on this
  generic path and validated against the embedded catalog (so `--dry-run` is
  offline):
  - Unknown tool → **exit 4**.
  - A **mutating** tool — any tool the catalog does not annotate
    `readOnlyHint: true`, including the `review_*`/`preview_*` simulations and
    `delete_alert`/`mark_alerts_read` — needs `--dry-run` or `--force`; a live
    mutation without `--force` → **exit 6**.
  - A mutating **account-scoped** tool (its schema requires `account_number`)
    must carry `account_number` in `--args` → else **exit 2**. No ambient
    account, ever — the same rule the typed commands enforce.
  - Reads pass through freely (the server enforces its own required fields).
- **`rhap tools list [--schemas]`** is the live surface (names + titles, or full
  schemas). **`rhap tools show <NAME>`** prints a tool's full `inputSchema` from
  the embedded catalog, offline (no token), plus its `mutating`/`account_scoped`
  classification — exactly what `call` validates against.
- **The embedded catalog** (`resources/tools_catalog.json`, the 2026-08-27
  `tools/list` snapshot) is the deterministic source for `call` validation and
  `tools show`. Regenerate it if the server surface changes: `rhap tools list
  --json` and re-derive the file.

## 9. Metrics — named, versioned contract scores (`metrics`, `option score`)

The MCP has no notion of a *good* contract. `src/commands/score.rs` publishes
the operator's own metrics under immutable codenames — `family/version` — so
revisions coexist and results stay comparable across time:

| Codename | Applies to | Definition |
| --- | --- | --- |
| `ark/1` | equity tickers held by ARK ETFs (ticker score) | Daily holdings of the six ARK ETFs (arkfunds.io mirror); day-over-day share diffs are trades. accumulation 35 (net bought ÷ held: ≥ 20% full; 10 → 25; 5 → 15; 1 → 8) · persistence 20 (buy-days ÷ sessions reported by ANY fund ≥ 50%/30%/15%) · recency 10 (net buying in last 5 / 10 sessions) · price_proximity 25 (price ÷ ARK buy VWAP ≤ 1.00 full; ≤ 1.05 → 18; ≤ 1.10 → 10; ≤ 1.20 → 4) · conviction 10 (max fund weight ≥ 3% / 1.5% / 0.5%). Trade at ≥ 60. `wheel_feasible` = 100 × 0.9 × price ≤ --collateral; `two_x_vehicle` lists a known 2× ETF. |
| `ark/2` | equity tickers held by ARK ETFs (ticker score) | `ark/1` with three measurement repairs, same factors and points: (a) a position counts as an initiation only if its first row is later than the first date of **its own fund's** file (ark/1 compares against the earliest date of any fund, so one late file fakes initiations); (b) persistence and recency use the sessions reported by **the funds that hold the ticker**; (c) the 10-session recency tier is withdrawn when the last 5 sessions are net selling. Default for `ark score`. |
| `regime/1` | S&P 500 market mode (state) | SPX closes (non-interpolated, live level as today): close vs SMA20/SMA50, SMA20 slope, RV10/RV20, drawdown from the 20-day high; VIX level, 5-day high, 20-day mean; VRP = VIX − RV20. States: panic (VIX ≥ 30 or dd ≤ −7%) → stressed (VIX ≥ 22, dd ≤ −4%, or close < SMA50 with RV10 > 1.3×RV20) → calm-uptrend (close > SMA20 > SMA50, VRP > 0) → calm-range. Handling: size 1.0/0.75/0.5/0.5, delta ceiling 0.35/0.30/0.25/0.20, index-only when stressed/panic, opens in stressed/panic only when VIX ≤ 90% of its 5-day high. |
| `regime/2` | S&P 500 market mode (state) | regime/1 + SMA20/50 spread expansion (calm-uptrend requires it; close < SMA20 with a contracting spread = `rolling-over`: size 0.5, index+sector, Δ ≤ 0.25) + single-day −3% trigger (stressed; panic with VIX ≥ 25) + asymmetric dwell (escalate at once, de-escalate after 3 consecutive calmer sessions). |
| `regime/3` | S&P 500 market mode, composite (state) | regime/2 overlaid with a structural phase from SMA50/SMA200 (spread > 3% expanding = full-bull; ≤ 1% contracting = convergence-exit; ≥ 10 days below SMA50 = correction-exit; contracting = bull-warning; else golden-cross-confirmed; death cross: early-bull / bear-hold / bear-deep < −3%) and a velocity override (≥ 8% below the 10-day high or a −3% day; 10-session cooldown). Bear/velocity phases cap size at 0.5, index only, Δ ≤ 0.25; velocity blocks opens unless vol is easing. `preferred_side`: put-spreads in bull phases, none-or-call-spreads otherwise. Constants are published in the venture's REGIME.md study. With fewer than 205 sessions the phase is `unknown`: restricted like a bear phase (size 0.5, index only, Δ ≤ 0.25), opening still allowed. |
| `regime/4` | S&P 500 market mode, composite (state) | `regime/3` that fails closed: an `unknown` phase **blocks opening outright**, and the structural reason line states the opening decision (`unknown`: blocked; `velocity-exit`: blocked until vol eases). Identical to `regime/3` wherever the 50/200 spread is measurable. |
| `harvest/1` | vertical credit spreads | credit/width 30 · short-leg liquidity 20 (market % of mid + OI bonus) · delta placement 15 (|Δ| 0.22–0.32) · theta/vega 15 (class bar: index 0.25 / sector 0.50 / leveraged 1.50) · probability 10 (chance_of_profit_short) · technical cushion 10 (≥ 1 ATR(14) beyond spot; beyond the 20-day low). Trade at ≥ 65. |

Rules: a published definition never changes — publish `harvest/2`. Every
metric has a scorer in `score()` and an entry in `METRICS`; the
`registry_dispatches_every_published_metric` test keeps them in lockstep.
`option score` emits an `option-score/v1` DTO: one row per spread with both
legs' quotes, `credit_mid`, `max_loss_per_contract`, and
`metrics.<codename> = { score, trade, factors[] }` — the "custom data point"
a strategy reads. Read-only: five tool families, no account argument.
