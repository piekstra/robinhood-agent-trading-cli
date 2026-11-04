# Security Policy

## Reporting a vulnerability

Please report security issues privately via GitHub's
[security advisories](https://github.com/piekstra/robinhood-agent-trading-cli/security/advisories/new)
rather than a public issue.

## Threat model

`rhap` authenticates to Robinhood's **official** Agentic Trading MCP server via
OAuth 2.1 + PKCE and can **read** across the owner's linked accounts and
**place/cancel orders** on the funded Agentic account — with **real money and no
sandbox**. The things worth protecting:

- **The OAuth token bundle** — access token, refresh token, expiry — stored in
  the OS keychain under service `piekstra.rhap`, entry `tokens`. The access
  token is a bearer credential: anyone holding it can read the accounts and
  place orders on the Agentic account until it expires. The refresh token is
  worse, because it mints new access tokens without re-authenticating. Read at
  point of use, never logged, never placed on argv, never written to disk.
  `--verbose` prints JSON-RPC methods and ids, never the token.
- **The connector token**, only after `rhap mcp --new-token`: the bearer token of `mcp --http`,
  in the keychain as `mcp-http-token`. It reads through the tools and reaches nothing else.
- **The OAuth `client_id`** from dynamic client registration is a **public**
  identifier of the app, not a secret (the server is a public OAuth client —
  `token_endpoint_auth_methods_supported: ["none"]`). It lives in `config.json`
  by design.

`~/.config/rhap/token-refresh.lock` is an empty file that holds no secret. `rhap`
processes lock it while renewing the session, so two copies do not renew at the
same time. That holds once every copy sharing the keychain item is 0.3.2 or later,
including a process not yet restarted; docs/api.md §3 has what a 0.3.1 copy does.

The account is identifying and is **never hardcoded**. It is also never ambient:
any account-scoped command (`positions list`, `orders list`, `order place`,
`order cancel`) takes a **required `--account <ID>`** per invocation — there is
no global flag, no `RHAP_ACCOUNT` env var, and no config default. This is a
safety property, not just hygiene: an ambient/default account is a wrong-account
hazard under concurrent CLI use (a second command, a stale exported var, or a
forgotten default could act on the wrong account, including a live order there).

## What this tool does — and the guards on the write path

Unlike most of the CLI family, `rhap` is **not read-only**. It can place and
cancel orders. Three guards, all enforced in code, stand between the binary and
a live order:

1. **`--dry-run`** builds and prints the exact JSON-RPC request and sends
   nothing — no keychain read, no network. The only write path the test suite
   and `make smoke` exercise.
2. **Confirmation** — a live place/cancel requires a typed confirmation and
   exits `6` (confirmation required) when it cannot ask (`--json`, a pipe, or no
   TTY), unless `--force` is given.
3. **Review-then-place** — a live equity order is reviewed
   (`review_equity_order`) before it is placed.

It talks only to `agent.robinhood.com` (the MCP endpoint and OAuth discovery),
`robinhood.com/oauth` (authorization), and `api.robinhood.com/oauth2/token/`
(tokens), and a request carrying a credential follows a redirect only on the same host, over
https. No telemetry. The only other path is the one you set up: `rhap mcp` results reach Claude,
and `--http` results pass through your own Cloudflare account. It hardcodes no secrets;
`gitleaks` runs in CI over the full history.

## MCP tools

`rhap mcp` serves five read-only tools to Claude: `rhap_accounts`, `rhap_portfolio`,
`rhap_positions`, `rhap_orders` and `rhap_quotes`. **No tool can place, change or cancel an order,
exercise an option, move money or change a watchlist, and no argument can name a tool that does.**
A tool reaches Robinhood only through a closed list of eleven tools its catalog annotates
read-only. Tests hold every definition and every request the real client sends to that list. The
three guards above are not what stops an order here: there is no path to one.

The session itself can place orders. It is read from the keychain once per tool call and held
only for that call, and renewed the same way the command line renews it. Listing the tools touches
no credential. Robinhood's `guide` beside a result, advice for a model that can trade, is left out.

**What reaches Claude.** Tool results go to Claude as part of your conversation: account numbers,
balances, holdings and orders across every linked account you ask about. A cursor holds a position
in Robinhood's list: its own page cursor and a fingerprint of the row, not the row.

`rhap mcp --http --port <PORT>` serves the same tools for your own Cloudflare Worker, which reaches
it through your own Cloudflare Tunnel (cli-common's `connector/`). It binds 127.0.0.1 only, with no
flag for another address, and refuses to start without a bearer token of 32 or more characters,
from `RHAP_MCP_TOKEN` or the keychain item `mcp-http-token`, which `rhap mcp --new-token` makes
from 256 random bits and replaces only with `--overwrite`. Every request must carry the token
(compared in constant time) and a `Host` of `127.0.0.1` or `localhost`, and must not carry an
`Origin`; there are no CORS headers. A copy of that token lives in your Worker as its
`HOME_RHAP_TOKEN` secret. A leaked copy can read your accounts through these tools, and cannot
place an order or obtain the Robinhood session.

**Tool results also cross Cloudflare on this path**, through your tunnel and your Worker, on their
way to Claude. Who holds what, and what a leaked token can do, is in the connector's
[threat model](https://github.com/piekstra/cli-common/blob/main/connector/README.md#threat-model).

To revoke the connector's access, run `rhap mcp --new-token --overwrite` and restart the server
(`launchctl kickstart -k gui/$(id -u)/com.piekstra.rhap-mcp-http`); the old token works until the
restart, and a token in `$RHAP_MCP_TOKEN` wins until you change it. To stop serving, run `launchctl
bootout gui/$(id -u)/com.piekstra.rhap-mcp-http` and stop the tunnel.

## Real-money risk

There is **no paper/sandbox endpoint** for the Agentic MCP. A non-dry-run
`order place`/`cancel` is a live trade. Two rules follow and are enforced:

- **Never send a live order to test or to verify a guessed schema.** Use
  `--dry-run`, and reconcile argument shapes with `rhap tools list`.
- **The tool argument schemas are UNVERIFIED best guesses** (docs/api.md §5):
  tool names are ground truth, some argument field names were inferred without a
  live introspection. Confirm them before relying on a live order.

## Handling the credential safely

`rhap auth login` performs the browser OAuth flow and stores the token bundle.
For rotation or headless setup, pipe a token bundle from a password manager
rather than typing it:

```console
$ op read 'op://Private/rhap/token-bundle' | rhap auth set-credential --stdin --overwrite
```

`rhap auth logout` clears the session; `--forget` also drops the stored config
(the OAuth client id) and the connector token. A running `rhap mcp --http` keeps its connector
token until it stops, and it prints what it could not reach.
