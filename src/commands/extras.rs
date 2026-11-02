//! The 2026-09-17 surface additions: news, SEC filings, alerts, OCO advanced
//! orders, politician trades, and crypto.
//!
//! The embedded catalog was re-captured on 2026-09-17 and the live surface had
//! grown from 54 tools to 78. Every new tool gets a typed command here, so
//! `catalog::tests::every_catalog_tool_has_a_typed_command` stays true and no
//! capability is reachable only through the generic `call` escape hatch.
//!
//! Writes (`alerts create|update|delete|mark-read`, `order advanced place|
//! cancel`, `crypto order place|cancel`) go through [`super::order::run_write`]:
//! `--dry-run` prints the JSON-RPC request and sends nothing, and a live run is
//! confirmed (exit 6 non-interactively without `--force`). `order advanced
//! place` reviews first, like every other order path.
//!
//! Schemas VERIFIED against the live `tools/list` captured 2026-09-17.

use pk_cli_core::CliError;
use serde_json::Value;

use super::args::{run_read, Args};
use super::order::run_write;
use super::Ctx;

// ------------------------------------------------------------------- news

/// `news get --symbol <SYM> [--limit N --cursor C]` → `get_equity_news`.
pub fn news(
    ctx: &Ctx,
    symbol: &str,
    limit: Option<i64>,
    cursor: Option<&str>,
) -> Result<(), CliError> {
    let args = Args::new()
        .str("symbol", symbol.trim().to_uppercase())
        .opt_int("limit", limit)
        .opt("cursor", cursor)
        .build();
    run_read(ctx, "equity-news", "get_equity_news", args)
}

// ------------------------------------------------------------- SEC filings

/// `sec index --symbol [--form-type --since --until --cursor]` →
/// `get_sec_filing_index`.
pub fn sec_index(
    ctx: &Ctx,
    symbol: &str,
    form_type: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
    cursor: Option<&str>,
) -> Result<(), CliError> {
    let args = Args::new()
        .str("symbol", symbol.trim().to_uppercase())
        .opt_csv("form_type", form_type)
        .opt("since", since)
        .opt("until", until)
        .opt("cursor", cursor)
        .build();
    run_read(ctx, "sec-filing-index", "get_sec_filing_index", args)
}

/// `sec filing --filing-id [--section]` → `get_sec_filing`.
pub fn sec_filing(ctx: &Ctx, filing_id: &str, section: Option<&str>) -> Result<(), CliError> {
    let args = Args::new()
        .str("filing_id", filing_id)
        .opt("section", section)
        .build();
    run_read(ctx, "sec-filing", "get_sec_filing", args)
}

/// `sec facts --filing-ids --concepts` → `get_sec_filing_facts`.
pub fn sec_facts(ctx: &Ctx, filing_ids: &str, concepts: &str) -> Result<(), CliError> {
    let args = Args::new()
        .csv("filing_ids", filing_ids)
        .csv("concepts", concepts)
        .build();
    run_read(ctx, "sec-filing-facts", "get_sec_filing_facts", args)
}

/// `sec facts-catalog --filing-id [--concept-contains --axis-name-in --offset]`
/// → `get_sec_filing_facts_catalog`.
pub fn sec_facts_catalog(
    ctx: &Ctx,
    filing_id: &str,
    concept_contains: Option<&str>,
    axis_name_in: Option<&str>,
    offset: Option<i64>,
) -> Result<(), CliError> {
    let args = Args::new()
        .str("filing_id", filing_id)
        .opt("concept_contains", concept_contains)
        .opt_csv("axis_name_in", axis_name_in)
        .opt_int("offset", offset)
        .build();
    run_read(
        ctx,
        "sec-filing-facts-catalog",
        "get_sec_filing_facts_catalog",
        args,
    )
}

// ----------------------------------------------------------------- alerts

/// `alerts list [--symbol --asset-class --cursor]` → `get_alerts`.
pub fn alerts_list(
    ctx: &Ctx,
    symbol: Option<&str>,
    asset_class: Option<&str>,
    cursor: Option<&str>,
) -> Result<(), CliError> {
    let args = Args::new()
        .opt("symbol", symbol)
        .opt("asset_class", asset_class)
        .opt("cursor", cursor)
        .build();
    run_read(ctx, "alerts", "get_alerts", args)
}

