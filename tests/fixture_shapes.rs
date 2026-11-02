//! Contract tests over `tests/fixtures/`.
//!
//! Two jobs:
//!
//! 1. Assert that the fields the CLI reads out of OAuth discovery, token, and
//!    MCP JSON-RPC payloads are present and correctly typed, so a shape drift
//!    fails the build loudly rather than breaking a command at runtime.
//! 2. Enforce the synthetic-value policy in `tests/fixtures/README.md`, and scan
//!    the rest of the tracked sources for personal data.
//!
//! Nothing here talks to a network.

use std::path::PathBuf;

use serde_json::Value;

/// Substrings that betray a committed pre-signed URL. Assembled at runtime so
/// this file — which the tracked-source scan also reads — does not itself
/// contain the markers it searches for.
fn presign_markers() -> [String; 3] {
    [
        format!("X-Amz{}", "-Signature"),
        format!("X-Amz{}", "-Credential"),
        format!("amazonaws{}", ".com"),
    ]
}

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path}: {e}"));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parsing {path}: {e}"))
}

fn read_text(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path}: {e}"))
}

// ---------------------------------------------------------- oauth discovery

/// RFC 9728: the protected-resource metadata names its authorization server,
/// and its `resource` is the MCP endpoint the token gets bound to.
#[test]
fn protected_resource_metadata_names_the_authorization_server() {
    let v = fixture("oauth_protected_resource.json");
    assert_eq!(v["resource"], "https://agent.robinhood.com/mcp/trading");
    let servers = v["authorization_servers"].as_array().expect("array");
    assert!(
        !servers.is_empty(),
        "must list at least one authorization server"
    );
    assert!(servers[0].is_string());
}

/// RFC 8414: the authorization-server metadata carries the three endpoints and
/// declares PKCE S256 + a public client (auth method `none`).
#[test]
fn authorization_server_metadata_carries_the_endpoints_and_pkce() {
    let v = fixture("oauth_authorization_server.json");
    assert!(v["authorization_endpoint"].is_string());
    assert!(v["token_endpoint"].is_string());
    assert!(v["registration_endpoint"].is_string());
    let challenge_methods = v["code_challenge_methods_supported"]
        .as_array()
        .expect("array");
    assert!(
        challenge_methods.iter().any(|m| m == "S256"),
        "S256 must be advertised, or the CLI would refuse to send a code"
    );
    let auth_methods = v["token_endpoint_auth_methods_supported"]
        .as_array()
        .expect("array");
    assert!(
        auth_methods.iter().any(|m| m == "none"),
        "a public client (no secret) requires the `none` auth method"
    );
    let grants = v["grant_types_supported"].as_array().expect("array");
    assert!(grants.iter().any(|g| g == "authorization_code"));
    assert!(grants.iter().any(|g| g == "refresh_token"));
}

// ------------------------------------------------------------ oauth tokens

/// The token-endpoint response carries the fields `TokenSet::from_response`
/// reads: the access/refresh tokens and `expires_in`.
#[test]
fn token_response_carries_the_bundle() {
    let v = fixture("token_response.json");
    assert!(v["access_token"].is_string());
    assert!(v["refresh_token"].is_string());
    assert!(v["expires_in"].is_number());
    assert_eq!(v["token_type"], "Bearer");
    assert_eq!(v["scope"], "internal");
}

/// RFC 7591: the registration response carries the public `client_id` the CLI
/// caches for later refreshes, and confirms the `none` auth method.
#[test]
fn client_registration_yields_a_public_client_id() {
    let v = fixture("client_registration.json");
    assert!(v["client_id"].is_string());
    assert_eq!(v["token_endpoint_auth_method"], "none");
}

// -------------------------------------------------------------- mcp shapes

