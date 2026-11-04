//! The tools over canned answers, and [`Live`] over a local server that speaks MCP, so the
//! requests themselves can be held to read-only tools. No test reads the keychain, and no
//! test sends anything to Robinhood.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use pk_cli_mcp::conformance::{assert_conforms, assert_fits, call};
use pk_cli_mcp::{tools_list_json, CallToolResult};
use serde_json::{json, Value};

use super::*;

const ACCOUNT: &str = "SAMPLE01";
/// Crypto's numeric account number.
const RHS: &str = "00000001";

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/connector_{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// A fixture for one upstream tool.
fn answer_for(tool: Upstream) -> Value {
    fixture(match tool {
        Upstream::Accounts => "accounts",
        Upstream::Portfolio => "portfolio",
        Upstream::RealizedPnl => "realized",
        Upstream::EquityPositions | Upstream::OptionPositions | Upstream::CryptoPositions => {
            "positions"
        }
        Upstream::EquityOrders
        | Upstream::OptionOrders
        | Upstream::CryptoOrders
        | Upstream::AdvancedOrders => "orders",
        Upstream::EquityQuotes => "quotes",
    })
}

/// Answers from the fixtures, or from a queue of pages, and records what it was asked.
#[derive(Default)]
struct Canned {
    pages: Mutex<VecDeque<Value>>,
    failure: Option<CliError>,
    asked: Mutex<Vec<(&'static str, Value)>>,
    connects: Mutex<u32>,
}

impl Canned {
    fn with_pages(pages: Vec<Value>) -> Self {
        Self {
            pages: Mutex::new(pages.into()),
            ..Self::default()
        }
    }
}

impl Source for Canned {
    fn connect(&self) -> Result<Box<dyn Reader + '_>, CliError> {
        *self.connects.lock().unwrap() += 1;
        Ok(Box::new(self))
    }
}

impl Reader for &Canned {
    fn read(&mut self, tool: Upstream, arguments: Value) -> Result<Value, CliError> {
        if let Some(e) = &self.failure {
            return Err(e.clone());
        }
        self.asked
            .lock()
            .unwrap()
            .push((tool.name(), arguments.clone()));
        Ok(self
            .pages
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| answer_for(tool)))
    }
}

/// A source that fails any use: a call refused on its arguments must reach none of it.
struct Untouchable;

impl Source for Untouchable {
    fn connect(&self) -> Result<Box<dyn Reader + '_>, CliError> {
        panic!("this read the session")
    }
}

fn tools() -> RhapTools<Canned> {
    RhapTools::new(Canned::default())
}

fn fits(tools: &RhapTools<impl Source + Send + Sync>, name: &str, args: Value) -> Value {
    let result = call(tools, name, args);
    assert_fits(tools, name, &result).clone()
}

fn error_text(result: &CallToolResult) -> String {
    assert!(result.is_error(), "expected an error: {}", result.text());
    result.text().to_string()
}

/// A page of `count` rows numbered from `first`, with Robinhood's `next` link if one follows.
fn page_of(first: u32, count: u32, next: Option<&str>) -> Value {
    let rows: Vec<Value> = (first..first + count)
        .map(|n| json!({"symbol": "SAMPL", "row": n, "quantity": "1.0000"}))
        .collect();
    let next = next.map(|c| format!("https://api.example.invalid/positions/?cursor={c}"));
    json!({"data": {"positions": rows, "next": next}, "guide": "advice"})
}

fn rows_in(out: &Value) -> Vec<u64> {
    out["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["row"].as_u64().unwrap())
        .collect()
}

// ---- reads only ---------------------------------------------------------------------

#[test]
fn the_tools_conform_and_every_one_is_read_only() {
    let t = tools();
    assert_conforms(&t);
    let defs = t.tools();
    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        names,
        [
            ACCOUNTS_TOOL,
            PORTFOLIO_TOOL,
            POSITIONS_TOOL,
            ORDERS_TOOL,
            QUOTES_TOOL
        ]
    );
    for d in &defs {
        assert!(d.annotations.read_only, "{}", d.name);
        assert!(!d.annotations.destructive, "{}", d.name);
    }
}

/// The catalog's annotation, from Robinhood's own server, is what makes a tool a read. A review
/// tool is not one here, though the command line's read path allows it.
#[test]
fn every_upstream_tool_is_one_the_catalog_marks_read_only() {
    let mut names: Vec<&str> = Upstream::ALL.iter().map(|t| t.name()).collect();
    for name in &names {
        assert!(catalog::is_known(name), "{name}");
        assert_eq!(catalog::kind(name), ToolKind::Read, "{name}");
    }
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), Upstream::ALL.len());
}

