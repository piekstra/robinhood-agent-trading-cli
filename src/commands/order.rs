//! The write commands — `order place` and `order cancel` — and the guards that
//! stand between them and real money.
//!
//! Three layers protect a live order:
//!
//! 1. **`--dry-run`** builds and validates the exact JSON-RPC request and
//!    prints it *without sending* — no keychain read, no network. This is the
//!    only path the test suite and `make smoke` ever exercise.
//! 2. **Confirmation** (SPEC v1 §mutations): a live place/cancel prompts for
//!    a typed confirmation unless `--force`, and exits `6` when there is nobody
//!    to ask (non-interactive: `--json`, a pipe, or no TTY).
//! 3. **Review-then-place**: a live equity order calls `review_equity_order`
//!    first and echoes the broker's preview before `place_equity_order`, per
//!    the documented MCP order flow.
//!
//! Robinhood's Agentic MCP grants WRITE only to the funded Agentic account, so
//! these commands take a required, per-invocation `--account` (never ambient —
//! see `config.rs`). The equity order schemas here are VERIFIED against the live
//! `tools/list` (2026-08-27); each is built in a tested function. The shared
//! write runner ([`run_write`]) is reused by the option order commands.

use std::io::{IsTerminal, Write};

use pk_cli_core::{output, CliError};
use serde_json::{json, Value};

use super::Ctx;
use crate::mcp;

/// A validated order side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn parse(raw: &str) -> Result<Self, CliError> {
        match raw.trim().to_lowercase().as_str() {
            "buy" => Ok(Side::Buy),
            "sell" => Ok(Side::Sell),
            other => Err(CliError::Usage(format!(
                "--side must be `buy` or `sell` (got `{other}`)"
            ))),
        }
    }
    pub fn wire(self) -> &'static str {
        match self {
            Side::Buy => "buy",
            Side::Sell => "sell",
        }
    }
}

/// Validate `--tif` and map it to the broker's wire spelling.
///
/// `time_in_force` is a verified field (2026-08-27); the accepted value spelling
/// follows Robinhood's convention — `day` maps to `gfd` ("good for day"), the
/// rest pass through. Reconcile the exact value set from `tools list` if the
/// server rejects one. See docs/api.md §5.
pub fn tif_wire(raw: &str) -> Result<String, CliError> {
    match raw.trim().to_lowercase().as_str() {
        "day" | "gfd" => Ok("gfd".into()),
        "gtc" => Ok("gtc".into()),
        "ioc" => Ok("ioc".into()),
        "fok" => Ok("fok".into()),
        other => Err(CliError::Usage(format!(
            "--tif must be one of day|gtc|ioc|fok (got `{other}`)"
        ))),
    }
}

/// Whether `s` is a plain decimal: `^\d+(\.\d+)?$`. ASCII digits only.
fn is_plain_decimal(s: &str) -> bool {
    let all_digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    match s.split_once('.') {
        Some((int, frac)) => all_digits(int) && all_digits(frac),
        None => all_digits(s),
    }
}

/// Validate a decimal string (quantity or price) is a positive number written
/// as `^\d+(\.\d+)?$`, keeping it as a string so no float ever touches an
/// order value.
///
/// This used to be `str::parse::<f64>() > 0`, which accepts `inf`, `Infinity`,
/// `1e3` and `1e999` (audit 2026-09-23, security #6). Those reached the broker
/// verbatim, and exponent notation walked past [`is_fractional`]: `5e-1` has
/// no `.`, so a half share looked whole. Only the plain form is accepted now.
pub fn positive_decimal(field: &str, raw: &str) -> Result<String, CliError> {
    let s = raw.trim();
    if !is_plain_decimal(s) || !s.bytes().any(|b| (b'1'..=b'9').contains(&b)) {
        return Err(CliError::Usage(format!(
            "{field} must be a positive decimal written as digits with an optional fraction, \
             like 10 or 1.25 (got `{raw}`)"
        )));
    }
    Ok(s.to_string())
}

/// Validate a positive whole number (`^\d+$`, not zero) — an option contract
/// count or a whole-share quantity — keeping it as a string.
pub fn positive_integer(field: &str, raw: &str) -> Result<String, CliError> {
    let s = raw.trim();
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) || !s.bytes().any(|b| b != b'0') {
        return Err(CliError::Usage(format!(
            "{field} must be a positive whole number (got `{raw}`)"
        )));
    }
    Ok(s.to_string())
}

