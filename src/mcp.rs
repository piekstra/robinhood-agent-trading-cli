//! A minimal, deterministic JSON-RPC 2.0 client for the MCP "Streamable HTTP"
//! transport.
//!
//! Each `rhap` invocation is a fresh process, so every command runs the full
//! MCP handshake before its real call: `initialize` (capturing any
//! `Mcp-Session-Id`), the `notifications/initialized` notification, then the
//! `tools/list` or `tools/call`. A Streamable-HTTP endpoint may answer a POST
//! with either a single JSON object or a `text/event-stream`; both are handled.
//!
//! The wire-shaping is pure and unit-tested (request envelopes, SSE parsing,
//! JSON-RPC error mapping, tool-result extraction, write-tool classification);
//! only [`McpClient`]'s POSTs touch the network, and the test suite never sends
//! one.

use pk_cli_core::CliError;
use serde_json::{json, Value};

/// The MCP protocol version this client speaks. The server may negotiate a
/// different one in its `initialize` result; this is what we offer.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

// The read-vs-mutating classification lives in `crate::catalog::is_mutating`
// (from the catalog's `readOnlyHint`; the single source of truth shared by the
// typed write guard and the generic `call` command).

/// A JSON-RPC 2.0 request object with an id.
pub fn request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// A JSON-RPC 2.0 notification (no id, no response expected).
pub fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// The `initialize` params this client sends.
pub fn initialize_params() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": { "name": "rhap", "version": env!("CARGO_PKG_VERSION") },
    })
}

/// Whether [`McpClient::read`] may run `tool`.
pub fn readable(tool: &str) -> Result<(), CliError> {
    match crate::catalog::kind(tool) {
        crate::catalog::ToolKind::Read | crate::catalog::ToolKind::Review => Ok(()),
        crate::catalog::ToolKind::Write => Err(CliError::Other(format!(
            "internal: `{tool}` is not read-only and cannot run on the read path"
        ))),
    }
}

/// The `tools/call` params: the deterministic 1:1 mapping from a CLI command to
/// an MCP tool invocation.
pub fn tools_call_params(name: &str, arguments: Value) -> Value {
    json!({ "name": name, "arguments": arguments })
}

/// Parse an SSE body into the JSON payloads of its `data:` fields.
///
/// Handles multi-line `data:` (joined with newlines per the SSE spec) and
/// ignores comments/other fields. Non-JSON data lines are skipped.
pub fn parse_sse(body: &str) -> Vec<Value> {
    let mut events = Vec::new();
    let mut data = String::new();
    let flush = |data: &mut String, events: &mut Vec<Value>| {
        if !data.is_empty() {
            if let Ok(v) = serde_json::from_str::<Value>(data.trim()) {
                events.push(v);
            }
            data.clear();
        }
    };
    for line in body.lines() {
        if line.is_empty() {
            flush(&mut data, &mut events);
        } else if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
        }
        // `event:`, `id:`, `:comment`, etc. are ignored — we only need the data.
    }
    flush(&mut data, &mut events);
    events
}

/// Pick the JSON-RPC response for `id` out of a body that is either a single
/// object or a batch/stream of them.
pub fn response_for(id: u64, values: &[Value]) -> Option<Value> {
    values
        .iter()
        .find(|v| v.get("id").and_then(Value::as_u64) == Some(id))
        .or_else(|| {
            values
                .iter()
                .find(|v| v.get("result").is_some() || v.get("error").is_some())
        })
        .cloned()
}

/// Extract a JSON-RPC `result`, mapping an `error` object onto the exit-code
/// contract.
pub fn extract_result(response: &Value) -> Result<Value, CliError> {
    if let Some(err) = response.get("error") {
        let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
        let message = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown JSON-RPC error");
        // -32601 method-not-found / -32602 invalid-params are usage-shaped; the
        // rest are upstream conditions.
        return Err(match code {
            -32601 | -32602 => CliError::Usage(format!("MCP error {code}: {message}")),
            _ => CliError::Upstream(format!("MCP error {code}: {message}")),
        });
    }
    response.get("result").cloned().ok_or_else(|| {
        CliError::Upstream("JSON-RPC response carried neither result nor error".into())
    })
}

/// Normalize a `tools/call` result into the payload a command emits.
///
/// An MCP tool result is `{ "content": [ { "type": "text", "text": "…" } ],
/// "isError": bool, "structuredContent"?: … }`. A tool-level error surfaces as
/// [`CliError::Upstream`]. When the single text block is itself JSON (the usual
/// case for these tools), it is parsed so the CLI emits structured data rather
/// than a JSON-string-in-a-string; `structuredContent`, when present, is
/// preferred.
pub fn tool_result_payload(result: &Value) -> Result<Value, CliError> {
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(CliError::Upstream(format!(
            "the MCP tool reported an error: {}",
            content_text(result).unwrap_or_else(|| result.to_string())
        )));
    }
    if let Some(structured) = result.get("structuredContent") {
        return Ok(structured.clone());
    }
    match content_text(result) {
        Some(text) => Ok(serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text))),
        // No text content: hand back the raw result so nothing is silently lost.
        None => Ok(result.clone()),
    }
}

