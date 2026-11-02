# Contributing

Thanks for helping improve `rhap`. Read **[AGENTS.md](AGENTS.md)** first — it is
the canonical guide to layout, conventions, and the safety rules. The essentials:

## Workflow

1. Branch off `main`.
2. Make the change. Reuse the shared `pk-cli-*` crates (pinned to a
   `cli-common` tag) before writing anything new.
3. `make verify` — the CI gate: `fmt-check` + `clippy -D warnings` + `test` +
   `smoke`. Keep it green.
4. Open a PR with a clear description of what changed and why.

## The bug-fix contract

- A fix comes with a test that fails before it and passes after. The pure logic
  (PKCE, discovery parsing, JSON-RPC/SSE handling, order-argument building,
  token parsing) is unit-tested; surface behavior is black-box tested through
  the built binary. Tests are **fully offline** — no network, no keychain, no
  live MCP.

## Hard rules

- **Never send a live order.** This tool can place and cancel real orders on a
  real account with no sandbox. Build and exercise the write path through
  `--dry-run` only. A live order is the owner's, at the owner's hand.
- **Don't call this tool read-only.** It is write-capable; if you change what it
  can do, the docs change in the same commit.
- **No secrets or personal data in the repo** — source, docs, fixtures, or
  commit messages. The token lives only in the keychain; fixtures are synthetic;
  the account number is never hardcoded. A test enforces this over every tracked
  file.
- **Keep the schema honesty.** Tool names are ground truth; some argument shapes
  are UNVERIFIED guesses (docs/api.md §5). Don't present a guess as confirmed;
  reconcile with `rhap tools list`.

## Commits

Small, focused commits with imperative subject lines. Don't accumulate unrelated
changes in one commit.