/// The equity sessions `place_equity_order` accepts (schema, 2026-09-17).
pub const EQUITY_MARKET_HOURS: &[&str] = &["regular_hours", "extended_hours", "all_day_hours"];

/// Validate `--market-hours` against the equity sessions.
pub fn market_hours_wire(raw: &str) -> Result<&'static str, CliError> {
    let s = raw.trim().to_ascii_lowercase();
    EQUITY_MARKET_HOURS
        .iter()
        .copied()
        .find(|h| *h == s)
        .ok_or_else(|| {
            CliError::Usage(format!(
                "--market-hours must be one of {} (got `{raw}`)",
                EQUITY_MARKET_HOURS.join("|")
            ))
        })
}

/// Equity order terms after validation — the single validation path shared by
/// the pre-session argument check in `main.rs` and [`place`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EquityTerms {
    pub side: Side,
    pub tif: String,
    pub order_type: &'static str,
    pub quantity: Option<String>,
    pub dollar_amount: Option<String>,
    pub limit_price: Option<String>,
    pub market_hours: &'static str,
}

/// Validate every `order place` term from the arguments alone.
pub fn validate_place(
    side: &str,
    qty: Option<&str>,
    dollar_amount: Option<&str>,
    order_type: &str,
    limit: Option<&str>,
    market_hours: &str,
    tif: &str,
) -> Result<EquityTerms, CliError> {
    let side = Side::parse(side)?;
    let tif = tif_wire(tif)?;
    let market_hours = market_hours_wire(market_hours)?;
    let order_type = match order_type.trim().to_ascii_lowercase().as_str() {
        "limit" => "limit",
        "market" => "market",
        other => {
            return Err(CliError::Usage(format!(
                "--type must be `limit` or `market` (got `{other}`)"
            )))
        }
    };

    if qty.is_some() == dollar_amount.is_some() {
        return Err(CliError::Usage(
            "provide exactly one of --qty or --dollar-amount".to_string(),
        ));
    }
    if dollar_amount.is_some() && order_type != "market" {
        return Err(CliError::Usage(
            "--dollar-amount requires --type market (the server derives the share count)"
                .to_string(),
        ));
    }
    let quantity = qty.map(|q| positive_decimal("--qty", q)).transpose()?;
    let dollar_amount = dollar_amount
        .map(|d| positive_decimal("--dollar-amount", d))
        .transpose()?;

    // The rule that made fractional exits impossible before 2026-09-18. Sound
    // only because `positive_decimal` refuses exponent notation.
    if let Some(q) = quantity.as_deref() {
        if is_fractional(q) {
            if order_type == "limit" {
                return Err(CliError::Usage(format!(
                    "quantity {q} is fractional, and the broker rejects fractional quantities on limit orders (`API 400: Limit order quantity cannot include fractional shares`). Use --type market — the only way to exit a fractional position. Check the spread first: a market order has no price bound."
                )));
            }
            if market_hours != "regular_hours" {
                return Err(CliError::Usage(
                    "fractional quantities are regular_hours only".to_string(),
                ));
            }
        }
    }
    // The schema: extended_hours and all_day_hours execute limit orders only.
    if order_type == "market" && market_hours != "regular_hours" {
        return Err(CliError::Usage(format!(
            "a market order is regular_hours only; {market_hours} executes limit orders only"
        )));
    }

    let limit_price = match (order_type, limit) {
        ("limit", Some(l)) => Some(positive_decimal("--limit", l)?),
        ("limit", None) => {
            return Err(CliError::Usage(
                "--limit is required for --type limit".to_string(),
            ))
        }
        ("market", Some(_)) => {
            return Err(CliError::Usage(
                "--limit is not valid with --type market".to_string(),
            ))
        }
        _ => None,
    };

    Ok(EquityTerms {
        side,
        tif,
        order_type,
        quantity,
        dollar_amount,
        limit_price,
        market_hours,
    })
}