/// Every tool in Robinhood's catalog that places, cancels, exercises or changes anything: none
/// is reachable by name, argument or description.
#[test]
fn no_definition_reaches_a_tool_that_places_cancels_or_changes_anything() {
    let list = tools_list_json(&tools());
    let text = list.to_string();
    let writes: Vec<&str> = catalog::tools()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .filter(|name| catalog::kind(name) != ToolKind::Read)
        .collect();
    assert_eq!(writes.len(), 29, "the catalog's write surface changed");
    for write in &writes {
        assert!(!text.contains(write), "a definition names {write}");
    }

    for tool in list["tools"].as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        for word in [
            "place", "cancel", "exercise", "transfer", "deposit", "withdraw", "buy", "sell",
            "trade", "review", "preview", "create", "update", "delete", "add", "remove", "call",
        ] {
            assert!(!name.contains(word), "{name} is named for {word}");
        }
        let args = tool["inputSchema"]["properties"].as_object().unwrap();
        for arg in args.keys() {
            assert!(
                ![
                    "tool",
                    "name",
                    "method",
                    "url",
                    "path",
                    "arguments",
                    "args",
                    "force",
                    "dry_run",
                    "side",
                    "quantity",
                    "price",
                    "amount",
                    "order_id",
                    "instrument_id",
                ]
                .contains(&arg.as_str()),
                "{name} takes {arg}, which could say what to trade or which tool to call"
            );
        }
    }
}

#[test]
fn this_module_never_calls_the_write_path() {
    let source = include_str!("../connector.rs");
    assert!(!source.contains(".write("));
    assert!(source.contains("McpClient::read(self, tool.name(), arguments)"));
}

/// A tool's arguments cannot name another tool: what reaches Robinhood is the variant's name.
#[test]
fn no_argument_or_tool_name_reaches_a_write() {
    let t = tools();
    for args in [
        json!({"tool": "place_equity_order"}),
        json!({"name": "place_equity_order", "method": "tools/call"}),
    ] {
        let _ = call(&t, ACCOUNTS_TOOL, args);
    }
    for name in [
        "place_equity_order",
        "cancel_equity_order",
        "rhap_place_order",
        "rhap_cancel_order",
        "rhap_transfer",
    ] {
        let text = pk_cli_core::block_on(t.call(name, json!({"account": ACCOUNT})))
            .unwrap_err()
            .render(&t.server());
        assert!(text.starts_with("usage:"), "{name}: {text}");
    }
    for kind in ["place", "cancel", "transfer"] {
        let text = error_text(&call(
            &t,
            ORDERS_TOOL,
            json!({"account": ACCOUNT, "kind": kind}),
        ));
        assert!(text.starts_with("usage:"), "{kind}: {text}");
    }
    let asked = t.source.asked.lock().unwrap();
    assert!(
        asked.iter().all(|(name, _)| *name == "get_accounts"),
        "{asked:?}"
    );
}

#[test]
fn listing_the_tools_reads_nothing() {
    let list = tools_list_json(&RhapTools::new(Untouchable));
    assert_eq!(list["tools"].as_array().unwrap().len(), 5);
}

#[test]
fn the_instructions_say_what_cannot_be_done_and_how_to_sign_in() {
    let text = pk_cli_mcp::instructions(&tools());
    assert!(
        text.contains("no tool can place, change or cancel"),
        "{text}"
    );
    assert!(text.contains("no default"), "{text}");
    assert!(text.contains("`rhap auth login`"), "{text}");
    assert!(text.contains("not affiliated"), "{text}");
}

// ---- results ------------------------------------------------------------------------

/// `guide` advises a model that can trade, which this one cannot, so it never arrives.
#[test]
fn no_result_carries_the_guide() {
    let t = tools();
    for (tool, args) in [
        (ACCOUNTS_TOOL, json!({})),
        (ACCOUNTS_TOOL, json!({"response_format": "detailed"})),
        (
            PORTFOLIO_TOOL,
            json!({"account": ACCOUNT, "realized": "3month"}),
        ),
        (POSITIONS_TOOL, json!({"account": ACCOUNT})),
        (
            POSITIONS_TOOL,
            json!({"account": ACCOUNT, "response_format": "detailed"}),
        ),
        (ORDERS_TOOL, json!({"account": ACCOUNT})),
        (QUOTES_TOOL, json!({"symbols": ["sampl"]})),
    ] {
        let out = fits(&t, tool, args).to_string();
        assert!(!out.contains("guide"), "{tool}: {out}");
        assert!(!out.contains("place_equity_order"), "{tool}: {out}");
    }
}

#[test]
fn a_result_with_no_data_key_still_loses_its_guide() {
    let t = RhapTools::new(Canned::with_pages(vec![json!({
        "total_value": "1.00",
        "guide": "advice",
    })]));
    let out = fits(&t, PORTFOLIO_TOOL, json!({"account": ACCOUNT}));
    assert_eq!(out["portfolio"], json!({"total_value": "1.00"}));
}