/// `alerts log [--asset-class --since --cursor --limit]` → `get_alert_log`.
/// The alert stream is poll-only: nothing is pushed, so a scheduled poll of
/// this is how an alert becomes an action.
pub fn alerts_log(
    ctx: &Ctx,
    asset_class: Option<&str>,
    since: Option<&str>,
    cursor: Option<&str>,
    limit: Option<i64>,
) -> Result<(), CliError> {
    let args = Args::new()
        .opt("asset_class", asset_class)
        .opt("since", since)
        .opt("cursor", cursor)
        .opt_int("limit", limit)
        .build();
    run_read(ctx, "alert-log", "get_alert_log", args)
}

/// `alerts create --symbol --condition-type [--threshold --asset-class
/// --indicator-json]` → `create_alert` (WRITE).
#[allow(clippy::too_many_arguments)]
pub fn alerts_create(
    ctx: &Ctx,
    symbol: &str,
    condition_type: &str,
    threshold: Option<&str>,
    asset_class: Option<&str>,
    indicator_json: Option<&str>,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    let indicator = parse_object("--indicator-json", indicator_json)?;
    let args = Args::new()
        .str("symbol", symbol.trim().to_uppercase())
        .str("condition_type", condition_type)
        .opt("threshold", threshold)
        .opt("asset_class", asset_class)
        .opt_value("indicator", indicator)
        .build();
    run_write(
        ctx,
        "alert-create-dry-run",
        "alert-create",
        "create_alert",
        args,
        dry_run,
        force,
        &format!("Create a LIVE alert on {symbol} ({condition_type})?"),
    )
}

/// `alerts update --alert-id [--enabled --condition-type --threshold
/// --indicator-json]` → `update_alert` (WRITE).
#[allow(clippy::too_many_arguments)]
pub fn alerts_update(
    ctx: &Ctx,
    alert_id: &str,
    enabled: Option<bool>,
    condition_type: Option<&str>,
    threshold: Option<&str>,
    indicator_json: Option<&str>,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    let indicator = parse_object("--indicator-json", indicator_json)?;
    let args = Args::new()
        .str("alert_id", alert_id)
        .opt_bool("enabled", enabled)
        .opt("condition_type", condition_type)
        .opt("threshold", threshold)
        .opt_value("indicator", indicator)
        .build();
    run_write(
        ctx,
        "alert-update-dry-run",
        "alert-update",
        "update_alert",
        args,
        dry_run,
        force,
        &format!("Update alert {alert_id}?"),
    )
}

/// `alerts delete <ALERT_ID>` → `delete_alert` (WRITE).
pub fn alerts_delete(
    ctx: &Ctx,
    alert_id: &str,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    let args = Args::new()
        .str("alert_id", alert_id)
        .opt_bool("confirm", Some(true))
        .build();
    run_write(
        ctx,
        "alert-delete-dry-run",
        "alert-delete",
        "delete_alert",
        args,
        dry_run,
        force,
        &format!("Delete alert {alert_id}?"),
    )
}

/// `alerts mark-read [--ids --all-through]` → `mark_alerts_read` (WRITE).
pub fn alerts_mark_read(
    ctx: &Ctx,
    ids: Option<&str>,
    all_through: Option<&str>,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    if ids.is_none() && all_through.is_none() {
        return Err(CliError::Usage(
            "pass --ids <ID[,ID...]> or --all-through <TIMESTAMP>".into(),
        ));
    }
    let args = Args::new()
        .opt_csv("alert_log_ids", ids)
        .opt("all_through", all_through)
        .build();
    run_write(
        ctx,
        "alerts-mark-read-dry-run",
        "alerts-mark-read",
        "mark_alerts_read",
        args,
        dry_run,
        force,
        "Mark alert-log entries read?",
    )
}

// ------------------------------------------------- advanced (OCO) orders

/// The validated terms of an OCO bracket, shared by review and place.
pub struct OcoInput<'a> {
    pub account: &'a str,
    pub symbol: &'a str,
    pub side: &'a str,
    pub quantity: &'a str,
    pub take_profit: &'a str,
    pub stop_loss: &'a str,
    pub tif: Option<&'a str>,
}

