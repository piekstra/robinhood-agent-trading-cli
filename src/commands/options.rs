//! The `option` command group: reads (positions, orders, quotes, chains) and
//! writes (order place, order cancel) over the MCP option tools.
//!
//! Schemas are VERIFIED against the live `tools/list` (2026-08-27) for the
//! **top-level** tool arguments. The one exception is the per-leg sub-schema of
//! `place_option_order`, which the introspection did not spell out: the
//! single-leg convenience builder here is a best guess, and `--legs <JSON>` is
//! the raw passthrough for full control (see `docs/api.md §6`).
//!
//! Every account-scoped option command takes a required, per-invocation
//! `--account` (never ambient — see `config.rs`). Writes reuse the equity
//! [`super::order::run_write`] guard chain: `--dry-run` prints the request and
//! sends nothing; a live run confirms (exit 6 non-interactively without
//! `--force`) and reviews before placing.

use pk_cli_core::CliError;
use serde_json::{json, Map, Value};

use super::args::{run_read, Args};
use super::order::{self, positive_decimal, positive_integer, tif_wire, Side};
use super::reads::emit_read;
use super::Ctx;

// ------------------------------------------------------------------ reads

/// `option positions list --account <ID>` → `get_option_positions`.
pub fn positions_list(ctx: &Ctx, account: &str, nonzero: bool) -> Result<(), CliError> {
    let result = ctx.client()?.read(
        "get_option_positions",
        option_positions_args(account, nonzero),
    )?;
    emit_read(
        ctx.common.json,
        "option-positions",
        "get_option_positions",
        vec![],
        result,
    );
    Ok(())
}

/// `option orders list --account <ID>` → `get_option_orders`.
pub fn orders_list(
    ctx: &Ctx,
    account: &str,
    since: Option<&str>,
    state: Option<&str>,
    placed_agent: Option<&str>,
) -> Result<(), CliError> {
    let args = option_orders_args(account, since, state, placed_agent);
    let result = ctx.client()?.read("get_option_orders", args)?;
    emit_read(
        ctx.common.json,
        "option-orders",
        "get_option_orders",
        vec![],
        result,
    );
    Ok(())
}

/// `option quotes get --instrument-ids <ID[,ID...]>` → `get_option_quotes`.
pub fn quotes_get(ctx: &Ctx, instrument_ids: &str) -> Result<(), CliError> {
    let ids = parse_ids(instrument_ids)?;
    let result = ctx
        .client()?
        .read("get_option_quotes", option_quotes_args(&ids))?;
    emit_read(
        ctx.common.json,
        "option-quote",
        "get_option_quotes",
        vec![("instrument_ids", json!(ids))],
        result,
    );
    Ok(())
}

/// `option chains get --underlying <SYM>` → `get_option_chains`.
pub fn chains_get(ctx: &Ctx, underlying: &str) -> Result<(), CliError> {
    let underlying = underlying.trim().to_uppercase();
    let result = ctx
        .client()?
        .read("get_option_chains", option_chains_args(Some(&underlying)))?;
    emit_read(
        ctx.common.json,
        "option-chains",
        "get_option_chains",
        vec![("underlying_symbol", Value::String(underlying))],
        result,
    );
    Ok(())
}

// ----------------------------------------------------------------- writes

/// The validated inputs for an option place (separated from clap so the leg and
/// argument builders are unit-testable).
pub struct OptionPlaceInput<'a> {
    pub account: &'a str,
    pub quantity: &'a str,
    /// The legs array — either the single-leg convenience or a `--legs` blob.
    pub legs: Value,
    pub direction: Option<&'a str>,
    pub price: Option<&'a str>,
    pub tif: Option<&'a str>,
    pub kind: Option<&'a str>,
}

/// Option order terms after validation.
#[derive(Debug)]
pub struct OptionTerms {
    /// A whole number of contracts (the schema: "positive integer contract
    /// count").
    pub quantity: String,
    pub legs: Value,
    pub direction: Option<String>,
    pub price: Option<String>,
    pub tif: Option<String>,
    pub kind: Option<String>,
}