#[test]
fn concise_drops_nulls_empties_and_links_and_detailed_keeps_the_row() {
    let t = tools();
    let out = fits(&t, ORDERS_TOOL, json!({"account": ACCOUNT}));
    let order = &out["items"][0];
    assert_eq!(order["symbol"], "SAMPL");
    assert_eq!(order["state"], "filled");
    for gone in ["url", "executions"] {
        assert!(order.get(gone).is_none(), "{gone}: {order}");
    }

    let out = fits(
        &t,
        ORDERS_TOOL,
        json!({"account": ACCOUNT, "response_format": "detailed"}),
    );
    let order = &out["items"][0];
    assert!(order["url"].as_str().unwrap().starts_with("https://"));
    assert_eq!(order["executions"], json!([]));
}

/// A rounded figure would be a wrong one: what Robinhood sends as text stays that text.
#[test]
fn figures_pass_through_unchanged() {
    let t = tools();
    let out = fits(&t, POSITIONS_TOOL, json!({"account": ACCOUNT}));
    assert_eq!(out["items"][0]["quantity"], "1.5000");
    assert_eq!(out["items"][0]["average_cost"], "10.0000");

    let out = fits(&t, PORTFOLIO_TOOL, json!({"account": ACCOUNT}));
    assert_eq!(out["portfolio"]["total_value"], "1000.00");
    assert_eq!(out["portfolio"]["buying_power"], "100.00");
}

#[test]
fn accounts_list_and_keep_the_number_a_later_call_needs() {
    let out = fits(&tools(), ACCOUNTS_TOOL, json!({}));
    assert_eq!(out["returned"], 2);
    assert_eq!(out["items"][0]["account_number"], ACCOUNT);
    assert_eq!(out["items"][0]["rhs_account_number"], RHS);
}

#[test]
fn portfolio_reads_realized_only_when_asked() {
    let t = tools();
    let out = fits(&t, PORTFOLIO_TOOL, json!({"account": ACCOUNT}));
    assert!(out.get("realized").is_none(), "{out}");
    assert_eq!(t.source.asked.lock().unwrap().len(), 1);

    let t = tools();
    let out = fits(
        &t,
        PORTFOLIO_TOOL,
        json!({"account": ACCOUNT, "realized": "3month"}),
    );
    assert_eq!(out["realized"]["total_realized_gain"], "10.00");
    let asked = t.source.asked.lock().unwrap();
    assert_eq!(
        asked[1],
        (
            "get_realized_pnl",
            json!({"account_number": ACCOUNT, "span": "3month"})
        )
    );
}

#[test]
fn each_kind_reads_its_own_tool_with_the_argument_that_tool_wants() {
    let t = tools();
    for (kind, tool, args) in [
        (
            "equity",
            "get_equity_positions",
            json!({"account_number": ACCOUNT}),
        ),
        (
            "option",
            "get_option_positions",
            json!({"account_number": ACCOUNT, "nonzero": true}),
        ),
        (
            "crypto",
            "get_crypto_positions",
            json!({"rhs_account_number": RHS}),
        ),
    ] {
        let account = if kind == "crypto" { RHS } else { ACCOUNT };
        let out = fits(
            &t,
            POSITIONS_TOOL,
            json!({"account": account, "kind": kind}),
        );
        assert_eq!(out["kind"], kind);
        assert_eq!(out["account"], account);
        assert_eq!(
            t.source.asked.lock().unwrap().last().unwrap(),
            &(tool, args)
        );
    }
    for (kind, tool, args) in [
        (
            "equity",
            "get_equity_orders",
            json!({"account_number": ACCOUNT}),
        ),
        (
            "option",
            "get_option_orders",
            json!({"account_number": ACCOUNT}),
        ),
        (
            "crypto",
            "get_crypto_orders",
            json!({"rhs_account_number": RHS}),
        ),
        (
            "advanced",
            "get_advanced_orders",
            json!({"account_number": ACCOUNT}),
        ),
    ] {
        let account = if kind == "crypto" { RHS } else { ACCOUNT };
        fits(&t, ORDERS_TOOL, json!({"account": account, "kind": kind}));
        assert_eq!(
            t.source.asked.lock().unwrap().last().unwrap(),
            &(tool, args)
        );
    }
}

#[test]
fn order_filters_reach_robinhood_by_its_own_names() {
    let t = tools();
    fits(
        &t,
        ORDERS_TOOL,
        json!({"account": ACCOUNT, "state": "filled", "symbol": "sampl", "since": "2000-01-01"}),
    );
    assert_eq!(
        t.source.asked.lock().unwrap().last().unwrap().1,
        json!({
            "account_number": ACCOUNT,
            "state": "filled",
            "symbol": "SAMPL",
            "created_at_gte": "2000-01-01",
        })
    );
}

