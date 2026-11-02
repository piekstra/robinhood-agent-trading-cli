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
- **The OAuth `client_id`** from dynamic client registration is a **public**
  identifier of the app, not a secret (the server is a public OAuth client —
  `token_endpoint_auth_methods_supported: ["none"]`). It lives in `config.json`
  by design.

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
(tokens). No telemetry, no third-party services. It hardcodes no secrets;
`gitleaks` runs in CI over the full history.

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
(the OAuth client id).
