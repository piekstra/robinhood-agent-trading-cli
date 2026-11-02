# Fixtures

**These are NOT captures of account data.** Every value that could identify a
real account, person, holding, balance, or credential is a synthetic dummy per
the policy below, and no raw MCP response or token response has ever entered the
repo.

Two kinds of file live here, with different provenance:

1. **Public OAuth discovery documents** (`oauth_protected_resource.json`,
   `oauth_authorization_server.json`). These are unauthenticated,
   world-readable metadata served from Robinhood's own well-known endpoints
   (captured 2026-08-27). They contain **only public infrastructure URLs** —
   the authorization/token/registration endpoints — and no account data or
   secret. They are reproduced verbatim so the discovery parser is tested
   against the real shape.
2. **Wire-shape samples** (`token_response.json`, `client_registration.json`,
   `tools_list.json`, `mcp_tool_result.json`, `jsonrpc_error.json`,
   `sse_response.txt`). Structure is modelled on the OAuth 2.1, RFC 7591, and
   MCP (JSON-RPC 2.0 / Streamable HTTP) specifications and the venture's
   `MCP_TOOLS.md` tool inventory; every identity-bearing or credential value is
   a dummy. The per-tool `inputSchema` shapes were **verified against the live
   `tools/list` (2026-08-27)** for the top-level arguments; the one exception is
   `place_option_order`'s per-leg sub-schema, still a best guess (see
   `docs/api.md §5`).

`tests/fixture_shapes.rs` asserts the fields the code reads still exist, so a
shape drift fails the build instead of silently breaking a command; it also
enforces the synthetic-value policy below.

## Provenance

| File | Modelled on |
| --- | --- |
| `oauth_protected_resource.json` | Live public capture 2026-08-27 of `https://agent.robinhood.com/.well-known/oauth-protected-resource` (RFC 9728). Public infrastructure metadata; no account data. |
| `oauth_authorization_server.json` | Live public capture 2026-08-27 of the RFC 8414 authorization-server metadata referenced by the above. Public endpoints only. |
| `token_response.json` | OAuth 2.1 token-endpoint response shape (RFC 6749 §5.1). Tokens replaced with `SAMPLE-…` dummies. |
| `client_registration.json` | RFC 7591 dynamic-client-registration response. `client_id` is a public identifier of the app (dummy here). |
| `tools_list.json` | MCP `tools/list` result envelope. Tool names and top-level `inputSchema` arguments VERIFIED against the live server (2026-08-27); `place_option_order`'s per-leg fields remain a best guess. |
| `mcp_tool_result.json` | MCP `tools/call` result — a `content[].text` JSON string with `isError`. Account values are dummies. |
| `jsonrpc_error.json` | JSON-RPC 2.0 error object (`-32602` invalid params). |
| `sse_response.txt` | A `text/event-stream` framing of a JSON-RPC response, for the SSE parser. |

## Value policy

No fixture may carry anything that identifies a real account, person, holding,
balance, or credential. Types and formats are preserved exactly, since the
parsers depend on them; values are obvious dummies:

| Value | Dummy used |
| --- | --- |
| Account numbers | `SAMPLE01`, `SAMPLE02` |
| Ticker symbols | `SAMPL`, `TESTF` |
| Access / refresh tokens | `SAMPLE-…-NOT-A-REAL-CREDENTIAL` |
| OAuth client id | `sample-client-id-000000` |
| UUIDs (if any) | `00000000-0000-4000-8000-…` |

Public endpoint URLs (`agent.robinhood.com`, `api.robinhood.com`,
`robinhood.com/oauth`) are **not** personal data and are kept real — they are
what the discovery parser must handle.

The enforcement test is written **positively** ("identity-bearing fields must
hold a known dummy shape") rather than as a denylist of real values, because a
denylist would itself publish the values it exists to keep out of the repo.

## Refreshing

If a shape needs checking against reality, capture **outside the repo** (e.g.
`rhap tools list --json > /tmp/raw.json`) and hand-copy the *structure* back,
applying the table above. A real `tools/call` result carries live account data,
and a token response is a working credential — `raw-*.json`, `capture-*.json`,
and `*.raw.json` are gitignored as a backstop, but the rule is simply: **never
commit a capture.**
