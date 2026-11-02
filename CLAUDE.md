# CLAUDE.md

The canonical agent guide for this repo is **[AGENTS.md](AGENTS.md)** — read it
first. It covers build/test/lint, layout, conventions, and the safety rules.

Claude Code specifics:

- **Gate on `make verify`.** Don't report a change as done until it's green
  (fmt + clippy `-D warnings` + tests + smoke). Tests are fully offline.
- **This CLI is WRITE-capable — never call it read-only.** It can place and
  cancel real orders on a real account with no sandbox. **Never send a live
  order**, not even to test; build and exercise writes through `--dry-run` only.
  Live orders are the owner's, at the owner's hand.
- **Never complete the OAuth browser flow or write a real token yourself.**
  `rhap auth login` is the owner's to run. The flow is built and unit-tested in
  pieces; do not execute it.
- **Secrets:** the OAuth token bundle lives in the OS keychain (`piekstra.rhap`).
  Never print it, put it on argv, or write it to a file — including while
  debugging. The OAuth `client_id` is a public identifier and lives in config.
- **The tool schemas are VERIFIED (2026-08-27) — except one.** Names and
  top-level argument shapes were introspected from the live `tools/list`. The
  only still-unverified shape is `place_option_order`'s per-leg fields
  (docs/api.md §5). Keep that note honest; confirm a leg from a real order
  capture (or use `--legs <JSON>`) rather than presenting the single-leg guess
  as fact.
- **Written as if public.** No real account numbers, tokens, holdings, or
  balances in any diff — including test fixtures (dummies only).