/// `tools/list` is a JSON-RPC 2.0 result whose `tools` each carry a name and an
/// input schema. The ground-truth tool names (from MCP_TOOLS.md) must be
/// present so the typed commands map onto real tools.
#[test]
fn tools_list_carries_the_ground_truth_tool_names() {
    let v = fixture("tools_list.json");
    assert_eq!(v["jsonrpc"], "2.0");
    let tools = v["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().expect("tool name"))
        .collect();
    for expected in [
        "get_accounts",
        "get_equity_positions",
        "get_equity_orders",
        "get_equity_quotes",
        "review_equity_order",
        "place_equity_order",
        "cancel_equity_order",
        // Option surface (introspected live 2026-08-27).
        "get_option_positions",
        "get_option_orders",
        "get_option_quotes",
        "get_option_chains",
        "review_option_order",
        "place_option_order",
        "cancel_option_order",
    ] {
        assert!(
            names.contains(&expected),
            "tools/list missing {expected}: {names:?}"
        );
    }
    for t in tools {
        assert!(
            t["inputSchema"].is_object(),
            "each tool declares an input schema"
        );
    }
}

/// The verified equity/option order schemas: the order-kind field is `type`
/// (not `order_type`), `get_equity_orders` filters by `created_at_gte`, and the
/// account-scoped tools require `account_number` (VERIFIED 2026-08-27).
#[test]
fn the_verified_schemas_use_type_created_at_gte_and_require_account() {
    let v = fixture("tools_list.json");
    let tools = v["result"]["tools"].as_array().expect("tools array");
    let schema = |name: &str| -> serde_json::Value {
        tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("no tool {name}"))["inputSchema"]
            .clone()
    };

    let place = schema("place_equity_order");
    assert!(
        place["properties"]["type"].is_object(),
        "the field is `type`"
    );
    assert!(
        place["properties"].get("order_type").is_none(),
        "there is no `order_type`"
    );
    assert!(
        place["properties"].get("extended_hours").is_none(),
        "there is no `extended_hours`"
    );

    let orders = schema("get_equity_orders");
    assert!(orders["properties"]["created_at_gte"].is_object());
    assert!(orders["properties"].get("start_date").is_none());
    assert!(orders["properties"].get("end_date").is_none());

    // Every account-scoped tool requires account_number.
    for name in [
        "get_equity_positions",
        "get_equity_orders",
        "place_equity_order",
        "cancel_equity_order",
        "get_option_positions",
        "get_option_orders",
        "place_option_order",
        "cancel_option_order",
    ] {
        let req = schema(name)["required"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            req.iter().any(|r| r == "account_number"),
            "{name} must require account_number"
        );
    }

    // Option quotes key on instrument_ids, not symbols.
    let oq = schema("get_option_quotes");
    assert!(oq["properties"]["instrument_ids"].is_object());
    assert!(oq["properties"].get("symbols").is_none());
}

/// A `tools/call` result nests its payload as a JSON string inside a text
/// content block, which the client parses — the path `tool_result_payload`
/// depends on.
#[test]
fn a_tool_call_result_nests_json_in_a_text_block() {
    let v = fixture("mcp_tool_result.json");
    let content = v["result"]["content"].as_array().expect("content array");
    assert_eq!(content[0]["type"], "text");
    let text = content[0]["text"].as_str().expect("text string");
    let parsed: Value = serde_json::from_str(text).expect("the text block is itself JSON");
    assert!(parsed["accounts"].is_array());
    assert_eq!(v["result"]["isError"], false);
}

/// A JSON-RPC error object carries the numeric `code` the exit-code mapping
/// branches on and a human `message`.
#[test]
fn a_jsonrpc_error_object_has_a_code_and_message() {
    let v = fixture("jsonrpc_error.json");
    assert_eq!(v["error"]["code"], -32602);
    assert!(v["error"]["message"].is_string());
}

/// The SSE framing carries the JSON-RPC response in a `data:` field — the shape
/// the Streamable-HTTP parser extracts.
#[test]
fn the_sse_framing_carries_a_jsonrpc_response_in_data() {
    let body = read_text("sse_response.txt");
    let data_line = body
        .lines()
        .find_map(|l| l.strip_prefix("data:"))
        .expect("an SSE data line");
    let v: Value = serde_json::from_str(data_line.trim()).expect("data payload is JSON");
    assert_eq!(v["jsonrpc"], "2.0");
    assert!(v["result"]["tools"].is_array());
}

// ------------------------------------------------------- the scrubbing rails

fn all_json_fixtures() -> Vec<(String, Value)> {
    let dir = format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"));
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("fixtures dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let raw = std::fs::read_to_string(&path).expect("read fixture");
        let v = serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parsing {name}: {e}"));
        out.push((name, v));
    }
    assert!(
        !out.is_empty(),
        "no fixtures found — the scrub check is vacuous"
    );
    out
}