/// Validate every `option order place` term from the arguments alone — the
/// single path shared by `main.rs` and [`order_place`].
#[allow(clippy::too_many_arguments)]
pub fn validate_place(
    quantity: &str,
    legs_json: Option<&str>,
    instrument_id: Option<&str>,
    position_effect: Option<&str>,
    side: Option<&str>,
    ratio: &str,
    direction: Option<&str>,
    price: Option<&str>,
    tif: Option<&str>,
    kind: Option<&str>,
) -> Result<OptionTerms, CliError> {
    Ok(OptionTerms {
        quantity: positive_integer("--quantity", quantity)?,
        legs: build_legs(legs_json, instrument_id, position_effect, side, ratio)?,
        direction: direction.map(direction_wire).transpose()?,
        price: price.map(|p| positive_decimal("--price", p)).transpose()?,
        tif: tif.map(tif_wire).transpose()?,
        kind: kind.map(order_kind_wire).transpose()?,
    })
}

/// `option order place --account <ID> --quantity <N> …`.
#[allow(clippy::too_many_arguments)]
pub fn order_place(
    ctx: &Ctx,
    account: &str,
    quantity: &str,
    legs_json: Option<&str>,
    instrument_id: Option<&str>,
    position_effect: Option<&str>,
    side: Option<&str>,
    ratio: &str,
    direction: Option<&str>,
    price: Option<&str>,
    tif: Option<&str>,
    kind: Option<&str>,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    // Validate from the arguments alone, before any keychain or network — the
    // same function `main.rs` runs before config load.
    let OptionTerms {
        quantity,
        legs,
        direction,
        price,
        tif,
        kind,
    } = validate_place(
        quantity,
        legs_json,
        instrument_id,
        position_effect,
        side,
        ratio,
        direction,
        price,
        tif,
        kind,
    )?;

    let input = OptionPlaceInput {
        account,
        quantity: &quantity,
        legs,
        direction: direction.as_deref(),
        price: price.as_deref(),
        tif: tif.as_deref(),
        kind: kind.as_deref(),
    };
    let args = place_option_args(&input);
    let leg_count = input.legs.as_array().map(Vec::len).unwrap_or(0);
    let summary = format!(
        "{quantity} contract(s) across {leg_count} leg(s){} on account {account}",
        input.price.map(|p| format!(" @ {p}")).unwrap_or_default()
    );
    order::run_write(
        ctx,
        "option-order-dry-run",
        "option-order-place",
        "place_option_order",
        args,
        dry_run,
        force,
        &format!("Place a LIVE option order: {summary}?"),
    )
}

/// `option order cancel <ORDER_ID> --account <ID>`.
pub fn order_cancel(
    ctx: &Ctx,
    account: &str,
    order_id: &str,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    let order_id = order::require_order_id(order_id)?;
    let args = cancel_option_args(account, order_id);
    order::run_write(
        ctx,
        "option-order-dry-run",
        "option-order-cancel",
        "cancel_option_order",
        args,
        dry_run,
        force,
        &format!("Cancel LIVE option order {order_id} on account {account}?"),
    )
}

// ------------------------------------------------------- argument builders

/// `get_option_positions` args (schema VERIFIED 2026-08-27: required
/// [account_number]; optional nonzero/…). Defaults `nonzero` true so a bare call
/// returns open positions.
pub fn option_positions_args(account: &str, nonzero: bool) -> Value {
    json!({ "account_number": account, "nonzero": nonzero })
}

/// `get_option_orders` args (schema VERIFIED 2026-08-27: required
/// [account_number]; optional created_at_gte/state/placed_agent/…).
pub fn option_orders_args(
    account: &str,
    since: Option<&str>,
    state: Option<&str>,
    placed_agent: Option<&str>,
) -> Value {
    let mut m = Map::new();
    m.insert("account_number".into(), Value::String(account.into()));
    if let Some(s) = since {
        m.insert("created_at_gte".into(), Value::String(s.into()));
    }
    if let Some(s) = state {
        m.insert("state".into(), Value::String(s.into()));
    }
    if let Some(a) = placed_agent {
        m.insert("placed_agent".into(), Value::String(a.into()));
    }
    Value::Object(m)
}