/// The catalog is Robinhood's own record of what each of its tools takes.
#[test]
fn every_argument_sent_is_one_the_catalog_lists_and_every_required_one_is_sent() {
    let t = tools();
    for args in [
        json!({"account": ACCOUNT, "kind": "equity"}),
        json!({"account": ACCOUNT, "kind": "option"}),
        json!({"account": RHS, "kind": "crypto"}),
    ] {
        fits(&t, POSITIONS_TOOL, args);
    }
    for args in [
        json!({"account": ACCOUNT, "kind": "equity", "state": "filled", "symbol": "SAMPL", "since": "2000-01-01"}),
        json!({"account": ACCOUNT, "kind": "option", "state": "filled", "since": "2000-01-01"}),
        json!({"account": RHS, "kind": "crypto", "state": "filled", "symbol": "SAMPL", "since": "2000-01-01"}),
        json!({"account": ACCOUNT, "kind": "advanced", "since": "2000-01-01"}),
    ] {
        fits(&t, ORDERS_TOOL, args);
    }
    fits(
        &t,
        PORTFOLIO_TOOL,
        json!({"account": ACCOUNT, "realized": "all"}),
    );
    fits(&t, QUOTES_TOOL, json!({"symbols": ["SAMPL"]}));
    fits(&t, ACCOUNTS_TOOL, json!({}));

    let asked = t.source.asked.lock().unwrap();
    assert_eq!(asked.len(), Upstream::ALL.len());
    for (name, sent) in asked.iter() {
        let schema = catalog::input_schema(name).unwrap();
        let sent = sent.as_object().unwrap();
        for arg in sent.keys() {
            assert!(
                schema["properties"].get(arg).is_some(),
                "{name} takes no {arg}"
            );
        }
        for required in catalog::required_fields(name) {
            assert!(sent.contains_key(&required), "{name} needs {required}");
        }
    }
}

#[test]
fn quotes_keep_every_part_of_the_answer_and_uppercase_the_symbols() {
    let t = tools();
    let out = fits(&t, QUOTES_TOOL, json!({"symbols": ["sampl"]}));
    assert_eq!(
        out["quotes"]["results"][0]["quote"]["last_trade_price"],
        "10.00"
    );
    assert_eq!(out["quotes"]["closes"][0]["close"], "9.50");
    assert_eq!(
        t.source.asked.lock().unwrap()[0],
        ("get_equity_quotes", json!({"symbols": ["SAMPL"]}))
    );
}

// ---- refused before anything is read ------------------------------------------------

#[test]
fn bad_arguments_are_usage_errors_before_any_read() {
    let t = RhapTools::new(Untouchable);
    let many: Vec<String> = (0..21).map(|i| format!("S{i}")).collect();
    for (tool, args) in [
        (PORTFOLIO_TOOL, json!({})),
        (PORTFOLIO_TOOL, json!({"account": ""})),
        (PORTFOLIO_TOOL, json!({"account": "a b"})),
        (
            PORTFOLIO_TOOL,
            json!({"account": ACCOUNT, "realized": "soon"}),
        ),
        (POSITIONS_TOOL, json!({})),
        (POSITIONS_TOOL, json!({"account": ACCOUNT, "kind": "bond"})),
        (POSITIONS_TOOL, json!({"account": ACCOUNT, "limit": 0})),
        (ORDERS_TOOL, json!({})),
        (
            ORDERS_TOOL,
            json!({"account": ACCOUNT, "since": "yesterday"}),
        ),
        (
            ORDERS_TOOL,
            json!({"account": ACCOUNT, "state": "Not A State"}),
        ),
        (
            ORDERS_TOOL,
            json!({"account": ACCOUNT, "symbol": "not a symbol"}),
        ),
        (
            ORDERS_TOOL,
            json!({"account": ACCOUNT, "kind": "advanced", "state": "filled"}),
        ),
        (
            ORDERS_TOOL,
            json!({"account": ACCOUNT, "kind": "option", "symbol": "SAMPL"}),
        ),
        (QUOTES_TOOL, json!({"symbols": []})),
        (QUOTES_TOOL, json!({"symbols": many})),
        (QUOTES_TOOL, json!({"symbols": ["not a symbol"]})),
    ] {
        let text = error_text(&call(&t, tool, args.clone()));
        assert!(text.starts_with("usage:"), "{tool} {args}: {text}");
    }
}

/// The account is never ambient, here as on the command line.
#[test]
fn an_account_tool_refuses_a_call_with_no_account() {
    for tool in [PORTFOLIO_TOOL, POSITIONS_TOOL, ORDERS_TOOL] {
        let text = error_text(&call(&RhapTools::new(Untouchable), tool, json!({})));
        assert!(text.contains("account"), "{tool}: {text}");
    }
}

// ---- paging -------------------------------------------------------------------------