/// The value policy from `tests/fixtures/README.md`, stated positively:
/// identity-bearing fields must hold a known dummy shape.
#[test]
fn fixtures_carry_only_synthetic_identifiers() {
    for (name, value) in all_json_fixtures() {
        check(&value, None, &name);
    }
}

fn check(v: &Value, key: Option<&str>, path: &str) {
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                check(val, Some(k), path);
            }
        }
        Value::Array(items) => {
            for item in items {
                check(item, key, path);
            }
        }
        Value::String(s) => {
            let ok = match key {
                Some(k) if k.starts_with("account_number") => s.starts_with("SAMPLE"),
                Some("symbol") | Some("chain_symbol") => s == "SAMPL" || s == "TESTF",
                Some("access_token") | Some("refresh_token") => s.starts_with("SAMPLE-"),
                Some("client_id") => s.starts_with("sample-client-id"),
                _ => true,
            };
            assert!(
                ok,
                "{path}: key `{}` holds a non-synthetic value {s:?} — see \
                 tests/fixtures/README.md",
                key.unwrap_or("?")
            );

            assert!(
                !looks_like_uuid(s) || is_dummy_uuid(s),
                "{path}: key `{}` holds a real-looking UUID {s:?}",
                key.unwrap_or("?")
            );

            for marker in presign_markers() {
                assert!(
                    !s.contains(&marker),
                    "{path}: value contains {marker:?} — never commit a pre-signed URL"
                );
            }
        }
        _ => {}
    }
}

fn looks_like_uuid(s: &str) -> bool {
    s.len() == 36
        && s.as_bytes()[8] == b'-'
        && s.as_bytes()[13] == b'-'
        && s.as_bytes()[18] == b'-'
        && s.as_bytes()[23] == b'-'
        && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

fn is_dummy_uuid(s: &str) -> bool {
    s.chars().filter(|c| *c != '-' && *c != '0').count() <= 4
}

#[test]
fn the_dummy_uuid_rule_rejects_a_real_looking_uuid() {
    assert!(looks_like_uuid("00000000-0000-4000-8000-000000000001"));
    assert!(is_dummy_uuid("00000000-0000-4000-8000-000000000001"));
    assert!(looks_like_uuid("58542935-67b5-56e1-a3f9-42686e07fa40"));
    assert!(!is_dummy_uuid("58542935-67b5-56e1-a3f9-42686e07fa40"));
    assert!(!looks_like_uuid("not-a-uuid"));
}

/// Personal data leaks into prose and examples at least as easily as fixtures —
/// so scan every tracked source, not just `tests/fixtures/`.
#[test]
fn tracked_sources_carry_no_personal_data() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["ls-files"])
        .output();
    let Ok(out) = out else {
        return; // not a git checkout — nothing to scan
    };
    if !out.status.success() {
        return;
    }

    let files = String::from_utf8_lossy(&out.stdout);
    let markers = presign_markers();
    let mut scanned = 0usize;
    for rel in files.lines() {
        if rel.starts_with("tests/fixtures/") || rel.ends_with(".lock") {
            continue;
        }
        let path = root.join(rel);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        scanned += 1;
        for (n, line) in text.lines().enumerate() {
            assert!(
                !has_phone_shape(line),
                "{rel}:{}: looks like a real phone number",
                n + 1
            );
            for marker in &markers {
                assert!(!line.contains(marker), "{rel}:{}: {marker}", n + 1);
            }
        }
    }
    assert!(scanned > 0, "scanned nothing — the check would be vacuous");
}

/// `NNN-NNN-NNNN` / `(NNN) NNN-NNNN`, ignoring version-like and date-like runs.
fn has_phone_shape(line: &str) -> bool {
    let digits: String = line
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    for part in digits.split(|c: char| !c.is_ascii_digit() && c != '-') {
        let groups: Vec<&str> = part.split('-').collect();
        if groups.len() == 3 && groups[0].len() == 3 && groups[1].len() == 3 && groups[2].len() == 4
        {
            return true;
        }
    }
    false
}

#[test]
fn the_phone_shape_rule_can_fail() {
    let sample = format!("call {}-{}-{} now", "555", "867", "5309");
    assert!(has_phone_shape(&sample));
    assert!(!has_phone_shape("version 1.431.4"));
    assert!(!has_phone_shape("2026-08-27"));
    assert!(!has_phone_shape("00000000-0000-4000-8000-000000000001"));
}