/// `get_option_quotes` args (schema VERIFIED 2026-08-27: required
/// [instrument_ids]). Option quotes key on instrument ids, not symbols.
pub fn option_quotes_args(instrument_ids: &[String]) -> Value {
    json!({ "instrument_ids": instrument_ids })
}

/// `get_option_chains` args (schema VERIFIED 2026-08-27: optional
/// [underlying_symbol, ids]).
pub fn option_chains_args(underlying: Option<&str>) -> Value {
    match underlying {
        Some(u) => json!({ "underlying_symbol": u }),
        None => json!({}),
    }
}

/// `cancel_option_order` args (schema VERIFIED 2026-08-27: [account_number,
/// order_id]).
pub fn cancel_option_args(account: &str, order_id: &str) -> Value {
    json!({ "account_number": account, "order_id": order_id })
}

/// `place_option_order` args. Top-level schema VERIFIED 2026-08-27 (required
/// [account_number, legs, quantity]; optional
/// direction/price/time_in_force/type/…). The **leg** sub-schema is UNVERIFIED —
/// see [`single_leg`].
pub fn place_option_args(input: &OptionPlaceInput) -> Value {
    let mut m = Map::new();
    m.insert("account_number".into(), Value::String(input.account.into()));
    m.insert("legs".into(), input.legs.clone());
    m.insert("quantity".into(), Value::String(input.quantity.into()));
    if let Some(d) = input.direction {
        m.insert("direction".into(), Value::String(d.into()));
    }
    if let Some(p) = input.price {
        m.insert("price".into(), Value::String(p.into()));
    }
    if let Some(t) = input.tif {
        m.insert("time_in_force".into(), Value::String(t.into()));
    }
    if let Some(k) = input.kind {
        m.insert("type".into(), Value::String(k.into()));
    }
    Value::Object(m)
}

/// Build the legs array: a raw `--legs <JSON>` passthrough, or the single-leg
/// convenience (`--instrument-id` + `--position-effect` + `--side` [+ `--ratio`]).
/// The two are mutually exclusive.
pub fn build_legs(
    legs_json: Option<&str>,
    instrument_id: Option<&str>,
    position_effect: Option<&str>,
    side: Option<&str>,
    ratio: &str,
) -> Result<Value, CliError> {
    match (legs_json, instrument_id) {
        (Some(_), Some(_)) => Err(CliError::Usage(
            "pass either --legs <JSON> or the single-leg flags (--instrument-id …), not both"
                .into(),
        )),
        (Some(raw), None) => {
            let v: Value = serde_json::from_str(raw)
                .map_err(|e| CliError::Usage(format!("--legs is not valid JSON: {e}")))?;
            match v.as_array() {
                Some(a) if !a.is_empty() => Ok(v),
                _ => Err(CliError::Usage(
                    "--legs must be a non-empty JSON array".into(),
                )),
            }
        }
        (None, Some(id)) => {
            let effect = position_effect.ok_or_else(|| {
                CliError::Usage(
                    "--position-effect (open|close) is required with --instrument-id".into(),
                )
            })?;
            let side = side.ok_or_else(|| {
                CliError::Usage("--side (buy|sell) is required with --instrument-id".into())
            })?;
            let effect = position_effect_wire(effect)?;
            let side = Side::parse(side)?;
            let ratio = leg_ratio("--ratio", ratio)?;
            Ok(json!([single_leg(id, &effect, side.wire(), ratio)]))
        }
        (None, None) => Err(CliError::Usage(
            "which legs? pass --legs <JSON>, or the single-leg flags: --instrument-id <ID> \
             --position-effect open|close --side buy|sell"
                .into(),
        )),
    }
}

/// One option leg, as `place_option_order`'s schema spells it (VERIFIED
/// 2026-09-02 via `tools show place_option_order`: `option_id` string, `side`
/// string, `position_effect` string, `ratio_quantity` INTEGER). A live order
/// was refused with `ratio_quantity: 1 has type "string", want "integer"`
/// while this sent the ratio as text under an `instrument_id` key the schema
/// does not name.
pub fn single_leg(option_id: &str, position_effect: &str, side: &str, ratio: u64) -> Value {
    json!({
        "option_id": option_id,
        "position_effect": position_effect,
        "side": side,
        "ratio_quantity": ratio,
    })
}