/// Build `review_advanced_order` / `place_advanced_order` arguments. Both legs
/// share symbol, side and quantity; the stop leg is always stop-market, and
/// `regular_hours` is the only session OCO supports (schema, 2026-09-17).
pub fn advanced_args(input: &OcoInput, ref_id: Option<&str>) -> Value {
    Args::new()
        .str("account_number", input.account)
        .str("symbol", input.symbol.trim().to_uppercase())
        .str("side", input.side)
        .str("quantity", input.quantity)
        .str("take_profit_limit_price", input.take_profit)
        .str("stop_loss_stop_price", input.stop_loss)
        .opt("time_in_force", input.tif)
        .str("market_hours", "regular_hours")
        .opt("ref_id", ref_id)
        .build()
}

/// Validate the OCO terms from the arguments alone (no keychain, no network).
pub fn validate_oco(input: &OcoInput) -> Result<(), CliError> {
    super::order::Side::parse(input.side)?;
    // The schema: "Whole-share quantity".
    super::order::positive_integer("--quantity", input.quantity).map_err(|_| {
        CliError::Usage(format!(
            "--quantity must be whole shares for an OCO bracket (got `{}`)",
            input.quantity
        ))
    })?;
    let tp = super::order::positive_decimal("--take-profit", input.take_profit)?;
    let sl = super::order::positive_decimal("--stop-loss", input.stop_loss)?;
    if tp == sl {
        return Err(CliError::Usage(
            "--take-profit and --stop-loss must differ".into(),
        ));
    }
    if let Some(t) = input.tif {
        super::order::tif_wire(t)?;
    }
    Ok(())
}

/// `order advanced list --account [--order-id --contingency-type --since
/// --cursor]` → `get_advanced_orders`.
pub fn advanced_list(
    ctx: &Ctx,
    account: &str,
    order_id: Option<&str>,
    contingency_type: Option<&str>,
    since: Option<&str>,
    cursor: Option<&str>,
) -> Result<(), CliError> {
    let args = Args::new()
        .str("account_number", account)
        .opt("order_id", order_id)
        .opt("contingency_type", contingency_type)
        .opt("created_at_gte", since)
        .opt("cursor", cursor)
        .build();
    run_read(ctx, "advanced-orders", "get_advanced_orders", args)
}

/// `order advanced review …` → `review_advanced_order` (simulation, no write).
pub fn advanced_review(ctx: &Ctx, input: &OcoInput) -> Result<(), CliError> {
    validate_oco(input)?;
    run_read(
        ctx,
        "advanced-order-review",
        "review_advanced_order",
        advanced_args(input, None),
    )
}

/// `order advanced place …` → `review_advanced_order` then
/// `place_advanced_order` (WRITE).
pub fn advanced_place(
    ctx: &Ctx,
    input: &OcoInput,
    ref_id: Option<&str>,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    validate_oco(input)?;
    let question = format!(
        "Place a LIVE OCO bracket: {} {} {} — take-profit {} / stop {} on account {}?",
        input.side, input.quantity, input.symbol, input.take_profit, input.stop_loss, input.account
    );
    run_write(
        ctx,
        "advanced-order-dry-run",
        "advanced-order-place",
        "place_advanced_order",
        advanced_args(input, ref_id),
        dry_run,
        force,
        &question,
    )
}

/// `order advanced cancel <ORDER_ID> --account` → `cancel_advanced_order`
/// (WRITE).
pub fn advanced_cancel(
    ctx: &Ctx,
    account: &str,
    order_id: &str,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    let args = Args::new()
        .str("account_number", account)
        .str("order_id", order_id)
        .build();
    run_write(
        ctx,
        "advanced-order-cancel-dry-run",
        "advanced-order-cancel",
        "cancel_advanced_order",
        args,
        dry_run,
        force,
        &format!("Cancel LIVE advanced order {order_id} on account {account}?"),
    )
}

// ------------------------------------------------------ politician trades