/// Robinhood's cursor marks a whole page, so a page cut by the limit must not skip the rest of it.
#[test]
fn a_page_cut_by_the_limit_loses_no_rows() {
    let t = RhapTools::new(Canned::with_pages(vec![
        page_of(0, 5, Some("p1")),
        page_of(0, 5, Some("p1")),
        page_of(5, 5, None),
    ]));
    let first = fits(&t, POSITIONS_TOOL, json!({"account": ACCOUNT, "limit": 3}));
    assert_eq!(rows_in(&first), [0, 1, 2]);

    let second = fits(
        &t,
        POSITIONS_TOOL,
        json!({"account": ACCOUNT, "limit": 3, "cursor": first["next_cursor"]}),
    );
    assert_eq!(rows_in(&second), [3, 4, 5]);
    let asked = t.source.asked.lock().unwrap();
    assert!(asked[1].1.get("cursor").is_none(), "{asked:?}");
    assert_eq!(asked[2].1["cursor"], "p1");
}

#[test]
fn a_call_reads_on_through_robinhoods_pages_with_one_connection() {
    let t = RhapTools::new(Canned::with_pages(vec![
        page_of(0, 2, Some("p1")),
        page_of(2, 2, Some("p2")),
        page_of(4, 2, None),
    ]));
    let out = fits(&t, POSITIONS_TOOL, json!({"account": ACCOUNT, "limit": 5}));
    assert_eq!(rows_in(&out), [0, 1, 2, 3, 4]);
    assert!(out["next_cursor"].is_string(), "{out}");
    assert_eq!(*t.source.connects.lock().unwrap(), 1);
    assert_eq!(t.source.asked.lock().unwrap().len(), 3);
}

/// The last page of the last of Robinhood's pages says so.
#[test]
fn the_last_page_has_no_cursor() {
    let t = RhapTools::new(Canned::with_pages(vec![page_of(0, 2, None)]));
    let out = fits(&t, POSITIONS_TOOL, json!({"account": ACCOUNT}));
    assert_eq!(out["returned"], 2);
    assert!(out["next_cursor"].is_null(), "{out}");
}

/// A key that says "the start of Robinhood's page p20", as a page ending there would give.
fn cursor_for(key: &str) -> Option<String> {
    let filter = PositionsFilter {
        account: ACCOUNT,
        kind: PositionKind::Equity,
    };
    ResumeRequest::new(&ListArgs::default(), &filter)
        .unwrap()
        .page(Vec::<(String, u32)>::new(), Some(key.to_string()), "limit")
        .unwrap()
        .info
        .next_cursor
}

/// A deep page costs what its own pages cost, however far in it is.
#[test]
fn a_deep_page_reads_from_where_it_left_off() {
    let t = RhapTools::new(Canned::with_pages(vec![page_of(100, 5, Some("p21"))]));
    let out = fits(
        &t,
        POSITIONS_TOOL,
        json!({"account": ACCOUNT, "limit": 1, "cursor": cursor_for("0||p20")}),
    );
    assert_eq!(rows_in(&out), [100]);
    let asked = t.source.asked.lock().unwrap();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].1["cursor"], "p20");
}

/// Rows with their own ids, as a page of orders has.
fn orders_page(ids: &[&str]) -> Value {
    let rows: Vec<Value> = ids
        .iter()
        .map(|id| json!({"id": id, "state": "filled"}))
        .collect();
    json!({"data": {"orders": rows}})
}

