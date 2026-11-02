//! The embedded MCP tool catalog — a snapshot of the live `tools/list`
//! (captured 2026-09-17, `resources/tools_catalog.json`; 78 tools).
//!
//! It is the deterministic, offline source of truth for:
//!
//! - `call` — validating a tool name (unknown → exit 4), classifying it as
//!   read / review / write (from the server's `readOnlyHint` annotation), and
//!   deciding whether it is account-scoped (so a mutating account-scoped tool
//!   can be refused without an explicit `account_number`).
//! - `tools show` — printing a tool's full `inputSchema` without a network call.
//!
//! `tools list` stays a **live** passthrough (the server's current surface);
//! this catalog is the reference the safety checks are pinned to. Re-introspect
//! and regenerate the file if the server surface changes.

use std::sync::OnceLock;

use serde_json::Value;

const CATALOG_JSON: &str = include_str!("../resources/tools_catalog.json");

fn catalog() -> &'static Value {
    static CATALOG: OnceLock<Value> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(CATALOG_JSON).expect("embedded tools_catalog.json is valid JSON")
    })
}

/// The capture date of the embedded snapshot.
pub fn captured() -> &'static str {
    catalog()
        .get("captured")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
}

/// Every tool object in the catalog.
pub fn tools() -> &'static [Value] {
    static TOOLS: OnceLock<Vec<Value>> = OnceLock::new();
    TOOLS
        .get_or_init(|| {
            catalog()
                .get("tools")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .as_slice()
}

/// The full tool object (name, description, inputSchema) for `name`.
pub fn tool(name: &str) -> Option<&'static Value> {
    tools()
        .iter()
        .find(|t| t.get("name").and_then(Value::as_str) == Some(name))
}

/// Whether `name` is a tool the catalog knows.
pub fn is_known(name: &str) -> bool {
    tool(name).is_some()
}

/// The `inputSchema` object for `name`.
pub fn input_schema(name: &str) -> Option<&'static Value> {
    tool(name).and_then(|t| t.get("inputSchema"))
}