/// `market politician-trades [--politician --symbol]` → `get_politician_trades`.
/// Public STOCK Act disclosures: amounts are ranges and filings lag the trade
/// by up to 45 days — context, never a signal on its own.
pub fn politician_trades(
    ctx: &Ctx,
    politician: Option<&str>,
    symbol: Option<&str>,
) -> Result<(), CliError> {
    if politician.is_none() && symbol.is_none() {
        return Err(CliError::Usage(
            "pass --politician <NAME> or --symbol <TICKER>".into(),
        ));
    }
    let args = Args::new()
        .opt("politician_name", politician)
        .opt(
            "equity_symbol",
            symbol.map(str::trim).map(|s| s.to_uppercase()).as_deref(),
        )
        .build();
    run_read(ctx, "politician-trades", "get_politician_trades", args)
}

// ----------------------------------------------------------------- crypto

/// `crypto onboarding` → `get_crypto_account_onboarding_info`.
pub fn crypto_onboarding(ctx: &Ctx) -> Result<(), CliError> {
    run_read(
        ctx,
        "crypto-onboarding",
        "get_crypto_account_onboarding_info",
        Args::new().build(),
    )
}

/// `crypto pairs [--cursor --limit]` → `get_currency_pairs`.
pub fn crypto_pairs(ctx: &Ctx, cursor: Option<&str>, limit: Option<i64>) -> Result<(), CliError> {
    let args = Args::new()
        .opt("cursor", cursor)
        .opt_int("limit", limit)
        .build();
    run_read(ctx, "currency-pairs", "get_currency_pairs", args)
}

/// `crypto quotes --symbols [--timezone --account]` → `get_crypto_quotes`.
pub fn crypto_quotes(
    ctx: &Ctx,
    symbols: &str,
    timezone: Option<&str>,
    account: Option<&str>,
) -> Result<(), CliError> {
    let args = Args::new()
        .csv("symbols", symbols)
        .opt("timezone", timezone)
        .opt("rhs_account_number", account)
        .build();
    run_read(ctx, "crypto-quotes", "get_crypto_quotes", args)
}

/// `crypto positions --account` → `get_crypto_positions`. The crypto tools take
/// the numeric `rhs_account_number`, not the alphanumeric account id.
pub fn crypto_positions(ctx: &Ctx, account: &str, cursor: Option<&str>) -> Result<(), CliError> {
    let args = Args::new()
        .str("rhs_account_number", account)
        .opt("cursor", cursor)
        .build();
    run_read(ctx, "crypto-positions", "get_crypto_positions", args)
}

/// `crypto orders list --account [--state --side --symbol --since --cursor]` →
/// `get_crypto_orders`.
#[allow(clippy::too_many_arguments)]
pub fn crypto_orders(
    ctx: &Ctx,
    account: &str,
    order_id: Option<&str>,
    state: Option<&str>,
    side: Option<&str>,
    symbol: Option<&str>,
    since: Option<&str>,
    cursor: Option<&str>,
) -> Result<(), CliError> {
    let args = Args::new()
        .str("rhs_account_number", account)
        .opt("order_id", order_id)
        .opt("state", state)
        .opt("side", side)
        .opt("symbol", symbol)
        .opt("created_at_gte", since)
        .opt("cursor", cursor)
        .build();
    run_read(ctx, "crypto-orders", "get_crypto_orders", args)
}

/// The validated terms of a crypto order, shared by preview and place.
pub struct CryptoOrderInput<'a> {
    pub account: &'a str,
    pub symbol: &'a str,
    pub side: &'a str,
    pub kind: &'a str,
    pub quantity: Option<&'a str>,
    pub dollar_amount: Option<&'a str>,
    pub limit_price: Option<&'a str>,
    pub stop_price: Option<&'a str>,
    pub tif: Option<&'a str>,
}