fn ids_in(out: &Value) -> Vec<String> {
    out["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect()
}

/// An order placed between two pages would otherwise repeat one, and a row that leaves would
/// skip one.
#[test]
fn a_row_arriving_or_leaving_in_the_page_it_re_reads_moves_nothing() {
    let t = RhapTools::new(Canned::with_pages(vec![
        orders_page(&["a", "b", "c", "d", "e", "f"]),
        // One arrives at the top, and one already returned leaves.
        orders_page(&["new", "a", "c", "d", "e", "f"]),
    ]));
    let first = fits(&t, ORDERS_TOOL, json!({"account": ACCOUNT, "limit": 3}));
    assert_eq!(ids_in(&first), ["a", "b", "c"]);
    let second = fits(
        &t,
        ORDERS_TOOL,
        json!({"account": ACCOUNT, "limit": 3, "cursor": first["next_cursor"]}),
    );
    assert_eq!(ids_in(&second), ["d", "e", "f"]);
}

#[test]
fn when_a_pages_own_last_row_has_gone_the_place_is_lost_and_it_says_so() {
    let t = RhapTools::new(Canned::with_pages(vec![
        orders_page(&["a", "b", "c", "d", "e"]),
        orders_page(&["a", "b", "d", "e"]),
    ]));
    let first = fits(&t, ORDERS_TOOL, json!({"account": ACCOUNT, "limit": 3}));
    let text = error_text(&call(
        &t,
        ORDERS_TOOL,
        json!({"account": ACCOUNT, "limit": 3, "cursor": first["next_cursor"]}),
    ));
    assert!(text.starts_with("usage:"), "{text}");
    assert!(text.contains("place is lost"), "{text}");
    assert!(text.contains("without cursor"), "{text}");
}

/// Rows that look the same are told apart by how far in they were.
#[test]
fn rows_sharing_an_identity_resume_from_the_nearest() {
    let same = || json!({"symbol": "SAMPL", "quantity": "1.0000"});
    let row = same();
    let place = Place::parse(&Place::after_row(None, &row, 2)).unwrap();
    assert_eq!(
        place.start_in(&[same(), same(), same(), same()]).unwrap(),
        3
    );
    assert_eq!(place.start_in(&[same()]).unwrap(), 1);
}

/// A row with no id field reads as a new one if anything in it changes.
#[test]
fn a_row_with_no_id_is_known_by_all_of_it() {
    let row = json!({"quantity": "1.0000"});
    let place = Place::parse(&Place::after_row(Some("p1"), &row, 0)).unwrap();
    assert_eq!(place.cursor.as_deref(), Some("p1"));
    assert_eq!(place.start_in(std::slice::from_ref(&row)).unwrap(), 1);
    let changed = json!({"quantity": "2.0000"});
    assert!(place.start_in(&[changed]).is_err());
}

#[test]
fn a_cursor_is_bound_to_its_filters() {
    let t = RhapTools::new(Canned::with_pages(vec![page_of(0, 5, Some("p1"))]));
    let first = fits(&t, POSITIONS_TOOL, json!({"account": ACCOUNT, "limit": 2}));
    let text = error_text(&call(
        &RhapTools::new(Untouchable),
        POSITIONS_TOOL,
        json!({"account": "SAMPLE02", "limit": 2, "cursor": first["next_cursor"]}),
    ));
    assert!(text.starts_with("usage:"), "{text}");
}

/// A cursor comes back from the client, and its key reaches Robinhood.
#[test]
fn a_cursor_holding_anything_else_is_refused_before_reading() {
    for key in [
        "no-separator",
        "x||p1",
        "0|zz|p1",
        "0||p1\"; place",
        "0||p1|extra",
    ] {
        let forged = cursor_for(key);
        let text = error_text(&call(
            &RhapTools::new(Untouchable),
            POSITIONS_TOOL,
            json!({"account": ACCOUNT, "cursor": forged}),
        ));
        assert!(text.starts_with("usage:"), "{key}: {text}");
    }
}

/// Robinhood can answer an empty page with more behind it, and a call stops at its page bound.
#[test]
fn a_run_of_empty_pages_stops_at_the_bound_and_moves_on() {
    let empties: Vec<Value> = (0..MAX_PROVIDER_PAGES)
        .map(|n| page_of(0, 0, Some(&format!("p{}", n + 1))))
        .collect();
    let t = RhapTools::new(Canned::with_pages(empties));
    let out = fits(&t, POSITIONS_TOOL, json!({"account": ACCOUNT}));
    assert_eq!(out["returned"], 0);
    assert!(out["next_cursor"].is_string(), "{out}");
    assert_eq!(t.source.asked.lock().unwrap().len(), MAX_PROVIDER_PAGES);
}

// ---- failures -----------------------------------------------------------------------

/// A shape this cannot place fails: a false empty list would read as an empty account.
#[test]
fn an_answer_with_no_list_is_upstream_and_an_empty_list_is_data() {
    for changed in [
        json!({"data": {"message": "not a list"}}),
        json!({"data": "not a list"}),
        json!({"data": {"lots": [], "more": []}}),
        json!({"data": {"positions": [], "next": 7}}),
        json!({"data": {"positions": [], "next": "https://api.example.invalid/no-cursor-here"}}),
        json!({"content": []}),
        json!({"content": [{"type": "image"}]}),
        json!({"data": {"warnings": []}}),
        json!({"data": {"warnings": ["an example"]}}),
        json!({"warnings": [{"text": "no data key vouches for this"}]}),
    ] {
        let t = RhapTools::new(Canned::with_pages(vec![changed.clone()]));
        let text = error_text(&call(&t, POSITIONS_TOOL, json!({"account": ACCOUNT})));
        assert!(text.starts_with("upstream:"), "{changed}: {text}");
    }

    let t = RhapTools::new(Canned::with_pages(vec![json!({"data": {"positions": []}})]));
    let out = fits(&t, POSITIONS_TOOL, json!({"account": ACCOUNT}));
    assert_eq!(out["returned"], 0);
}

/// A cursor this could not send back is found out now, not on the next call.
#[test]
fn a_next_page_this_could_not_ask_for_is_upstream() {
    let t = RhapTools::new(Canned::with_pages(vec![page_of(0, 1, Some("a|b"))]));
    let text = error_text(&call(&t, POSITIONS_TOOL, json!({"account": ACCOUNT})));
    assert!(text.starts_with("upstream:"), "{text}");
}

#[test]
fn a_page_naming_itself_as_the_next_is_upstream_and_not_a_loop() {
    let t = RhapTools::new(Canned::with_pages(vec![
        page_of(0, 1, Some("p1")),
        page_of(1, 1, Some("p1")),
    ]));
    let text = error_text(&call(&t, POSITIONS_TOOL, json!({"account": ACCOUNT})));
    assert!(text.starts_with("upstream:"), "{text}");
    assert!(text.contains("its own page"), "{text}");
    assert_eq!(t.source.asked.lock().unwrap().len(), 2);
}

/// A figure-less answer is not a read of no figures.
#[test]
fn an_answer_holding_nothing_is_upstream_for_the_tools_that_are_not_lists() {
    for empty in [
        json!({"content": []}),
        json!({"content": [{"type": "image"}]}),
        json!({"data": {}}),
        json!({"data": {"total_value": null}}),
        json!({"data": "not an object"}),
        json!({"guide": "advice alone"}),
    ] {
        let t = RhapTools::new(Canned::with_pages(vec![empty.clone()]));
        let text = error_text(&call(&t, PORTFOLIO_TOOL, json!({"account": ACCOUNT})));
        assert!(text.starts_with("upstream:"), "{empty}: {text}");
        let t = RhapTools::new(Canned::with_pages(vec![empty.clone()]));
        let text = error_text(&call(&t, QUOTES_TOOL, json!({"symbols": ["SAMPL"]})));
        assert!(text.starts_with("upstream:"), "{empty}: {text}");
    }
}

#[test]
fn every_tool_can_return_the_answer_whole() {
    let t = tools();
    let args = json!({"account": ACCOUNT, "response_format": "detailed"});
    let out = fits(&t, PORTFOLIO_TOOL, args);
    assert!(
        out["portfolio"]
            .get("updated_at")
            .is_some_and(Value::is_null),
        "{out}"
    );
    let out = fits(
        &t,
        QUOTES_TOOL,
        json!({"symbols": ["SAMPL"], "response_format": "detailed"}),
    );
    assert!(
        out["quotes"]["results"][0]["quote"]
            .get("previous_close")
            .is_some(),
        "{out}"
    );
    let out = fits(&t, QUOTES_TOOL, json!({"symbols": ["SAMPL"]}));
    assert!(
        out["quotes"]["results"][0]["quote"]
            .get("previous_close")
            .is_none(),
        "{out}"
    );
}

/// The two numbers look alike in `rhap_accounts`, so the mistake is stopped before a read.
#[test]
fn crypto_refuses_an_account_number_and_names_the_one_it_wants() {
    let t = RhapTools::new(Untouchable);
    for tool in [POSITIONS_TOOL, ORDERS_TOOL] {
        let text = error_text(&call(
            &t,
            tool,
            json!({"account": ACCOUNT, "kind": "crypto"}),
        ));
        assert!(text.starts_with("usage:"), "{tool}: {text}");
        assert!(text.contains("rhs_account_number"), "{tool}: {text}");
    }
}

#[test]
fn a_list_among_other_arrays_is_found_by_its_name() {
    let t = RhapTools::new(Canned::with_pages(vec![json!({
        "data": {"orders": [{"state": "filled"}], "warnings": ["example"]},
    })]));
    let out = fits(&t, ORDERS_TOOL, json!({"account": ACCOUNT}));
    assert_eq!(out["items"], json!([{"state": "filled"}]));
}

#[test]
fn a_refused_session_says_to_sign_in() {
    let t = RhapTools::new(Canned {
        failure: Some(CliError::Auth(
            "the MCP server rejected the access token (HTTP 401/403) — run `rhap auth login`"
                .into(),
        )),
        ..Canned::default()
    });
    let text = error_text(&call(&t, ACCOUNTS_TOOL, json!({})));
    assert!(text.starts_with("auth:"), "{text}");
    assert!(text.contains("rhap auth login"), "{text}");
}

#[test]
fn no_keychain_text_reaches_the_model() {
    let t = RhapTools::new(Canned {
        failure: Some(CliError::Keychain(
            "reading piekstra.rhap/tokens: denied".into(),
        )),
        ..Canned::default()
    });
    let text = error_text(&call(&t, ACCOUNTS_TOOL, json!({})));
    assert!(text.contains("keychain"), "{text}");
    assert!(!text.contains("tokens"), "{text}");
    assert!(!text.contains("piekstra.rhap"), "{text}");
}

#[test]
fn a_tool_error_from_robinhood_is_upstream() {
    let t = RhapTools::new(Canned {
        failure: Some(CliError::Upstream(
            "the MCP tool reported an error: an example refusal".into(),
        )),
        ..Canned::default()
    });
    let text = error_text(&call(&t, POSITIONS_TOOL, json!({"account": ACCOUNT})));
    assert!(text.starts_with("upstream:"), "{text}");
    assert!(text.contains("an example refusal"), "{text}");
}

// ---- Live, over a local server ------------------------------------------------------

/// Each request a stub received, with its `Authorization` line.
type Seen = Arc<Mutex<Vec<(Value, String)>>>;

/// A plain HTTP server that speaks enough MCP to answer the fixtures, keeping each request.
fn stub() -> (String, Seen) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { return };
            let mut raw = Vec::new();
            let mut buf = [0u8; 8192];
            let body = loop {
                let n = conn.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break None;
                }
                raw.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&raw).to_string();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let wanted = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if body.len() >= wanted {
                        let auth = head
                            .lines()
                            .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                            .unwrap_or("")
                            .to_string();
                        break Some((body.to_string(), auth));
                    }
                }
            };
            let Some((body, auth)) = body else { continue };
            let request: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            log.lock().unwrap().push((request.clone(), auth));
            let reply = match request["method"].as_str() {
                Some("initialize") => json!({
                    "jsonrpc": "2.0",
                    "id": request["id"],
                    "result": {"protocolVersion": "2025-06-18", "capabilities": {}},
                }),
                Some("tools/call") => {
                    let name = request["params"]["name"].as_str().unwrap_or("");
                    let tool = Upstream::ALL.iter().find(|t| t.name() == name);
                    let payload = tool.map_or(json!({}), |t| answer_for(*t));
                    json!({
                        "jsonrpc": "2.0",
                        "id": request["id"],
                        "result": {
                            "content": [{"type": "text", "text": payload.to_string()}],
                            "isError": false,
                        },
                    })
                }
                _ => Value::Null,
            };
            let text = if reply.is_null() {
                String::new()
            } else {
                reply.to_string()
            };
            let answer = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{text}",
                text.len()
            );
            let _ = conn.write_all(answer.as_bytes());
        }
    });
    (url, seen)
}