/// The `required` field list from a tool's schema.
pub fn required_fields(name: &str) -> Vec<String> {
    input_schema(name)
        .and_then(|s| s.get("required"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a tool may change account/portfolio/watchlist/scan/alert state.
///
/// A tool is a read **only** when the catalog marks it
/// `annotations.readOnlyHint: true`; everything else, including an unknown
/// name, is a write. The rule used to be a name-prefix list, which classified
/// `delete_alert` and `mark_alerts_read` as reads (no confirmation) because
/// nobody had added `delete_`/`mark_` to it (audit 2026-09-23, security #2).
/// Deriving from the server's own annotation fails closed: a new write tool is
/// guarded the day it enters the catalog. [`kind`] is the single classifier
/// shared by the `call` guard, the typed write path and the read path.
pub fn is_mutating(name: &str) -> bool {
    kind(name) != ToolKind::Read
}

/// How rhap treats a tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    /// Annotated `readOnlyHint: true`: runs unguarded.
    Read,
    /// An order review/preview: unannotated by the server, so `call` guards it
    /// like a write, but it places nothing, so the typed review commands and
    /// the review-before-place step may run it.
    Review,
    /// Everything else: needs `--dry-run`, confirmation or `--force`.
    Write,
}

/// Each order-placing tool and the review tool it must call first.
pub const REVIEWED_PLACES: &[(&str, &str)] = &[
    ("place_equity_order", "review_equity_order"),
    ("place_option_order", "review_option_order"),
    ("place_advanced_order", "review_advanced_order"),
    ("place_crypto_order", "preview_crypto_order"),
];

/// The review tool `place` must call first; `None` for every other tool.
pub fn required_review(place: &str) -> Option<&'static str> {
    REVIEWED_PLACES
        .iter()
        .find(|(p, _)| *p == place)
        .map(|(_, review)| *review)
}

/// Classify `name`. Unknown names are writes.
pub fn kind(name: &str) -> ToolKind {
    if is_read_only(name) {
        ToolKind::Read
    } else if REVIEWED_PLACES.iter().any(|(_, r)| *r == name) {
        ToolKind::Review
    } else {
        ToolKind::Write
    }
}

/// Whether the catalog marks `name` `annotations.readOnlyHint: true`.
fn is_read_only(name: &str) -> bool {
    tool(name)
        .and_then(|t| t.pointer("/annotations/readOnlyHint"))
        .and_then(Value::as_bool)
        == Some(true)
}

/// Whether a tool is account-scoped — i.e. its schema **requires**
/// `account_number`. Used to refuse a mutating account-scoped `call` that omits
/// the account (the no-ambient-account invariant on the generic path).
pub fn is_account_scoped(name: &str) -> bool {
    required_fields(name).iter().any(|f| f == "account_number")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_kinds() {
        assert_eq!(kind("get_accounts"), ToolKind::Read);
        assert_eq!(kind("run_scan"), ToolKind::Read);
        assert_eq!(kind("review_option_order"), ToolKind::Review);
        assert_eq!(kind("preview_crypto_order"), ToolKind::Review);
        assert_eq!(kind("delete_alert"), ToolKind::Write);
        assert_eq!(kind("nope"), ToolKind::Write);
        assert!(is_mutating("review_option_order"), "call guards reviews");
    }

    /// Every place tool in the catalog has a review step, and every review
    /// named is a real, non-read-only tool.
    #[test]
    fn every_place_tool_is_reviewed() {
        for t in tools() {
            let name = t.get("name").and_then(Value::as_str).unwrap();
            if name.starts_with("place_") {
                let review =
                    required_review(name).unwrap_or_else(|| panic!("`{name}` has no review step"));
                assert!(is_known(review), "{review}");
                assert_eq!(kind(review), ToolKind::Review);
            }
        }
        assert_eq!(required_review("cancel_equity_order"), None);
        for (place, _) in REVIEWED_PLACES {
            assert!(is_known(place), "{place}");
        }
    }

    #[test]
    fn the_catalog_matches_the_last_capture() {
        // Re-captured 2026-09-17: the surface grew from 54 to 78 — news, SEC
        // filings, alerts, crypto, politician trades, and OCO advanced orders.
        assert_eq!(tools().len(), 78, "the live surface had 78 tools");
        assert_eq!(captured(), "2026-09-17");
    }

    #[test]
    fn the_2026_09_17_additions_are_present() {
        for name in [
            "get_equity_news",
            "get_sec_filing_index",
            "get_alert_log",
            "place_advanced_order",
            "review_advanced_order",
            "get_advanced_orders",
            "get_politician_trades",
        ] {
            assert!(is_known(name), "{name} must be in the catalog");
        }
    }

    #[test]
    fn known_and_unknown_tools() {
        assert!(is_known("get_accounts"));
        assert!(is_known("place_option_order"));
        assert!(!is_known("delete_everything"));
    }

    #[test]
    fn required_fields_come_from_the_real_schema() {
        assert_eq!(required_fields("get_portfolio"), vec!["account_number"]);
        let place = required_fields("place_equity_order");
        for f in ["account_number", "symbol", "side", "type"] {
            assert!(place.iter().any(|x| x == f), "missing {f}");
        }
        assert!(required_fields("get_accounts").is_empty());
    }

    #[test]
    fn mutating_classification_follows_the_read_only_hint() {
        for m in [
            "place_equity_order",
            "cancel_option_order",
            "exercise_option",
            "cancel_option_exercise",
            "create_watchlist",
            "update_scan_filters",
            "add_to_watchlist",
            "remove_option_from_watchlist",
            "follow_watchlist",
            "unfollow_watchlist",
            // Reads under the old prefix list (audit security #2).
            "delete_alert",
            "mark_alerts_read",
            // Not annotated read-only by the server, so treated as writes.
            "review_equity_order",
            "preview_crypto_order",
            // An unknown name fails closed.
            "delete_everything",
        ] {
            assert!(is_mutating(m), "{m} should be mutating");
        }
        for r in [
            "get_accounts",
            "get_portfolio",
            "search",
            "get_scans",
            "get_scanner_filter_specs",
            // Annotated read-only: it returns matches and changes nothing.
            "run_scan",
        ] {
            assert!(!is_mutating(r), "{r} should be a read");
        }
    }

    /// Every catalog tool without `readOnlyHint: true` is guarded and every
    /// tool with it is not, checked against the raw annotation.
    #[test]
    fn every_tool_not_marked_read_only_is_guarded() {
        let mut writes = 0;
        for t in tools() {
            let name = t.get("name").and_then(Value::as_str).unwrap();
            let read_only = t.pointer("/annotations/readOnlyHint") == Some(&Value::Bool(true));
            assert_eq!(
                is_mutating(name),
                !read_only,
                "`{name}`: readOnlyHint={read_only} but is_mutating={}",
                is_mutating(name)
            );
            if !read_only {
                writes += 1;
            }
        }
        // 2026-09-17 capture: 49 tools annotated read-only, 29 not.
        assert_eq!(writes, 29, "the write surface changed; review the guards");
    }

    /// Coverage gate: every tool in the live catalog is reachable through a
    /// typed command (the two `review_*` tools are invoked by `order place` /
    /// `option order place`). `call` is a backstop for anything not listed here.
    /// If the server surface grows, this fails until the new tool is mapped.
    #[test]
    fn every_catalog_tool_has_a_typed_command() {
        // The tool each typed command targets (2026-09-17 surface).
        const TYPED: &[&str] = &[
            // accounts / equity core
            "get_accounts",
            "get_equity_positions",
            "get_equity_orders",
            "get_equity_quotes",
            "review_equity_order",
            "place_equity_order",
            "cancel_equity_order",
            // equity extras
            "get_equity_fundamentals",
            "get_equity_historicals",
            "get_equity_price_book",
            "get_equity_tax_lots",
            "get_equity_technical_indicators",
            "get_equity_tradability",
            // options
            "get_option_positions",
            "get_option_orders",
            "get_option_quotes",
            "get_option_chains",
            "get_option_instruments",
            "get_option_historicals",
            "get_option_watchlist",
            "review_option_order",
            "place_option_order",
            "cancel_option_order",
            "exercise_option",
            "cancel_option_exercise",
            // portfolio / pnl / margin
            "get_portfolio",
            "get_pnl_trade_history",
            "get_realized_pnl",
            "get_limited_margin_upgrade_info",
            "get_option_level_upgrade_info",
            // indexes
            "get_indexes",
            "get_index_quotes",
            "get_index_historicals",
            // market / research
            "get_earnings_calendar",
            "get_earnings_results",
            "get_financials",
            "search",
            // watchlists
            "get_watchlists",
            "get_watchlist_items",
            "get_popular_watchlists",
            "create_watchlist",
            "update_watchlist",
            "add_to_watchlist",
            "remove_from_watchlist",
            "add_option_to_watchlist",
            "remove_option_from_watchlist",
            "follow_watchlist",
            "unfollow_watchlist",
            // scans
            "get_scans",
            "get_scanner_filter_specs",
            "run_scan",
            "create_scan",
            "update_scan_config",
            "update_scan_filters",
            // 2026-09-17 additions: news, SEC filings, alerts, OCO advanced
            // orders, politician trades, crypto (src/commands/extras.rs)
            "get_equity_news",
            "get_sec_filing_index",
            "get_sec_filing",
            "get_sec_filing_facts",
            "get_sec_filing_facts_catalog",
            "get_alerts",
            "get_alert_log",
            "create_alert",
            "update_alert",
            "delete_alert",
            "mark_alerts_read",
            "get_advanced_orders",
            "review_advanced_order",
            "place_advanced_order",
            "cancel_advanced_order",
            "get_politician_trades",
            "get_crypto_account_onboarding_info",
            "get_currency_pairs",
            "get_crypto_quotes",
            "get_crypto_positions",
            "get_crypto_orders",
            "preview_crypto_order",
            "place_crypto_order",
            "cancel_crypto_order",
        ];
        for t in tools() {
            let name = t.get("name").and_then(Value::as_str).unwrap();
            assert!(
                TYPED.contains(&name),
                "catalog tool `{name}` has no typed command (map it or use `call`)"
            );
        }
        // And every name we claim to map is a real tool.
        for name in TYPED {
            assert!(is_known(name), "TYPED lists unknown tool `{name}`");
        }
        assert_eq!(TYPED.len(), 78, "expected full 78-tool typed coverage");
    }

    #[test]
    fn account_scoping_follows_the_schema() {
        assert!(is_account_scoped("exercise_option"));
        assert!(is_account_scoped("place_equity_order"));
        assert!(is_account_scoped("get_portfolio"));
        // Watchlist/scan writes are not account-scoped.
        assert!(!is_account_scoped("create_watchlist"));
        assert!(!is_account_scoped("run_scan"));
        assert!(!is_account_scoped("add_to_watchlist"));
    }
}