/// Validate crypto order terms from the arguments alone. Exactly one of
/// quantity / dollar_amount; limit and stop prices required by type.
pub fn validate_crypto(input: &CryptoOrderInput) -> Result<(), CliError> {
    super::order::Side::parse(input.side)?;
    let kind = input.kind.trim().to_lowercase();
    if !matches!(
        kind.as_str(),
        "market" | "limit" | "stop_loss" | "stop_limit"
    ) {
        return Err(CliError::Usage(
            "--type must be market, limit, stop_loss or stop_limit".into(),
        ));
    }
    match (input.quantity, input.dollar_amount) {
        (Some(_), Some(_)) | (None, None) => {
            return Err(CliError::Usage(
                "pass exactly one of --quantity or --dollar-amount".into(),
            ))
        }
        (Some(q), None) => {
            super::order::positive_decimal("--quantity", q)?;
        }
        (None, Some(d)) => {
            super::order::positive_decimal("--dollar-amount", d)?;
        }
    }
    if matches!(kind.as_str(), "limit" | "stop_limit") {
        let p = input
            .limit_price
            .ok_or_else(|| CliError::Usage(format!("--limit-price is required for {kind}")))?;
        super::order::positive_decimal("--limit-price", p)?;
    }
    if matches!(kind.as_str(), "stop_loss" | "stop_limit") {
        let p = input
            .stop_price
            .ok_or_else(|| CliError::Usage(format!("--stop-price is required for {kind}")))?;
        super::order::positive_decimal("--stop-price", p)?;
    }
    Ok(())
}

/// Build `preview_crypto_order` / `place_crypto_order` arguments.
pub fn crypto_order_args(input: &CryptoOrderInput, ref_id: Option<&str>) -> Value {
    Args::new()
        .str("rhs_account_number", input.account)
        .str("symbol", input.symbol.trim().to_uppercase())
        .str("side", input.side)
        .str("type", input.kind.trim().to_lowercase())
        .opt("quantity", input.quantity)
        .opt("dollar_amount", input.dollar_amount)
        .opt("limit_price", input.limit_price)
        .opt("stop_price", input.stop_price)
        .opt("time_in_force", input.tif)
        .opt("ref_id", ref_id)
        .build()
}

/// `crypto order preview …` → `preview_crypto_order` (simulation, no write).
pub fn crypto_preview(ctx: &Ctx, input: &CryptoOrderInput) -> Result<(), CliError> {
    validate_crypto(input)?;
    run_read(
        ctx,
        "crypto-order-preview",
        "preview_crypto_order",
        crypto_order_args(input, None),
    )
}

/// `crypto order place …` → `preview_crypto_order` then `place_crypto_order`
/// (WRITE).
pub fn crypto_place(
    ctx: &Ctx,
    input: &CryptoOrderInput,
    ref_id: Option<&str>,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    validate_crypto(input)?;
    let size = input
        .quantity
        .map(|q| format!("{q} {}", input.symbol))
        .unwrap_or_else(|| {
            format!(
                "${} of {}",
                input.dollar_amount.unwrap_or("?"),
                input.symbol
            )
        });
    run_write(
        ctx,
        "crypto-order-dry-run",
        "crypto-order-place",
        "place_crypto_order",
        crypto_order_args(input, ref_id),
        dry_run,
        force,
        &format!(
            "Place a LIVE crypto {} of {size} on account {}?",
            input.side, input.account
        ),
    )
}

/// `crypto order cancel <ORDER_ID> --account` → `cancel_crypto_order` (WRITE).
pub fn crypto_cancel(
    ctx: &Ctx,
    account: &str,
    order_id: &str,
    dry_run: bool,
    force: bool,
) -> Result<(), CliError> {
    let args = Args::new()
        .str("rhs_account_number", account)
        .str("order_id", order_id)
        .build();
    run_write(
        ctx,
        "crypto-order-cancel-dry-run",
        "crypto-order-cancel",
        "cancel_crypto_order",
        args,
        dry_run,
        force,
        &format!("Cancel LIVE crypto order {order_id} on account {account}?"),
    )
}

// ------------------------------------------------------------- validators

