//! `rhap call <TOOL> [--args '<json>'] [--dry-run] [--force]` — the generic
//! passthrough to any MCP tool, and the coverage + future-proofing layer.
//!
//! Safety invariants preserved even on this generic path (validated against the
//! embedded catalog, so `--dry-run` is fully offline):
//!
//! - **Unknown tool → exit 4.** The name must be in the 2026-08-27 catalog.
//! - **A mutating tool needs `--dry-run` or `--force`.** A live mutation without
//!   `--force` is refused (exit 6). Mutating = `catalog::is_mutating`: every
//!   tool not annotated `readOnlyHint: true`.
//! - **A mutating account-scoped tool must carry `account_number` in `--args`**
//!   (exit 2). No ambient/default account, ever — the same rule the typed
//!   commands enforce with a required `--account`.
//! - **Reads pass through freely** (no account requirement; the server enforces
//!   its own required fields).

use pk_cli_core::{output, CliError};
use serde_json::{json, Value};

use super::Ctx;
use crate::{catalog, mcp};

pub fn call(
    ctx: &Ctx,
    tool: &str,
    args_json: Option<&str>,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    // 1. The tool must be one the catalog knows.
    if !catalog::is_known(tool) {
        return Err(CliError::NotFound(format!(
            "unknown tool `{tool}` — not in the {} catalog. Run `rhap tools list` to see the live \
             surface, or `rhap tools show <NAME>` for a schema.",
            catalog::captured()
        )));
    }

    // 2. Parse --args (a JSON object; defaults to {}).
    let args = parse_args(args_json)?;

    // 3. Mutation guards.
    let mutating = catalog::is_mutating(tool);
    if mutating {
        // 3a. Account-scoped mutation requires an explicit account_number.
        if catalog::is_account_scoped(tool) && !has_account(&args) {
            return Err(CliError::Usage(format!(
                "`{tool}` is an account-scoped mutation — its `--args` must include a non-empty \
                 \"account_number\" (never defaulted or inferred)"
            )));
        }
        // 3b. A live mutation needs --dry-run or --force.
        if !dry_run && !force {
            return Err(CliError::ConfirmationRequired(format!(
                "`{tool}` is a mutating tool — re-run with `--dry-run` to preview the request, or \
                 `--force` to send it"
            )));
        }
    }

    // 4. Dry-run: print the exact JSON-RPC request, send nothing.
    if dry_run {
        let req = mcp::request(1, "tools/call", mcp::tools_call_params(tool, args));
        output::emit(
            ctx.common.json,
            "call-dry-run",
            json!({ "tool": tool, "mutating": mutating, "would_send": false, "request": req }),
            |v| {
                eprintln!(
                    "dry run — the following JSON-RPC request would be sent to the MCP endpoint:"
                );
                output::json(v.get("request").unwrap_or(v));
            },
        );
        return Ok(());
    }

    // 5. Live invocation.
    // A mutating tool reaches here only past the --force guard above.
    let mut client = ctx.client()?;
    let result = if mutating {
        client.write(tool, args)?
    } else {
        client.read(tool, args)?
    };
    output::emit(
        ctx.common.json,
        "call",
        json!({ "tool": tool, "result": result }),
        |v| output::render(v.get("result").unwrap_or(v)),
    );
    Ok(())
}

/// Parse the `--args` JSON object, defaulting to `{}`.
fn parse_args(args_json: Option<&str>) -> Result<Value, CliError> {
    match args_json {
        None => Ok(json!({})),
        Some(raw) => {
            let v: Value = serde_json::from_str(raw)
                .map_err(|e| CliError::Usage(format!("--args is not valid JSON: {e}")))?;
            if !v.is_object() {
                return Err(CliError::Usage("--args must be a JSON object".into()));
            }
            Ok(v)
        }
    }
}

/// Whether `args` carries a non-empty `account_number`.
fn has_account(args: &Value) -> bool {
    args.get("account_number")
        .and_then(Value::as_str)
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_parsing_requires_an_object() {
        assert_eq!(parse_args(None).unwrap(), json!({}));
        assert_eq!(parse_args(Some(r#"{"a":1}"#)).unwrap(), json!({"a": 1}));
        assert!(parse_args(Some("[1,2]")).is_err());
        assert!(parse_args(Some("nope")).is_err());
    }

    #[test]
    fn account_presence_is_detected() {
        assert!(has_account(&json!({"account_number": "SAMPLE01"})));
        assert!(!has_account(&json!({"account_number": ""})));
        assert!(!has_account(&json!({"account_number": "  "})));
        assert!(!has_account(&json!({})));
    }
}