fn live(url: String, dir: &std::path::Path) -> RhapTools<Live> {
    let store = ConfigStore::new("rhap").with_override(Some(dir.join("config.json")));
    RhapTools::new(Live {
        store,
        connect: Box::new(move |_| McpClient::new(&url, "dummy-access-token", false)),
    })
}

/// The whole surface over the real client: every request any tool sends.
#[test]
fn every_request_any_tool_sends_is_a_read_the_catalog_allows() {
    let dir = tempfile::tempdir().unwrap();
    let (url, seen) = stub();
    let t = live(url, dir.path());
    fits(&t, ACCOUNTS_TOOL, json!({}));
    fits(
        &t,
        PORTFOLIO_TOOL,
        json!({"account": ACCOUNT, "realized": "year"}),
    );
    for kind in ["equity", "option", "crypto"] {
        let account = if kind == "crypto" { RHS } else { ACCOUNT };
        fits(
            &t,
            POSITIONS_TOOL,
            json!({"account": account, "kind": kind}),
        );
    }
    for kind in ["equity", "option", "crypto", "advanced"] {
        let account = if kind == "crypto" { RHS } else { ACCOUNT };
        fits(&t, ORDERS_TOOL, json!({"account": account, "kind": kind}));
    }
    fits(&t, QUOTES_TOOL, json!({"symbols": ["SAMPL"]}));

    let seen = seen.lock().unwrap().clone();
    let mut called: Vec<String> = Vec::new();
    for (request, auth) in &seen {
        assert_eq!(
            auth.to_lowercase(),
            "authorization: bearer dummy-access-token"
        );
        match request["method"].as_str().unwrap() {
            "initialize" | "notifications/initialized" => {}
            "tools/call" => {
                let name = request["params"]["name"].as_str().unwrap();
                assert_eq!(catalog::kind(name), ToolKind::Read, "{name}");
                called.push(name.to_string());
            }
            other => panic!("a tool sent {other}"),
        }
    }
    called.sort();
    let mut every: Vec<String> = Upstream::ALL.iter().map(|t| t.name().to_string()).collect();
    every.sort();
    assert_eq!(called, every, "each upstream tool once");
}