/// Concatenate the `text` of every text content block.
fn content_text(result: &Value) -> Option<String> {
    let arr = result.get("content").and_then(Value::as_array)?;
    let joined: String = arr
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");
    (!joined.is_empty()).then_some(joined)
}

/// A live client over the MCP Streamable HTTP endpoint. Constructed only for
/// commands that actually call the server; the test suite never builds one that
/// sends.
pub struct McpClient {
    http: reqwest::blocking::Client,
    endpoint: String,
    token: String,
    session_id: Option<String>,
    next_id: u64,
    verbose: bool,
}

impl McpClient {
    pub fn new(endpoint: &str, access_token: &str, verbose: bool) -> Result<Self, CliError> {
        Ok(McpClient {
            http: pk_cli_http::client("rhap", env!("CARGO_PKG_VERSION"))?,
            endpoint: endpoint.to_string(),
            token: access_token.to_string(),
            session_id: None,
            next_id: 0,
            verbose,
        })
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Run the MCP handshake: `initialize` then `notifications/initialized`.
    pub fn initialize(&mut self) -> Result<Value, CliError> {
        let id = self.id();
        let (result, session) = self.post(&request(id, "initialize", initialize_params()), id)?;
        self.session_id = session;
        // Best-effort: a server that ignores the notification is fine.
        let _ = self.post_notification(&notification("notifications/initialized", json!({})));
        result.ok_or_else(|| CliError::Upstream("initialize returned no result".into()))
    }

    /// `tools/list` passthrough.
    pub fn tools_list(&mut self) -> Result<Value, CliError> {
        self.initialize()?;
        let id = self.id();
        let (result, _) = self.post(&request(id, "tools/list", json!({})), id)?;
        result.ok_or_else(|| CliError::Upstream("tools/list returned no result".into()))
    }

    /// `tools/call` for a tool the unguarded read path may run: a read, or an
    /// order review (see [`crate::catalog::kind`]). A write refuses here, so
    /// a typed read command cannot reach a write tool whichever helper it
    /// uses.
    pub fn read(&mut self, name: &str, arguments: Value) -> Result<Value, CliError> {
        readable(name)?;
        self.tools_call(name, arguments)
    }

    /// `tools/call` for any tool. Only callers that have already passed the
    /// write guard use this: `run_write` and the generic `call`.
    pub fn write(&mut self, name: &str, arguments: Value) -> Result<Value, CliError> {
        self.tools_call(name, arguments)
    }

    /// `tools/call` for `name` with `arguments`, returning the normalized tool
    /// payload.
    fn tools_call(&mut self, name: &str, arguments: Value) -> Result<Value, CliError> {
        self.initialize()?;
        let id = self.id();
        let (result, _) = self.post(
            &request(id, "tools/call", tools_call_params(name, arguments)),
            id,
        )?;
        let result =
            result.ok_or_else(|| CliError::Upstream("tools/call returned no result".into()))?;
        tool_result_payload(&result)
    }

    /// POST a JSON-RPC request, returning `(result, session_id)`.
    fn post(&self, body: &Value, id: u64) -> Result<(Option<Value>, Option<String>), CliError> {
        if self.verbose {
            // The method and id, never the token.
            eprintln!(
                "POST {} {}",
                self.endpoint,
                body.get("method").and_then(Value::as_str).unwrap_or("?")
            );
        }
        let mut req = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.token)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL_VERSION);
        if let Some(sid) = &self.session_id {
            req = req.header("Mcp-Session-Id", sid);
        }
        let resp = req
            .json(body)
            .send()
            .map_err(|e| CliError::Upstream(format!("MCP request failed: {e}")))?;

        let status = resp.status().as_u16();
        let session = resp
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .or_else(|| self.session_id.clone());
        let ctype = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let text = resp
            .text()
            .map_err(|e| CliError::Upstream(format!("reading MCP response: {e}")))?;

        if status == 401 || status == 403 {
            return Err(CliError::Auth(
                "the MCP server rejected the access token (HTTP 401/403) — run `rhap auth login`"
                    .into(),
            ));
        }
        if !(200..300).contains(&status) {
            return Err(CliError::Upstream(format!(
                "MCP server returned HTTP {status}: {}",
                text.chars().take(200).collect::<String>()
            )));
        }