/// A leg ratio is a whole number of contracts per unit of the order.
fn leg_ratio(flag: &str, raw: &str) -> Result<u64, CliError> {
    match raw.trim().parse::<u64>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(CliError::Usage(format!(
            "{flag} must be a positive whole number (got {raw:?})"
        ))),
    }
}

// ------------------------------------------------------- more option reads

/// `option instruments [--chain-symbol --chain-id --expiration-dates
/// --strike-price --type --state --tradability --ids --cursor]` →
/// `get_option_instruments` (all fields optional; schema VERIFIED 2026-08-27).
#[allow(clippy::too_many_arguments)]
pub fn instruments(
    ctx: &Ctx,
    chain_symbol: Option<&str>,
    chain_id: Option<&str>,
    expiration_dates: Option<&str>,
    strike_price: Option<&str>,
    kind: Option<&str>,
    state: Option<&str>,
    tradability: Option<&str>,
    ids: Option<&str>,
    cursor: Option<&str>,
) -> Result<(), CliError> {
    let args = Args::new()
        .opt("chain_symbol", chain_symbol)
        .opt("chain_id", chain_id)
        .opt("expiration_dates", expiration_dates)
        .opt("strike_price", strike_price)
        .opt("type", kind)
        .opt("state", state)
        .opt("tradability", tradability)
        .opt("ids", ids)
        .opt("cursor", cursor)
        .build();
    run_read(ctx, "option-instruments", "get_option_instruments", args)
}

/// `option historicals --instrument-ids --start-time [--end-time --interval
/// --bounds]` → `get_option_historicals`.
pub fn historicals(
    ctx: &Ctx,
    instrument_ids: &str,
    start_time: &str,
    end_time: Option<&str>,
    interval: Option<&str>,
    bounds: Option<&str>,
) -> Result<(), CliError> {
    let args = Args::new()
        .csv("instrument_ids", instrument_ids)
        .str("start_time", start_time)
        .opt("end_time", end_time)
        .opt("interval", interval)
        .opt("bounds", bounds)
        .build();
    run_read(ctx, "option-historicals", "get_option_historicals", args)
}

/// `option watchlist` → `get_option_watchlist`.
pub fn watchlist(ctx: &Ctx) -> Result<(), CliError> {
    run_read(
        ctx,
        "option-watchlist",
        "get_option_watchlist",
        Args::new().build(),
    )
}

// ---------------------------------------------------- option exercise (WRITE)

/// `option exercise --account --option-id --quantity [--ref-id --reason
/// --allow-shorts]` → `exercise_option` (account-scoped WRITE).
#[allow(clippy::too_many_arguments)]
pub fn exercise(
    ctx: &Ctx,
    account: &str,
    option_id: &str,
    quantity: i64,
    ref_id: Option<&str>,
    reason: Option<&str>,
    allow_shorts: bool,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    if quantity <= 0 {
        return Err(CliError::Usage(
            "--quantity must be a positive integer".into(),
        ));
    }
    let args = Args::new()
        .str("account_number", account)
        .str("option_id", option_id)
        .opt_int("quantity", Some(quantity))
        .opt("ref_id", ref_id)
        .opt("reason", reason)
        .opt_bool("allow_shorts", allow_shorts.then_some(true))
        .build();
    order::run_write(
        ctx,
        "option-exercise-dry-run",
        "option-exercise",
        "exercise_option",
        args,
        dry_run,
        force,
        &format!("Exercise {quantity} contract(s) of {option_id} on account {account}?"),
    )
}

/// `option exercise-cancel <option_id> --account` → `cancel_option_exercise`
/// (account-scoped WRITE).
pub fn exercise_cancel(
    ctx: &Ctx,
    account: &str,
    option_id: &str,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    let args = Args::new()
        .str("account_number", account)
        .str("option_id", option_id)
        .build();
    order::run_write(
        ctx,
        "option-exercise-cancel-dry-run",
        "option-exercise-cancel",
        "cancel_option_exercise",
        args,
        dry_run,
        force,
        &format!("Cancel the exercise of {option_id} on account {account}?"),
    )
}