/// The validated inputs for a place. Keeping this separate from clap lets the
/// builder be unit-tested without a parser.
pub struct PlaceInput<'a> {
    pub account: &'a str,
    pub symbol: &'a str,
    pub side: Side,
    /// Share count. Exactly one of `quantity` or `dollar_amount` is set.
    pub quantity: Option<&'a str>,
    /// USD notional. Market orders only — the server derives the share count.
    pub dollar_amount: Option<&'a str>,
    /// "limit" or "market".
    pub order_type: &'a str,
    /// Set for limit orders, absent for market.
    pub limit_price: Option<&'a str>,
    pub market_hours: &'a str,
    pub tif: &'a str,
}

/// True when a share count carries a non-zero fraction.
///
/// The broker rejects fractional quantities on limit orders
/// (`API 400: Limit order quantity cannot include fractional shares`), and it
/// only rejects them *after* the review round-trip. Before 2026-09-18 this
/// command built `type: "limit"` unconditionally, so a fractional position
/// could not be exited through rhap at all — the 09-18 core liquidation had
/// to fall back to the raw `call` passthrough, which has no dry-run
/// ergonomics and no argument validation. Catch it at argument time instead.
pub fn is_fractional(qty: &str) -> bool {
    match qty.split_once('.') {
        Some((_, frac)) => frac.chars().any(|c| c != '0'),
        None => false,
    }
}

/// Build the `place_equity_order` arguments.
///
/// Schema VERIFIED against the live `tools/list` (2026-08-27): required
/// [account_number, symbol, side, type]. The order-kind field is **`type`**
/// (not `order_type`), and there is **no** `extended_hours` field (the optional
/// hours toggle is `market_hours`). `review_equity_order` takes the same shape.
pub fn place_args(input: &PlaceInput) -> Value {
    let mut a = json!({
        "account_number": input.account,
        "symbol": input.symbol,
        "side": input.side.wire(),
        "type": input.order_type,
        "time_in_force": input.tif,
        "market_hours": input.market_hours,
    });
    let m = a.as_object_mut().expect("object");
    // Exactly one sizing field — the schema rejects both together.
    if let Some(q) = input.quantity {
        m.insert("quantity".into(), Value::String(q.to_string()));
    }
    if let Some(d) = input.dollar_amount {
        m.insert("dollar_amount".into(), Value::String(d.to_string()));
    }
    // limit_price must be ABSENT on a market order, not null.
    if let Some(px) = input.limit_price {
        m.insert("limit_price".into(), Value::String(px.to_string()));
    }
    a
}

/// Build the `cancel_equity_order` arguments (schema VERIFIED 2026-08-27:
/// {account_number, order_id}). The account scopes the cancel so it can never be
/// misdirected to another account; the order id names the order within it.
pub fn cancel_args(account: &str, order_id: &str) -> Value {
    json!({ "account_number": account, "order_id": order_id })
}

/// `order place --account <ID>`. Validates, then either prints the request
/// (`--dry-run`) or — past the confirmation guard — reviews and places it live.
///
/// The account is required and passed explicitly per-invocation; it is never
/// read from config or env (see `config.rs`).
#[allow(clippy::too_many_arguments)]
pub fn place(
    ctx: &Ctx,
    account: &str,
    symbol: &str,
    side: &str,
    qty: Option<&str>,
    dollar_amount: Option<&str>,
    order_type: &str,
    limit: Option<&str>,
    market_hours: &str,
    tif: &str,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    // Validate everything from the arguments alone, before any keychain or
    // network — so a typo fails the same way whether or not the machine has a
    // session (SPEC v1 §1.2). `main.rs` runs the same function before config
    // load; this is the one implementation.
    let t = validate_place(
        side,
        qty,
        dollar_amount,
        order_type,
        limit,
        market_hours,
        tif,
    )?;

    let input = PlaceInput {
        account,
        symbol: &symbol.trim().to_uppercase(),
        side: t.side,
        quantity: t.quantity.as_deref(),
        dollar_amount: t.dollar_amount.as_deref(),
        order_type: t.order_type,
        limit_price: t.limit_price.as_deref(),
        market_hours: t.market_hours,
        tif: &t.tif,
    };
    let args = place_args(&input);
    let size = match (&t.quantity, &t.dollar_amount) {
        (Some(q), _) => format!("{q} sh"),
        (_, Some(d)) => format!("${d}"),
        _ => unreachable!("exactly one sizing field is validated above"),
    };
    let px = match &t.limit_price {
        Some(l) => format!("@ {l}"),
        None => "at MARKET (no price bound)".to_string(),
    };
    let summary = format!(
        "{} {} {} {} ({}, {}) on account {}",
        t.side.wire().to_uppercase(),
        size,
        input.symbol,
        px,
        t.tif,
        t.market_hours,
        account
    );
    run_write(
        ctx,
        "order-dry-run",
        "order-place",
        "place_equity_order",
        args,
        dry_run,
        force,
        &format!("Place a LIVE order: {summary}?"),
    )
}