        let values = if ctype.contains("text/event-stream") {
            parse_sse(&text)
        } else if text.trim().is_empty() {
            Vec::new()
        } else {
            match serde_json::from_str::<Value>(&text) {
                Ok(Value::Array(a)) => a,
                Ok(v) => vec![v],
                Err(e) => return Err(CliError::Upstream(format!("MCP response is not JSON: {e}"))),
            }
        };

        // A notification's ack (or an empty 202) has no response body.
        if values.is_empty() {
            return Ok((None, session));
        }
        let response = response_for(id, &values).ok_or_else(|| {
            CliError::Upstream("no JSON-RPC response matched the request id".into())
        })?;
        Ok((Some(extract_result(&response)?), session))
    }

    /// POST a notification (no response expected). Errors are swallowed by the
    /// caller where appropriate.
    fn post_notification(&self, body: &Value) -> Result<(), CliError> {
        let mut req = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.token)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL_VERSION);
        if let Some(sid) = &self.session_id {
            req = req.header("Mcp-Session-Id", sid);
        }
        req.json(body)
            .send()
            .map_err(|e| CliError::Upstream(format!("MCP notification failed: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_and_notification_envelopes() {
        let r = request(7, "tools/list", json!({}));
        assert_eq!(r["jsonrpc"], "2.0");
        assert_eq!(r["id"], 7);
        assert_eq!(r["method"], "tools/list");
        let n = notification("notifications/initialized", json!({}));
        assert_eq!(n["jsonrpc"], "2.0");
        assert!(n.get("id").is_none(), "a notification has no id");
    }

    #[test]
    fn the_read_path_refuses_writes_but_allows_reviews() {
        for ok in [
            "get_accounts",
            "run_scan",
            "review_advanced_order",
            "preview_crypto_order",
        ] {
            assert!(readable(ok).is_ok(), "{ok}");
        }
        for w in [
            "delete_alert",
            "mark_alerts_read",
            "cancel_equity_order",
            "nope",
        ] {
            assert!(readable(w).is_err(), "{w} must not run unguarded");
        }
    }

    #[test]
    fn tools_call_params_map_one_to_one() {
        let p = tools_call_params("get_accounts", json!({"account_number": "SAMPLE01"}));
        assert_eq!(p["name"], "get_accounts");
        assert_eq!(p["arguments"]["account_number"], "SAMPLE01");
    }

    #[test]
    fn sse_body_yields_its_data_payloads() {
        let body =
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n\n";
        let events = parse_sse(body);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["result"]["ok"], true);
    }

    #[test]
    fn sse_joins_multiline_data() {
        let body = "data: {\"jsonrpc\":\"2.0\",\ndata: \"id\":2,\"result\":1}\n\n";
        let events = parse_sse(body);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["id"], 2);
    }

    #[test]
    fn response_for_matches_the_request_id() {
        let vals = vec![
            json!({"jsonrpc":"2.0","id":1,"result":{"a":1}}),
            json!({"jsonrpc":"2.0","id":2,"result":{"b":2}}),
        ];
        assert_eq!(response_for(2, &vals).unwrap()["result"]["b"], 2);
    }

    #[test]
    fn a_jsonrpc_error_maps_to_the_exit_code_contract() {
        let usage = extract_result(&json!({"error":{"code":-32602,"message":"bad params"}}));
        assert_eq!(usage.unwrap_err().exit_code(), 2);
        let upstream = extract_result(&json!({"error":{"code":-32000,"message":"boom"}}));
        assert_eq!(upstream.unwrap_err().exit_code(), 5);
        let ok = extract_result(&json!({"result":{"x":1}})).unwrap();
        assert_eq!(ok["x"], 1);
    }

    #[test]
    fn a_tool_result_text_block_is_parsed_as_json() {
        let result = json!({
            "content": [{"type": "text", "text": "{\"accounts\":[\"SAMPLE01\"]}"}],
            "isError": false
        });
        let payload = tool_result_payload(&result).unwrap();
        assert_eq!(payload["accounts"][0], "SAMPLE01");
    }

    #[test]
    fn structured_content_wins_when_present() {
        let result = json!({
            "content": [{"type": "text", "text": "ignored"}],
            "structuredContent": {"total": 2}
        });
        assert_eq!(tool_result_payload(&result).unwrap()["total"], 2);
    }

    #[test]
    fn a_tool_error_result_is_an_upstream_error() {
        let result = json!({
            "content": [{"type": "text", "text": "insufficient buying power"}],
            "isError": true
        });
        let err = tool_result_payload(&result).unwrap_err();
        assert_eq!(err.exit_code(), 5);
        assert!(err.to_string().contains("insufficient buying power"));
    }

    #[test]
    fn plain_text_content_falls_back_to_a_string() {
        let result = json!({"content": [{"type": "text", "text": "hello"}]});
        assert_eq!(tool_result_payload(&result).unwrap(), json!("hello"));
    }
}