/// Parse a raw JSON object from a `--…-json` flag.
fn parse_object(flag: &str, raw: Option<&str>) -> Result<Option<Value>, CliError> {
    let Some(raw) = raw else { return Ok(None) };
    let v: Value = serde_json::from_str(raw)
        .map_err(|e| CliError::Usage(format!("{flag} is not valid JSON: {e}")))?;
    if !v.is_object() {
        return Err(CliError::Usage(format!("{flag} must be a JSON object")));
    }
    Ok(Some(v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn oco() -> OcoInput<'static> {
        OcoInput {
            account: "SAMPLE01",
            symbol: "spy",
            side: "sell",
            quantity: "10",
            take_profit: "780.00",
            stop_loss: "700.00",
            tif: Some("gtc"),
        }
    }

    #[test]
    fn oco_args_share_symbol_side_quantity_and_pin_regular_hours() {
        let a = advanced_args(&oco(), Some("REF-1"));
        assert_eq!(a["account_number"], "SAMPLE01");
        assert_eq!(a["symbol"], "SPY", "symbol is upper-cased");
        assert_eq!(a["side"], "sell");
        assert_eq!(a["quantity"], "10");
        assert_eq!(a["take_profit_limit_price"], "780.00");
        assert_eq!(a["stop_loss_stop_price"], "700.00");
        assert_eq!(a["time_in_force"], "gtc");
        assert_eq!(
            a["market_hours"], "regular_hours",
            "OCO supports no other session"
        );
        assert_eq!(a["ref_id"], "REF-1");
        assert!(advanced_args(&oco(), None).get("ref_id").is_none());
    }

    #[test]
    fn oco_terms_are_validated_before_any_network() {
        assert!(validate_oco(&oco()).is_ok());
        let mut bad = oco();
        bad.quantity = "1.5";
        assert_eq!(
            validate_oco(&bad).unwrap_err().exit_code(),
            2,
            "fractional shares"
        );
        let mut same = oco();
        same.take_profit = "700.00";
        assert_eq!(
            validate_oco(&same).unwrap_err().exit_code(),
            2,
            "legs must differ"
        );
        let mut side = oco();
        side.side = "sideways";
        assert!(validate_oco(&side).is_err());
        let mut tif = oco();
        tif.tif = Some("forever");
        assert!(validate_oco(&tif).is_err());
        let mut neg = oco();
        neg.stop_loss = "-1";
        assert!(validate_oco(&neg).is_err());
    }

    fn crypto(kind: &'static str) -> CryptoOrderInput<'static> {
        CryptoOrderInput {
            account: "12345",
            symbol: "btc",
            side: "buy",
            kind,
            quantity: Some("0.001"),
            dollar_amount: None,
            limit_price: None,
            stop_price: None,
            tif: None,
        }
    }

    #[test]
    fn crypto_orders_need_exactly_one_size_and_the_prices_their_type_requires() {
        assert!(validate_crypto(&crypto("market")).is_ok());
        let mut both = crypto("market");
        both.dollar_amount = Some("100.00");
        assert!(
            validate_crypto(&both).is_err(),
            "quantity and dollar_amount"
        );
        let mut neither = crypto("market");
        neither.quantity = None;
        assert!(validate_crypto(&neither).is_err());
        let mut limit = crypto("limit");
        assert!(
            validate_crypto(&limit).is_err(),
            "limit needs a limit price"
        );
        limit.limit_price = Some("60000");
        assert!(validate_crypto(&limit).is_ok());
        let mut stop = crypto("stop_loss");
        assert!(validate_crypto(&stop).is_err(), "stop needs a stop price");
        stop.stop_price = Some("50000");
        assert!(validate_crypto(&stop).is_ok());
        let mut kind = crypto("trailing");
        kind.quantity = Some("1");
        assert!(validate_crypto(&kind).is_err());
    }

    #[test]
    fn crypto_args_use_the_numeric_account_field_and_upper_case_the_symbol() {
        let a = crypto_order_args(&crypto("market"), Some("REF"));
        assert_eq!(a["rhs_account_number"], "12345");
        assert_eq!(a["symbol"], "BTC");
        assert_eq!(a["type"], "market");
        assert_eq!(a["quantity"], "0.001");
        assert!(
            a.get("dollar_amount").is_none(),
            "absent optionals are omitted"
        );
        assert_eq!(a["ref_id"], "REF");
    }

    #[test]
    fn indicator_json_must_be_an_object() {
        assert_eq!(parse_object("--indicator-json", None).unwrap(), None);
        assert_eq!(
            parse_object("--indicator-json", Some(r#"{"kind":"rsi","period":14}"#)).unwrap(),
            Some(json!({"kind": "rsi", "period": 14}))
        );
        assert!(parse_object("--indicator-json", Some("[1,2]")).is_err());
        assert!(parse_object("--indicator-json", Some("nope")).is_err());
    }
}
