//! `tools list` / `tools show` — discoverability for the MCP tool surface.
//!
//! - `tools list [--schemas]` is a **live** `tools/list` passthrough: the
//!   server's current surface, names + one-line titles (or full schemas).
//! - `tools show <NAME>` prints a tool's full `inputSchema` from the embedded
//!   catalog (offline, no token needed) — the exact schema `rhap call` validates
//!   against. Unknown tool → exit 4.

use pk_cli_core::{output, CliError};
use serde_json::{json, Value};

use super::Ctx;
use crate::catalog;

/// `tools list [--schemas]` — live passthrough.
pub fn list(ctx: &Ctx, schemas: bool) -> Result<(), CliError> {
    let result = ctx.client()?.tools_list()?;
    output::emit(ctx.common.json, "tools", json!({ "result": result }), |v| {
        let tools = v
            .get("result")
            .and_then(|r| r.get("tools"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if tools.is_empty() {
            output::render(v.get("result").unwrap_or(v));
            return;
        }
        for t in tools {
            let name = t.get("name").and_then(Value::as_str).unwrap_or("?");
            match t.get("description").and_then(Value::as_str) {
                Some(d) => println!("{name}\t{d}"),
                None => println!("{name}"),
            }
            if schemas {
                if let Some(s) = t.get("inputSchema") {
                    output::json(s);
                }
            }
        }
    });
    Ok(())
}

/// `tools show <NAME>` — the full schema for one tool, from the embedded
/// catalog (offline). Emitted as `tool-schema/v1`.
pub fn show(common_json: bool, name: &str) -> Result<(), CliError> {
    let tool = catalog::tool(name).ok_or_else(|| {
        CliError::NotFound(format!(
            "unknown tool `{name}` — run `rhap tools list` to see the live surface"
        ))
    })?;
    let payload = json!({
        "name": name,
        "mutating": catalog::is_mutating(name),
        "account_scoped": catalog::is_account_scoped(name),
        "captured": catalog::captured(),
        "description": tool.get("description").cloned().unwrap_or(Value::Null),
        "input_schema": tool.get("inputSchema").cloned().unwrap_or(json!({})),
    });
    output::emit(common_json, "tool-schema", payload, output::render);
    Ok(())
}
