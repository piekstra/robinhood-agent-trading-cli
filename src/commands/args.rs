//! Small helpers for building a tool's `arguments` object from CLI flags, and
//! for running a read tool through the shared emit path.
//!
//! Array-valued fields are accepted as comma-separated lists on the CLI
//! (`--symbols AAPL,MSFT`) and split here; genuinely structured array fields
//! (scan filters, tax lots) are accepted as raw JSON via a `--…-json` flag.

use pk_cli_core::CliError;
use serde_json::{json, Map, Value};

use super::reads::emit_read;
use super::Ctx;

/// A fluent builder for a tool's `arguments` object. Optional flags are omitted
/// when absent (never emitted as `null`), matching the omit-don't-null rule.
#[derive(Default)]
pub struct Args(Map<String, Value>);

impl Args {
    pub fn new() -> Self {
        Args(Map::new())
    }

    /// A required string field.
    pub fn str(mut self, key: &str, value: impl Into<String>) -> Self {
        self.0.insert(key.into(), Value::String(value.into()));
        self
    }

    /// An optional string field (omitted when `None`).
    pub fn opt(mut self, key: &str, value: Option<&str>) -> Self {
        if let Some(v) = value {
            self.0.insert(key.into(), Value::String(v.into()));
        }
        self
    }

    /// A required array field from a comma-separated list.
    pub fn csv(mut self, key: &str, value: &str) -> Self {
        self.0.insert(key.into(), json!(csv(value)));
        self
    }

    /// An optional array field from a comma-separated list.
    pub fn opt_csv(mut self, key: &str, value: Option<&str>) -> Self {
        if let Some(v) = value {
            self.0.insert(key.into(), json!(csv(v)));
        }
        self
    }

    /// An optional integer field.
    pub fn opt_int(mut self, key: &str, value: Option<i64>) -> Self {
        if let Some(v) = value {
            self.0.insert(key.into(), json!(v));
        }
        self
    }

    /// An optional boolean field.
    pub fn opt_bool(mut self, key: &str, value: Option<bool>) -> Self {
        if let Some(v) = value {
            self.0.insert(key.into(), json!(v));
        }
        self
    }

    /// A pre-built JSON value, when present.
    pub fn opt_value(mut self, key: &str, value: Option<Value>) -> Self {
        if let Some(v) = value {
            self.0.insert(key.into(), v);
        }
        self
    }

    pub fn build(self) -> Value {
        Value::Object(self.0)
    }
}

/// Split a comma-separated CLI value into a trimmed, non-empty vector.
pub fn csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Parse a raw JSON array from a `--…-json` flag (for structured array fields).
pub fn json_array(field: &str, raw: &str) -> Result<Value, CliError> {
    let v: Value = serde_json::from_str(raw)
        .map_err(|e| CliError::Usage(format!("{field} is not valid JSON: {e}")))?;
    if !v.is_array() {
        return Err(CliError::Usage(format!("{field} must be a JSON array")));
    }
    Ok(v)
}

/// Call a read tool and emit its result under `<schema>/v1`. The read gate
/// ([`crate::mcp::readable`]) is checked before the keychain is touched, and
/// again by [`crate::mcp::McpClient::read`].
pub fn run_read(ctx: &Ctx, schema: &str, tool: &str, args: Value) -> Result<(), CliError> {
    crate::mcp::readable(tool)?;
    let result = ctx.client()?.read(tool, args)?;
    emit_read(ctx.common.json, schema, tool, vec![], result);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_splits_and_trims() {
        assert_eq!(csv("AAPL, MSFT ,GOOG"), vec!["AAPL", "MSFT", "GOOG"]);
        assert!(csv("  , ").is_empty());
    }

    #[test]
    fn the_builder_omits_absent_optionals() {
        let a = Args::new()
            .str("account_number", "SAMPLE01")
            .opt("state", Some("filled"))
            .opt("symbol", None)
            .opt_int("limit", Some(5))
            .opt_int("days", None)
            .csv("symbols", "AAPL,MSFT")
            .build();
        assert_eq!(a["account_number"], "SAMPLE01");
        assert_eq!(a["state"], "filled");
        assert!(a.get("symbol").is_none());
        assert_eq!(a["limit"], 5);
        assert!(a.get("days").is_none());
        assert_eq!(a["symbols"], json!(["AAPL", "MSFT"]));
    }

    #[test]
    fn json_array_validates() {
        assert_eq!(json_array("--filters", "[1,2]").unwrap(), json!([1, 2]));
        assert!(json_array("--filters", "{}").is_err());
        assert!(json_array("--filters", "nope").is_err());
    }
}
