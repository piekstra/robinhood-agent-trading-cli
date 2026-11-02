//! Read commands: a deterministic 1:1 mapping onto the MCP read tools.
//!
//! Each command builds the tool's `arguments`, calls it, and emits the tool's
//! own result under a `<schema>/v1` DTO — `{ schema, tool, …, result }`. The CLI
//! does not reshape data it has never seen live: the `result` is whatever the
//! MCP returns, so a shape it doesn't recognize is surfaced, not dropped.
//!
//! Tool names and argument shapes are VERIFIED against the live `tools/list`
//! (2026-08-27) — see `docs/api.md §5`. They are built here in one tested place
//! so a future server change is a one-line edit.

use pk_cli_core::{output, CliError};
use serde_json::{json, Map, Value};

use super::Ctx;

/// Emit a read result under the family output contract. Shared with the option
/// read commands.
pub(crate) fn emit_read(
    json_mode: bool,
    schema: &str,
    tool: &str,
    extra: Vec<(&str, Value)>,
    result: Value,
) {
    let mut payload = Map::new();
    payload.insert("tool".into(), Value::String(tool.into()));
    for (k, v) in extra {
        payload.insert(k.into(), v);
    }
    payload.insert("result".into(), result);
    output::emit(json_mode, schema, Value::Object(payload), |v| {
        output::render(v.get("result").unwrap_or(v));
    });
}

/// `accounts list` → `get_accounts`. READ spans every linked account, so no
/// account filter is applied.
pub fn accounts_list(ctx: &Ctx) -> Result<(), CliError> {
    let result = ctx.client()?.read("get_accounts", json!({}))?;
    emit_read(ctx.common.json, "accounts", "get_accounts", vec![], result);
    Ok(())
}

/// `positions list --account <ID>` → `get_equity_positions`, scoped to the
/// required account. The account is passed explicitly per-invocation — never
/// defaulted from config or env (see `config.rs`).
pub fn positions_list(ctx: &Ctx, account: &str) -> Result<(), CliError> {
    let args = account_scoped_args(account);
    let result = ctx.client()?.read("get_equity_positions", args)?;
    emit_read(
        ctx.common.json,
        "positions",
        "get_equity_positions",
        vec![],
        result,
    );
    Ok(())
}

/// `orders list --account <ID> [--since]` → `get_equity_orders`, scoped to the
/// required account. `--since` maps to the server's `created_at_gte`.
///
/// The live schema (VERIFIED 2026-08-27) offers **only** `created_at_gte` — no
/// end-date and no server-side limit — so this command takes only `--since`
/// (not the shared range flags), rather than advertising a `--until`/`--limit`
/// that would silently do nothing.
pub fn orders_list(ctx: &Ctx, account: &str, since: Option<&str>) -> Result<(), CliError> {
    let args = orders_args(account, since);
    let result = ctx.client()?.read("get_equity_orders", args)?;
    emit_read(
        ctx.common.json,
        "orders",
        "get_equity_orders",
        vec![],
        result,
    );
    Ok(())
}

/// `quotes get <symbol>` → `get_equity_quotes` (schema VERIFIED 2026-08-27).
pub fn quotes_get(ctx: &Ctx, symbol: &str) -> Result<(), CliError> {
    let symbol = symbol.trim().to_uppercase();
    let result = ctx
        .client()?
        .read("get_equity_quotes", quote_args(&symbol))?;
    emit_read(
        ctx.common.json,
        "quote",
        "get_equity_quotes",
        vec![("symbol", Value::String(symbol))],
        result,
    );
    Ok(())
}

/// `{ "account_number": <ID> }` — the required account scope. Schema VERIFIED
/// 2026-08-27 (`get_equity_positions` also accepts an optional `cursor`).
pub fn account_scoped_args(account: &str) -> Value {
    json!({ "account_number": account })
}

/// Arguments for `get_equity_orders` (schema VERIFIED 2026-08-27): the required
/// `account_number`, plus `created_at_gte` when `--since` is given.
pub fn orders_args(account: &str, since: Option<&str>) -> Value {
    let mut m = Map::new();
    m.insert("account_number".into(), Value::String(account.into()));
    if let Some(s) = since {
        m.insert("created_at_gte".into(), Value::String(s.into()));
    }
    Value::Object(m)
}

/// Arguments for `get_equity_quotes`: the batch `symbols` array form (schema
/// VERIFIED 2026-08-27).
pub fn quote_args(symbol: &str) -> Value {
    json!({ "symbols": [symbol] })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_scoping_always_carries_the_required_account() {
        assert_eq!(
            account_scoped_args("SAMPLE01"),
            json!({"account_number": "SAMPLE01"})
        );
    }

    #[test]
    fn orders_args_map_since_to_created_at_gte_and_never_send_an_end_date() {
        let a = orders_args("SAMPLE01", Some("2026-01-01"));
        assert_eq!(a["account_number"], "SAMPLE01");
        // VERIFIED 2026-08-27: the filter key is `created_at_gte`.
        assert_eq!(a["created_at_gte"], "2026-01-01");
        // There is no end-date filter on this tool — never fabricate one.
        assert!(a.get("end_date").is_none());
        assert!(a.get("start_date").is_none());

        // With no --since, only the required account travels.
        let bare = orders_args("SAMPLE01", None);
        assert_eq!(bare, json!({"account_number": "SAMPLE01"}));
    }

    #[test]
    fn quote_args_use_the_batch_symbols_array() {
        assert_eq!(quote_args("AAPL"), json!({"symbols": ["AAPL"]}));
    }
}
