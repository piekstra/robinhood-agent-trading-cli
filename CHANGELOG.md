# Changelog

All notable changes to `rhap` are documented here. Newest first. The project follows
[SemVer](https://semver.org/); until 1.0, minors carry the breaking-change signal.

## 0.4.0 — 2026-11-03

- `rhap mcp`: five read-only tools for Claude: `rhap_accounts`, `rhap_portfolio`,
  `rhap_positions`, `rhap_orders` and `rhap_quotes`. **No tool can place, change or cancel an
  order, exercise an option, move money or change a watchlist**, and tests hold every definition
  and every request to that. They run over stdio for Claude Code and Claude Desktop, and with
  `--http` on 127.0.0.1 behind a bearer token for Claude on the web and mobile through your own
  Cloudflare Worker and tunnel.
- `auth logout --forget` also removes the connector token of `mcp --http`.
- A request carrying a credential follows a redirect only on the same host, over https. A refresh
  token used to follow a 307 to any host.
- Releases attach `rhap-<version>.mcpb`, a Claude Desktop extension, and a Windows x86_64 tarball,
  and publish only once every asset is there.
- cli-common v0.28.0, up from v0.20.0.