/// A cancel's order id, trimmed; blank is a usage error.
pub(crate) fn require_order_id(raw: &str) -> Result<&str, CliError> {
    let id = raw.trim();
    if id.is_empty() {
        return Err(CliError::Usage("an order id is required".into()));
    }
    Ok(id)
}

/// `order cancel <id> --account <ID>`. The account is required and passed
/// explicitly per-invocation (never defaulted — see `config.rs`).
pub fn cancel(
    ctx: &Ctx,
    account: &str,
    order_id: &str,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    let order_id = require_order_id(order_id)?;
    let args = cancel_args(account, order_id);
    run_write(
        ctx,
        "order-dry-run",
        "order-cancel",
        "cancel_equity_order",
        args,
        dry_run,
        force,
        &format!("Cancel LIVE order {order_id} on account {account}?"),
    )
}

/// The shared write runner, reused by the equity and option order commands:
/// dry-run prints and stops; a live run confirms (only for a genuinely mutating
/// tool), optionally reviews first, then calls the tool. Routing through
/// [`crate::catalog::is_mutating`] keeps the confirmation gate tied to the tool
/// classification rather than to the call site.
///
/// `dry_run_schema` / `result_schema` tag the emitted DTO (`order-dry-run` vs
/// `option-order-dry-run`, `order-place` vs `option-order-place`, …).
///
/// The review step comes from [`crate::catalog::required_review`], not from
/// the call site, so a place tool always reviews first and no caller can
/// omit it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_write(
    ctx: &Ctx,
    dry_run_schema: &str,
    result_schema: &str,
    tool: &str,
    args: Value,
    dry_run: bool,
    force: bool,
    question: &str,
) -> Result<(), CliError> {
    if dry_run {
        return emit_dry_run(ctx, dry_run_schema, tool, args);
    }
    if crate::catalog::is_mutating(tool) {
        guard(ctx, force, question)?;
    }

    // Owner-confirmed live path (never run by the test suite).
    let mut client = ctx.client()?;
    if let Some(rt) = crate::catalog::required_review(tool) {
        if !ctx.common.quiet {
            eprintln!("reviewing order…");
        }
        let review = client.read(rt, args.clone())?;
        if !ctx.common.quiet {
            eprintln!("review: {review}");
        }
    }
    let result = client.write(tool, args)?;
    emit_result(ctx, result_schema, tool, result);
    Ok(())
}

/// What the confirmation guard does before a live write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuardAction {
    Proceed,
    /// Nobody to ask: exit 6.
    Refuse,
    Ask,
}

/// `--force` proceeds; `--json` or no TTY refuses, even when a TTY is
/// attached to a `--json` run (a machine caller must not get a prompt it
/// cannot see); otherwise ask.
fn guard_action(force: bool, json: bool, stdin_is_tty: bool) -> GuardAction {
    if force {
        GuardAction::Proceed
    } else if json || !stdin_is_tty {
        GuardAction::Refuse
    } else {
        GuardAction::Ask
    }
}

/// Only an explicit `y` or `yes` (any case, surrounding whitespace ignored)
/// confirms. An empty line, EOF and everything else abort.
fn confirmed(answer: &str) -> bool {
    matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}

/// The mutation confirmation guard (SPEC v1 §mutations). Exits 6 when it cannot
/// ask; prompts otherwise.
fn guard(ctx: &Ctx, force: bool, question: &str) -> Result<(), CliError> {
    match guard_action(force, ctx.common.json, std::io::stdin().is_terminal()) {
        GuardAction::Proceed => return Ok(()),
        GuardAction::Refuse => {
            return Err(CliError::ConfirmationRequired(
                "this changes LIVE account state and needs confirmation — re-run with \
                 `--force`, or interactively without `--json`"
                    .into(),
            ))
        }
        GuardAction::Ask => {}
    }
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| CliError::Other(format!("reading confirmation: {e}")))?;
    if confirmed(&line) {
        Ok(())
    } else {
        Err(CliError::Other("aborted".into()))
    }
}

