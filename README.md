# rhap — Robinhood Agentic Passthrough

A deterministic, **LLM-free** command-line client for Robinhood's **official**
[Agentic Trading MCP server](https://agent.robinhood.com/mcp/trading). It maps
CLI arguments 1:1 onto MCP tool calls (JSON-RPC 2.0 over Streamable HTTP,
authenticated with OAuth 2.1 + PKCE) and returns JSON. No model, no autonomy,
no decisions — a thin, auditable pipe to the MCP.

Conforms to `piekstra-cli/1` (see [`cli-common`](https://github.com/piekstra/cli-common)),
except that `self-update` is deliberately absent (see AGENTS.md).

> ### ⚠️ Write-capable — this is NOT a read-only tool
> Robinhood's Agentic MCP grants **READ across all linked accounts** and
> **WRITE (place / cancel orders) to the funded "Agentic" account**. `rhap order
> place` and `rhap order cancel` reach the write tools against **real money —
> there is no sandbox.** Every write is protected three ways: `--dry-run` builds
> and prints the request without sending; a live run requires a typed
> confirmation (or `--force`) and exits `6` non-interactively; and every order
> is reviewed before it is placed. Treat live orders as owner-only.

> [!CAUTION]
> **Use at your own risk.** This software is provided as is, without warranty
> of any kind (see the licenses). You are responsible for every order it
> sends on your account. Unofficial; not affiliated with or endorsed by
> Robinhood Markets, Inc.

## Install

Requires Rust **1.89** or newer (`rust-version` in `Cargo.toml`; the shared
token-refresh lock needs it).

```console
$ make install     # cargo install --locked --path . + stable-identity code signing
```

Or build from source: `make release` (binary at `target/release/rhap`).

## First-time setup (owner-run)

Authentication is a real browser round trip and yours alone to complete:

```console
$ rhap auth login                 # opens a browser; prints the URL always
$ rhap auth status                # token presence/expiry, no request spent
```

`auth login` discovers the OAuth endpoints, self-registers a public client
(RFC 7591), runs the PKCE authorization-code flow via a loopback listener, and
stores the token bundle in the OS keychain (`piekstra.rhap`). See
[`docs/api.md`](docs/api.md) for the full flow. There is **no** account to
configure — see below.

## The account is required per-operation — never ambient

Authentication is global, but the account is not. Any command that acts against
a specific account takes a **required `--account <ID>`** on that invocation.
There is deliberately **no** global `--account`, **no** `RHAP_ACCOUNT` env var,
and **no** `config` default. An ambient/default account is a wrong-account
hazard under concurrent CLI use — a second command, a stale exported var, or a
forgotten default could silently act on the wrong account. Naming the account
per-operation makes every account-scoped call self-describing.

`accounts list`, `quotes get`, and `tools list` are not account-scoped and take
no `--account`.

## Usage

```console
$ rhap tools list                        # discover the live MCP tool surface
$ rhap accounts list                     # get_accounts (all linked accounts)
$ rhap positions list --account <ID>     # get_equity_positions (account required)
$ rhap orders list --account <ID> --since 2026-08-01   # --since → created_at_gte
$ rhap quotes get AAPL                    # get_equity_quotes (not account-scoped)

# WRITE — dry run first, always (account required):
$ rhap order place --account <ID> --symbol AAPL --side buy --qty 1 --limit 200.00 --tif day --dry-run
$ rhap order place --account <ID> --symbol AAPL --side buy --qty 1 --limit 200.00 --tif day   # prompts
$ rhap order cancel <ORDER_ID> --account <ID>                                                  # prompts

# Options
$ rhap option positions list --account <ID>            # open positions (nonzero=true)
$ rhap option orders list --account <ID> --state filled
$ rhap option chains get --underlying AAPL             # resolve instruments
$ rhap option quotes get --instrument-ids <ID1,ID2>    # quote by instrument id
$ rhap option order place --account <ID> --quantity 1 \
    --instrument-id <ID> --position-effect open --side buy --type limit --price 1.25 --dry-run
$ rhap option order place --account <ID> --quantity 1 --legs '[{...},{...}]' --dry-run   # multi-leg
$ rhap option order cancel <ORDER_ID> --account <ID>

# Metrics — the operator's own contract scores, versioned by codename (offline registry)
$ rhap metrics list                                     # harvest/1 … (factors + weights)
$ rhap option score --underlying SPY --expiration 2026-10-16 --width 2 --class index --json
$ rhap option score --underlying TQQQ --expiration 2026-10-16 --class leveraged --top 5
$ rhap market regime --metric regime/4 --history 30      # composite regime with a 30-session replay

# 2026-09-17 additions (78-tool surface)
$ rhap news get --symbol NVDA --limit 10
$ rhap sec index --symbol CBRS --form-type 8-K,S-1 --since 2026-08-01
$ rhap alerts log --since 2026-09-17T09:30:00Z          # poll-only alert stream
$ rhap order advanced place --account <ID> --symbol SPY --side sell --quantity 10 \
    --take-profit 780.00 --stop-loss 700.00 --tif gtc --dry-run   # OCO bracket
$ rhap market politician-trades --symbol NVDA
$ rhap crypto quotes --symbols BTC,ETH
$ rhap ark score --collateral 1600 --top 15             # ark/2: ARK Invest accumulation + price vs her buy VWAP
$ rhap option score --underlying XSP --expiration 2026-10-16 --width 2 --json   # index underlyings resolve via the index tools
```

`--json` on every command emits one `schema`-tagged DTO on stdout (diagnostics
to stderr). Standard family commands — `info`, `completions <shell>`,
`config` — are all present. `self-update` is deliberately absent: it installed
an unverified download; install from source with `make install`.

## How it maps

| Command | MCP tool |
| --- | --- |
| `tools list` | `tools/list` |
| `accounts list` | `get_accounts` |
| `positions list` | `get_equity_positions` |
| `orders list` | `get_equity_orders` |
| `quotes get <sym>` | `get_equity_quotes` |
| `order place` | `review_equity_order` → `place_equity_order` |
| `order cancel <id>` | `cancel_equity_order` |
| `option positions list` | `get_option_positions` |
| `option orders list` | `get_option_orders` |
| `option quotes get` | `get_option_quotes` |
| `option chains get` | `get_option_chains` |
| `option order place` | `review_option_order` → `place_option_order` |
| `option order cancel <id>` | `cancel_option_order` |
| `portfolio`/`pnl`/`margin`/`equity`/`index`/`market`/`search`/`watchlists`/`scans`/`option …` | the rest of the 54 tools |
| `call <tool> [--args …]` | any tool by name (escape hatch) |

**All 54 live MCP tools are reachable** — via a typed command, or the generic
`rhap call <tool> --args '<json>'`. The full tool → command map is in
[`docs/api.md §7`](docs/api.md); the `call` safety rules and discoverability
(`tools list --schemas`, `tools show <NAME>`) are in §8.

Tool names and argument schemas are **VERIFIED against the live `tools/list`
(2026-08-27)**. The one exception is `place_option_order`'s per-leg sub-schema,
still a best guess — use the single-leg convenience flags or `--legs <JSON>`.
`call` refuses an unknown tool (exit 4), a live mutation without `--force`
(exit 6), and a mutating account-scoped tool with no `account_number` in its
args (exit 2) — the no-ambient-account rule holds even on the generic path.
A mutation is any tool the catalog does not annotate `readOnlyHint: true`,
including the `review_*`/`preview_*` simulations and `delete_alert`/
`mark_alerts_read`; see [`docs/api.md`](docs/api.md) §8.

## Use with Claude

`rhap mcp` serves five read-only tools to Claude over MCP. **No tool can place, change or cancel
an order, exercise an option, move money or change a watchlist.** Those stay on this command line,
behind their guards.

| Tool | Answers |
|---|---|
| `rhap_accounts` | Your brokerage accounts and their numbers |
| `rhap_portfolio` | One account's value by asset type and buying power, and its realized profit and loss if asked |
| `rhap_positions` | One account's open positions of one kind |
| `rhap_orders` | One account's orders of one kind, newest first |
| `rhap_quotes` | Current quotes for up to 20 stock symbols |

The account is never ambient here either: each tool takes it on each call, and `kind: crypto` takes
the account's numeric `rhs_account_number`. Concise results drop nulls, empty values and links;
`response_format: detailed` keeps them. What the tools return
goes to Claude, including account numbers, balances, holdings and orders
([`SECURITY.md`](SECURITY.md)).

**Claude Code.** `rhap auth login` first: the server reads the session from the keychain. Then
add the `robinhood-agent-trading-cli` plugin from the
[piekstra plugin marketplace](https://github.com/piekstra/claude-plugins):

```console
$ claude plugin marketplace add piekstra/claude-plugins
$ claude plugin install robinhood-agent-trading-cli@piekstra-plugins
```

or register the server yourself:

```console
$ claude mcp add rhap -- rhap mcp
```

**Claude Desktop.** Run `rhap auth login` in a terminal, then download `rhap-<version>.mcpb` from
the [latest release](https://github.com/piekstra/robinhood-agent-trading-cli/releases/latest) and
open it. It asks for nothing: the bundled `rhap` reads the session your installed `rhap` stored,
and renews it. On macOS the bundled binary is a different program to the keychain, so its first
call raises a keychain prompt for each `piekstra.rhap` item it reads; choose **Always Allow**, or
every call will ask again.

Any other MCP client that launches a local server can run `rhap mcp`.

**Claude on the web and mobile.** claude.ai reaches tools through a remote connector. `rhap` keeps
your session on your computer, so it serves the same tools from your computer to a connector you
deploy into your own Cloudflare account:

```console
$ rhap mcp --new-token            # once: a bearer token, saved in the keychain and printed for the Worker
$ rhap mcp --http --port 8773     # serve on 127.0.0.1 only
```

`--http` takes the token from `$RHAP_MCP_TOKEN` if it is set, and otherwise from the keychain.
`rhap mcp --new-token --overwrite` replaces a saved one.
The connector, [cli-common's `connector/`](https://github.com/piekstra/cli-common/tree/main/connector#tools-from-your-computer-home-modules),
reaches that port through a Cloudflare Tunnel in your account; its README walks through the
tunnel, the VPC service, a launchd agent that keeps the server running, and the `HOME_RHAP_TOKEN`
secret, with the threat model. `--http` binds 127.0.0.1 only, refuses to start without the token,
refuses any request whose `Host` is not loopback or that carries an `Origin`, and serves only
read-only tools, which is all `rhap mcp` has. Tool results pass through Cloudflare and reach
Claude. While the computer sleeps, the tools answer that it is offline.
`rhap auth logout --forget` removes the session and this bearer token.

There is no `self-update`, so a new binary comes from `make install`. Restart the server after
one: a running server keeps the old code, and its launchd agent restarts it only when it exits. Its
start line names the version.

```console
$ launchctl kickstart -k gui/$(id -u)/com.piekstra.rhap-mcp-http
```

`make install` does not reach a Claude Desktop extension, which runs its own copy. Install the new
release's `.mcpb` too.

## Exit codes

`0` ok · `2` usage · `3` auth · `4` not found · `5` upstream · `6` confirmation
required. Scripts can branch on `3` (log in) and `6` (a mutation needs
confirmation). Exit `5` also covers another `rhap` process holding the
token-refresh lock past a 75-second wait, in which case nothing was sent.

## Safety & privacy

- The OAuth token bundle is the only secret; it lives in the OS keychain
  (`piekstra.rhap`), never on argv, never in a log. `--verbose` prints methods
  and ids, never the token.
- The account is required per-operation and never ambient — no global flag, env
  var, or config default (a wrong-account guard under concurrent use). The
  account number is never hardcoded.
- Tests are fully offline; fixtures are synthetic. See [`SECURITY.md`](SECURITY.md).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