#[test]
fn a_broken_config_is_one_calls_error_and_names_no_path() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::new("rhap").with_override(Some(dir.path().join("config.json")));
    let creds = CredentialStore::for_binary("rhap-test-never-read");
    let t = RhapTools::new(Live::new(store, creds, false));
    std::fs::write(dir.path().join("config.json"), "{not json").unwrap();
    assert_eq!(tools_list_json(&t)["tools"].as_array().unwrap().len(), 5);
    let text = error_text(&call(&t, ACCOUNTS_TOOL, json!({})));
    assert!(text.contains("rhap config show"), "{text}");
    assert!(!text.contains(dir.path().to_str().unwrap()), "{text}");
}

#[test]
fn the_fixtures_hold_only_dummies() {
    let accounts = fixture("accounts");
    for account in accounts["data"]["accounts"].as_array().unwrap() {
        assert!(account["account_number"]
            .as_str()
            .unwrap()
            .starts_with("SAMPLE"));
        assert!(account["rhs_account_number"]
            .as_str()
            .unwrap()
            .starts_with("0000000"));
    }
    let orders = fixture("orders");
    for order in orders["data"]["orders"].as_array().unwrap() {
        assert!(order["created_at"]
            .as_str()
            .unwrap()
            .starts_with("2000-01-"));
        assert!(order["url"].as_str().unwrap().contains(".invalid/"));
    }
}