/// Print the exact JSON-RPC request that a live run would send, and stop.
fn emit_dry_run(ctx: &Ctx, schema: &str, tool: &str, args: Value) -> Result<(), CliError> {
    let req = mcp::request(1, "tools/call", mcp::tools_call_params(tool, args));
    output::emit(
        ctx.common.json,
        schema,
        json!({ "tool": tool, "would_send": false, "request": req }),
        |v| {
            eprintln!(
                "dry run — the following JSON-RPC request would be sent to the MCP endpoint:"
            );
            output::json(v.get("request").unwrap_or(v));
        },
    );
    Ok(())
}

fn emit_result(ctx: &Ctx, schema: &str, tool: &str, result: Value) {
    output::emit(
        ctx.common.json,
        schema,
        json!({ "tool": tool, "result": result }),
        |v| output::render(v.get("result").unwrap_or(v)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_parsing_is_case_insensitive_and_validated() {
        assert_eq!(Side::parse("BUY").unwrap(), Side::Buy);
        assert_eq!(Side::parse("sell").unwrap(), Side::Sell);
        assert_eq!(Side::parse("hold").unwrap_err().exit_code(), 2);
        assert_eq!(Side::Buy.wire(), "buy");
    }

    #[test]
    fn tif_maps_day_to_gfd_and_rejects_junk() {
        assert_eq!(tif_wire("day").unwrap(), "gfd");
        assert_eq!(tif_wire("GTC").unwrap(), "gtc");
        assert_eq!(tif_wire("ioc").unwrap(), "ioc");
        assert_eq!(tif_wire("week").unwrap_err().exit_code(), 2);
    }

    #[test]
    fn decimals_must_be_positive_and_stay_strings() {
        assert_eq!(positive_decimal("--qty", "1.5").unwrap(), "1.5");
        assert_eq!(positive_decimal("--limit", " 10.00 ").unwrap(), "10.00");
        assert!(positive_decimal("--qty", "0").is_err());
        assert!(positive_decimal("--qty", "-1").is_err());
        assert!(positive_decimal("--qty", "abc").is_err());
    }

    /// Audit 2026-09-23 security #6: `parse::<f64>()` accepted all of these.
    #[test]
    fn decimals_accept_only_plain_digits() {
        for bad in [
            "inf", "Infinity", "+inf", "NaN", "nan", "1e3", "1E3", "1e999", "5e-1", "0x10", "+1",
            "1.", ".5", "1.2.3", "1_000", "1,000", "", " ", "0.000", "00", "１",
        ] {
            let e = positive_decimal("--qty", bad).unwrap_err();
            assert_eq!(e.exit_code(), 2, "`{bad}` must be a usage error");
        }
        for good in ["1", "0.5", "10.00", "007", "0.968395"] {
            assert_eq!(positive_decimal("--qty", good).unwrap(), good);
        }
    }

    #[test]
    fn integers_are_whole_and_positive() {
        assert_eq!(positive_integer("--quantity", " 3 ").unwrap(), "3");
        for bad in ["0", "000", "1.0", "1.5", "1e1", "-1", "inf", "", "one"] {
            assert!(positive_integer("--quantity", bad).is_err(), "`{bad}`");
        }
    }

    #[test]
    fn market_hours_are_validated() {
        assert_eq!(market_hours_wire("regular_hours").unwrap(), "regular_hours");
        assert_eq!(
            market_hours_wire("EXTENDED_HOURS").unwrap(),
            "extended_hours"
        );
        assert_eq!(market_hours_wire("all_day_hours").unwrap(), "all_day_hours");
        for bad in ["", "regular", "overnight", "regular_curb_hours"] {
            assert_eq!(
                market_hours_wire(bad).unwrap_err().exit_code(),
                2,
                "`{bad}`"
            );
        }
    }

    fn vp(
        qty: Option<&str>,
        dollars: Option<&str>,
        kind: &str,
        limit: Option<&str>,
        hours: &str,
    ) -> Result<EquityTerms, CliError> {
        validate_place("buy", qty, dollars, kind, limit, hours, "day")
    }

    #[test]
    fn fractional_quantities_are_market_and_regular_hours_only() {
        let t = vp(Some("0.5"), None, "market", None, "regular_hours").unwrap();
        assert_eq!(t.quantity.as_deref(), Some("0.5"));
        assert_eq!(t.market_hours, "regular_hours");
        // Fractional on a limit order.
        assert!(vp(Some("0.5"), None, "limit", Some("1.00"), "regular_hours").is_err());
        // Fractional outside regular hours, on either order type.
        for hours in ["extended_hours", "all_day_hours"] {
            let e = vp(Some("0.5"), None, "market", None, hours).unwrap_err();
            assert!(e.to_string().contains("regular_hours only"), "{e}");
            assert!(vp(Some("0.5"), None, "limit", Some("1.00"), hours).is_err());
        }
        // Exponent notation cannot pass as a whole number.
        assert!(vp(Some("5e-1"), None, "limit", Some("1.00"), "regular_hours").is_err());
        assert!(vp(Some("1e3"), None, "limit", Some("1.00"), "regular_hours").is_err());
    }

    #[test]
    fn market_orders_are_regular_hours_only_and_limits_run_any_session() {
        assert!(vp(Some("1"), None, "market", None, "extended_hours").is_err());
        assert!(vp(None, Some("100"), "market", None, "all_day_hours").is_err());
        let t = vp(Some("2"), None, "limit", Some("1.00"), "extended_hours").unwrap();
        assert_eq!(t.market_hours, "extended_hours");
        assert!(vp(Some("2"), None, "limit", Some("1.00"), "overnight").is_err());
    }

    #[test]
    fn validate_place_checks_sizing_type_and_price() {
        assert!(vp(Some("1"), Some("100"), "market", None, "regular_hours").is_err());
        assert!(vp(None, None, "market", None, "regular_hours").is_err());
        assert!(vp(None, Some("100"), "limit", Some("1.00"), "regular_hours").is_err());
        assert!(vp(Some("1"), None, "limit", None, "regular_hours").is_err());
        assert!(vp(Some("1"), None, "market", Some("1.00"), "regular_hours").is_err());
        assert!(vp(Some("1"), None, "stop", None, "regular_hours").is_err());
        assert!(vp(Some("1"), None, "limit", Some("inf"), "regular_hours").is_err());
        assert!(vp(None, Some("Infinity"), "market", None, "regular_hours").is_err());
        let t = vp(Some("2"), None, "LIMIT", Some("10.00"), "regular_hours").unwrap();
        assert_eq!(
            t,
            EquityTerms {
                side: Side::Buy,
                tif: "gfd".into(),
                order_type: "limit",
                quantity: Some("2".into()),
                dollar_amount: None,
                limit_price: Some("10.00".into()),
                market_hours: "regular_hours",
            }
        );
    }

    #[test]
    fn only_an_explicit_yes_confirms() {
        for yes in ["y", "Y", "yes", "YES", " yes \n", "y\n"] {
            assert!(confirmed(yes), "`{yes:?}` should confirm");
        }
        for no in [
            "",
            "\n",
            "n",
            "no",
            "ye",
            "yess",
            "yes please",
            "sure",
            "1",
            "true",
            "ok",
        ] {
            assert!(!confirmed(no), "`{no:?}` must abort");
        }
    }

    #[test]
    fn the_guard_never_prompts_a_json_caller() {
        assert_eq!(guard_action(false, true, true), GuardAction::Refuse);
        assert_eq!(guard_action(false, true, false), GuardAction::Refuse);
        assert_eq!(guard_action(false, false, false), GuardAction::Refuse);
        assert_eq!(guard_action(false, false, true), GuardAction::Ask);
        assert_eq!(guard_action(true, true, false), GuardAction::Proceed);
    }

    #[test]
    fn a_blank_cancel_id_is_a_usage_error() {
        for blank in ["", "   ", "\t\n"] {
            assert_eq!(require_order_id(blank).unwrap_err().exit_code(), 2);
        }
        assert_eq!(require_order_id("  ORDER-1 ").unwrap(), "ORDER-1");
    }

    #[test]
    fn place_args_carry_the_order_terms_as_strings() {
        let input = PlaceInput {
            account: "SAMPLE01",
            symbol: "SAMPL",
            side: Side::Buy,
            quantity: Some("2"),
            dollar_amount: None,
            order_type: "limit",
            limit_price: Some("10.00"),
            market_hours: "regular_hours",
            tif: "gfd",
        };
        let a = place_args(&input);
        assert_eq!(a["account_number"], "SAMPLE01");
        assert_eq!(a["symbol"], "SAMPL");
        assert_eq!(a["side"], "buy");
        // VERIFIED 2026-08-27: the field is `type`, not `order_type`.
        assert_eq!(a["type"], "limit");
        assert!(a.get("order_type").is_none(), "the live field is `type`");
        assert_eq!(a["quantity"], "2"); // string, never a JSON number
        assert!(a["quantity"].is_string());
        assert!(a["limit_price"].is_string());
        assert_eq!(a["time_in_force"], "gfd");
        // VERIFIED 2026-08-27: there is no `extended_hours` field.
        assert!(a.get("extended_hours").is_none());
    }

    /// 2026-09-18: `place` built `type: "limit"` unconditionally, so a
    /// fractional position could not be exited through rhap at all. The
    /// broker rejects fractional-on-limit, and only after the review
    /// round-trip, so the venture's core liquidation fell back to the raw
    /// `call` passthrough — no validation, no dry-run ergonomics.
    #[test]
    fn fractional_quantities_are_recognised() {
        assert!(is_fractional("0.968395"));
        assert!(is_fractional("1.5"));
        assert!(!is_fractional("2"));
        assert!(!is_fractional("2.0"));
        assert!(!is_fractional("2.000"));
        // A zero right after the point must not hide the fraction.
        assert!(is_fractional("1.05"));
        assert!(is_fractional("3.001"));
    }

    #[test]
    fn a_market_order_omits_limit_price_entirely() {
        let a = place_args(&PlaceInput {
            account: "SAMPLE01",
            symbol: "SAMPL",
            side: Side::Sell,
            quantity: Some("0.968395"),
            dollar_amount: None,
            order_type: "market",
            limit_price: None,
            market_hours: "regular_hours",
            tif: "gfd",
        });
        assert_eq!(a["type"], "market");
        // ABSENT, not null: the schema rejects a null limit_price.
        assert!(a.get("limit_price").is_none());
        assert_eq!(a["quantity"], "0.968395");
        assert_eq!(a["market_hours"], "regular_hours");
    }

    #[test]
    fn a_dollar_order_carries_no_share_count() {
        let a = place_args(&PlaceInput {
            account: "SAMPLE01",
            symbol: "SAMPL",
            side: Side::Buy,
            quantity: None,
            dollar_amount: Some("250.00"),
            order_type: "market",
            limit_price: None,
            market_hours: "regular_hours",
            tif: "gfd",
        });
        assert_eq!(a["dollar_amount"], "250.00");
        assert!(
            a.get("quantity").is_none(),
            "both sizing fields together is a schema error"
        );
    }

    #[test]
    fn cancel_args_name_the_account_and_the_order() {
        let a = cancel_args("SAMPLE01", "SAMPLE-ORDER-ID");
        assert_eq!(a["account_number"], "SAMPLE01");
        assert_eq!(a["order_id"], "SAMPLE-ORDER-ID");
    }

    /// The dry-run request is a well-formed JSON-RPC `tools/call` for the
    /// mutating tool — the shape the guard exists to gate.
    #[test]
    fn a_dry_run_request_is_a_tools_call_for_the_write_tool() {
        let args = place_args(&PlaceInput {
            account: "SAMPLE01",
            symbol: "SAMPL",
            side: Side::Sell,
            quantity: Some("1"),
            dollar_amount: None,
            order_type: "limit",
            limit_price: Some("5.00"),
            market_hours: "regular_hours",
            tif: "gfd",
        });
        let req = mcp::request(
            1,
            "tools/call",
            mcp::tools_call_params("place_equity_order", args),
        );
        assert_eq!(req["method"], "tools/call");
        assert_eq!(req["params"]["name"], "place_equity_order");
        assert!(crate::catalog::is_mutating(
            req["params"]["name"].as_str().unwrap()
        ));
    }
}