// --------------------------------------------------------------- validators

/// Parse a comma-separated id list into a non-empty vec.
pub fn parse_ids(raw: &str) -> Result<Vec<String>, CliError> {
    let ids: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if ids.is_empty() {
        return Err(CliError::Usage(
            "--instrument-ids needs at least one id".into(),
        ));
    }
    Ok(ids)
}

/// Validate an option order direction (credit/debit).
pub fn direction_wire(raw: &str) -> Result<String, CliError> {
    match raw.trim().to_lowercase().as_str() {
        "credit" => Ok("credit".into()),
        "debit" => Ok("debit".into()),
        other => Err(CliError::Usage(format!(
            "--direction must be `credit` or `debit` (got `{other}`)"
        ))),
    }
}

/// Validate an option order type (limit/market).
pub fn order_kind_wire(raw: &str) -> Result<String, CliError> {
    match raw.trim().to_lowercase().as_str() {
        "limit" => Ok("limit".into()),
        "market" => Ok("market".into()),
        other => Err(CliError::Usage(format!(
            "--type must be `limit` or `market` (got `{other}`)"
        ))),
    }
}

/// Validate a leg's position effect (open/close).
pub fn position_effect_wire(raw: &str) -> Result<String, CliError> {
    match raw.trim().to_lowercase().as_str() {
        "open" => Ok("open".into()),
        "close" => Ok("close".into()),
        other => Err(CliError::Usage(format!(
            "--position-effect must be `open` or `close` (got `{other}`)"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vq(quantity: &str, price: Option<&str>) -> Result<OptionTerms, CliError> {
        validate_place(
            quantity,
            None,
            Some("INST-1"),
            Some("open"),
            Some("buy"),
            "1",
            None,
            price,
            None,
            Some("limit"),
        )
    }

    /// Audit 2026-09-23 security #6: a contract count is a whole number.
    #[test]
    fn option_quantities_are_whole_contracts() {
        assert_eq!(vq("2", Some("1.00")).unwrap().quantity, "2");
        for bad in ["1.5", "1.0", "0", "1e1", "inf", "-1", ""] {
            assert_eq!(vq(bad, Some("1.00")).unwrap_err().exit_code(), 2, "`{bad}`");
        }
        for bad in ["inf", "1e2", "NaN", "0"] {
            assert!(vq("1", Some(bad)).is_err(), "price `{bad}`");
        }
    }

    #[test]
    fn positions_args_default_nonzero_and_carry_the_account() {
        assert_eq!(
            option_positions_args("SAMPLE01", true),
            json!({"account_number": "SAMPLE01", "nonzero": true})
        );
        assert_eq!(option_positions_args("SAMPLE01", false)["nonzero"], false);
    }

    #[test]
    fn orders_args_map_since_to_created_at_gte_and_forward_state() {
        let a = option_orders_args(
            "SAMPLE01",
            Some("2026-01-01"),
            Some("filled"),
            Some("agentic"),
        );
        assert_eq!(a["account_number"], "SAMPLE01");
        assert_eq!(a["created_at_gte"], "2026-01-01");
        assert_eq!(a["state"], "filled");
        assert_eq!(a["placed_agent"], "agentic");
        // A bare call carries only the required account.
        assert_eq!(
            option_orders_args("SAMPLE01", None, None, None),
            json!({"account_number": "SAMPLE01"})
        );
    }

    #[test]
    fn quote_args_key_on_instrument_ids_not_symbols() {
        let ids = parse_ids("id-1, id-2 ,id-3").unwrap();
        assert_eq!(ids, vec!["id-1", "id-2", "id-3"]);
        assert_eq!(
            option_quotes_args(&ids),
            json!({"instrument_ids": ["id-1", "id-2", "id-3"]})
        );
        assert!(parse_ids("  , ").is_err());
    }

    #[test]
    fn chains_args_are_optional() {
        assert_eq!(
            option_chains_args(Some("AAPL")),
            json!({"underlying_symbol": "AAPL"})
        );
        assert_eq!(option_chains_args(None), json!({}));
    }

    #[test]
    fn cancel_args_carry_account_and_order() {
        let a = cancel_option_args("SAMPLE01", "OID");
        assert_eq!(a["account_number"], "SAMPLE01");
        assert_eq!(a["order_id"], "OID");
    }

    #[test]
    fn single_leg_convenience_builds_a_one_element_array() {
        let legs = build_legs(None, Some("INST-1"), Some("open"), Some("buy"), "1").unwrap();
        let arr = legs.as_array().expect("array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["option_id"], "INST-1");
        assert_eq!(arr[0]["position_effect"], "open");
        assert_eq!(arr[0]["side"], "buy");
        assert_eq!(arr[0]["ratio_quantity"], 1, "the schema wants an integer");
        assert!(
            arr[0].get("instrument_id").is_none(),
            "the schema names the leg by option_id"
        );
        assert!(build_legs(None, Some("INST-1"), Some("open"), Some("buy"), "1.5").is_err());
    }

    #[test]
    fn legs_passthrough_accepts_a_json_array() {
        let raw = r#"[{"instrument_id":"A","side":"buy"},{"instrument_id":"B","side":"sell"}]"#;
        let legs = build_legs(Some(raw), None, None, None, "1").unwrap();
        assert_eq!(legs.as_array().unwrap().len(), 2);
    }

    #[test]
    fn legs_are_required_and_mutually_exclusive() {
        // Neither form.
        assert_eq!(
            build_legs(None, None, None, None, "1")
                .unwrap_err()
                .exit_code(),
            2
        );
        // Both forms.
        assert_eq!(
            build_legs(Some("[]"), Some("INST-1"), None, None, "1")
                .unwrap_err()
                .exit_code(),
            2
        );
        // Passthrough must be a non-empty array.
        assert!(build_legs(Some("[]"), None, None, None, "1").is_err());
        assert!(build_legs(Some("{}"), None, None, None, "1").is_err());
        // Single-leg needs effect + side.
        assert!(build_legs(None, Some("INST-1"), None, Some("buy"), "1").is_err());
        assert!(build_legs(None, Some("INST-1"), Some("open"), None, "1").is_err());
    }

    #[test]
    fn place_args_carry_the_required_and_optional_fields() {
        let legs = build_legs(None, Some("INST-1"), Some("open"), Some("buy"), "1").unwrap();
        let input = OptionPlaceInput {
            account: "SAMPLE01",
            quantity: "2",
            legs,
            direction: Some("debit"),
            price: Some("1.25"),
            tif: Some("gfd"),
            kind: Some("limit"),
        };
        let a = place_option_args(&input);
        assert_eq!(a["account_number"], "SAMPLE01");
        assert_eq!(a["quantity"], "2");
        assert!(a["quantity"].is_string());
        assert!(a["legs"].is_array());
        assert_eq!(a["direction"], "debit");
        assert_eq!(a["price"], "1.25");
        assert_eq!(a["time_in_force"], "gfd");
        assert_eq!(a["type"], "limit");

        // Optionals are omitted, not nulled, when absent.
        let minimal = place_option_args(&OptionPlaceInput {
            account: "SAMPLE01",
            quantity: "1",
            legs: json!([{"instrument_id": "X"}]),
            direction: None,
            price: None,
            tif: None,
            kind: None,
        });
        assert!(minimal.get("direction").is_none());
        assert!(minimal.get("price").is_none());
        assert!(minimal.get("type").is_none());
    }

    #[test]
    fn the_option_validators_reject_junk() {
        assert_eq!(direction_wire("credit").unwrap(), "credit");
        assert!(direction_wire("sideways").is_err());
        assert_eq!(order_kind_wire("MARKET").unwrap(), "market");
        assert!(order_kind_wire("stop").is_err());
        assert_eq!(position_effect_wire("Close").unwrap(), "close");
        assert!(position_effect_wire("roll").is_err());
    }
}
