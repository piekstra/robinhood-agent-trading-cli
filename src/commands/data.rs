//! Thin read commands over the account-state, market-data, and research tools.
//!
//! Each builds the tool's real `arguments` (schemas VERIFIED live 2026-08-27 —
//! see `docs/api.md §7`) and emits the result under a `<name>/v1` DTO. Account-
//! scoped reads take a required `--account`, passed explicitly per-invocation
//! (never ambient — see `config.rs`). Reads never mutate, so none are guarded.

use pk_cli_core::CliError;

use super::args::{run_read, Args};
use super::Ctx;

// ------------------------------------------------ portfolio / pnl / margin

/// `portfolio get --account` → `get_portfolio`.
pub fn portfolio_get(ctx: &Ctx, account: &str) -> Result<(), CliError> {
    run_read(
        ctx,
        "portfolio",
        "get_portfolio",
        Args::new().str("account_number", account).build(),
    )
}

/// `pnl trade-history --account [--span --symbol --cursor]` → `get_pnl_trade_history`.
pub fn pnl_trade_history(
    ctx: &Ctx,
    account: &str,
    span: Option<&str>,
    symbol: Option<&str>,
    cursor: Option<&str>,
) -> Result<(), CliError> {
    let args = Args::new()
        .str("account_number", account)
        .opt("span", span)
        .opt("symbol", symbol)
        .opt("cursor", cursor)
        .build();
    run_read(ctx, "pnl-trade-history", "get_pnl_trade_history", args)
}

/// `pnl realized --account [--span --start-date --end-date --display-currency]` →
/// `get_realized_pnl`.
pub fn pnl_realized(
    ctx: &Ctx,
    account: &str,
    span: Option<&str>,
    start_date: Option<&str>,
    end_date: Option<&str>,
    display_currency: Option<&str>,
) -> Result<(), CliError> {
    let args = Args::new()
        .str("account_number", account)
        .opt("span", span)
        .opt("start_date", start_date)
        .opt("end_date", end_date)
        .opt("display_currency", display_currency)
        .build();
    run_read(ctx, "realized-pnl", "get_realized_pnl", args)
}

/// `margin equity-upgrade-info --account` → `get_limited_margin_upgrade_info`.
pub fn margin_equity_upgrade(ctx: &Ctx, account: &str) -> Result<(), CliError> {
    run_read(
        ctx,
        "margin-upgrade",
        "get_limited_margin_upgrade_info",
        Args::new().str("account_number", account).build(),
    )
}

/// `margin option-upgrade-info --account` → `get_option_level_upgrade_info`.
pub fn margin_option_upgrade(ctx: &Ctx, account: &str) -> Result<(), CliError> {
    run_read(
        ctx,
        "option-level-upgrade",
        "get_option_level_upgrade_info",
        Args::new().str("account_number", account).build(),
    )
}

// ---------------------------------------------------------- equity extras

/// `equity fundamentals --symbols [--bounds]` → `get_equity_fundamentals`.
pub fn equity_fundamentals(ctx: &Ctx, symbols: &str, bounds: Option<&str>) -> Result<(), CliError> {
    let args = Args::new()
        .csv("symbols", symbols)
        .opt("bounds", bounds)
        .build();
    run_read(ctx, "equity-fundamentals", "get_equity_fundamentals", args)
}

/// `equity historicals --symbols --start-time [--end-time --interval --bounds
/// --adjustment-type]` → `get_equity_historicals`.
#[allow(clippy::too_many_arguments)]
pub fn equity_historicals(
    ctx: &Ctx,
    symbols: &str,
    start_time: &str,
    end_time: Option<&str>,
    interval: Option<&str>,
    bounds: Option<&str>,
    adjustment_type: Option<&str>,
) -> Result<(), CliError> {
    let args = Args::new()
        .csv("symbols", symbols)
        .str("start_time", start_time)
        .opt("end_time", end_time)
        .opt("interval", interval)
        .opt("bounds", bounds)
        .opt("adjustment_type", adjustment_type)
        .build();
    run_read(ctx, "equity-historicals", "get_equity_historicals", args)
}

/// `equity price-book --symbols` → `get_equity_price_book`.
pub fn equity_price_book(ctx: &Ctx, symbols: &str) -> Result<(), CliError> {
    run_read(
        ctx,
        "equity-price-book",
        "get_equity_price_book",
        Args::new().csv("symbols", symbols).build(),
    )
}

/// `equity tax-lots --account --symbol [--cursor]` → `get_equity_tax_lots`.
pub fn equity_tax_lots(
    ctx: &Ctx,
    account: &str,
    symbol: &str,
    cursor: Option<&str>,
) -> Result<(), CliError> {
    let args = Args::new()
        .str("account_number", account)
        .str("symbol", symbol)
        .opt("cursor", cursor)
        .build();
    run_read(ctx, "equity-tax-lots", "get_equity_tax_lots", args)
}

/// `equity technicals --symbol --type --interval --start-time [--end-time
/// --output --period]` → `get_equity_technical_indicators`. The exotic
/// per-indicator params (num_std, fast/slow/signal_period, multiplier, method)
/// are reachable via `rhap call get_equity_technical_indicators --args …`.
#[allow(clippy::too_many_arguments)]
pub fn equity_technicals(
    ctx: &Ctx,
    symbol: &str,
    kind: &str,
    interval: &str,
    start_time: &str,
    end_time: Option<&str>,
    output: Option<&str>,
    period: Option<i64>,
) -> Result<(), CliError> {
    let args = Args::new()
        .str("symbol", symbol)
        .str("type", kind)
        .str("interval", interval)
        .str("start_time", start_time)
        .opt("end_time", end_time)
        .opt("output", output)
        .opt_int("period", period)
        .build();
    run_read(
        ctx,
        "equity-technicals",
        "get_equity_technical_indicators",
        args,
    )
}

/// `equity tradability --account --symbols` → `get_equity_tradability`.
pub fn equity_tradability(ctx: &Ctx, account: &str, symbols: &str) -> Result<(), CliError> {
    let args = Args::new()
        .str("account_number", account)
        .csv("symbols", symbols)
        .build();
    run_read(ctx, "equity-tradability", "get_equity_tradability", args)
}

// ----------------------------------------------------------------- index

/// `index list [--symbols]` → `get_indexes` (its `symbols` is a single string).
pub fn index_list(ctx: &Ctx, symbols: Option<&str>) -> Result<(), CliError> {
    run_read(
        ctx,
        "indexes",
        "get_indexes",
        Args::new().opt("symbols", symbols).build(),
    )
}

/// `index quotes --instrument-ids` → `get_index_quotes`.
pub fn index_quotes(ctx: &Ctx, instrument_ids: &str) -> Result<(), CliError> {
    run_read(
        ctx,
        "index-quotes",
        "get_index_quotes",
        Args::new().csv("instrument_ids", instrument_ids).build(),
    )
}

/// `index historicals --instrument-ids --start-time --interval [--end-time]` →
/// `get_index_historicals`.
pub fn index_historicals(
    ctx: &Ctx,
    instrument_ids: &str,
    start_time: &str,
    interval: &str,
    end_time: Option<&str>,
) -> Result<(), CliError> {
    let args = Args::new()
        .csv("instrument_ids", instrument_ids)
        .str("start_time", start_time)
        .str("interval", interval)
        .opt("end_time", end_time)
        .build();
    run_read(ctx, "index-historicals", "get_index_historicals", args)
}

// -------------------------------------------------------- market / research

/// `market earnings-calendar [--start-date --days --filter]` →
/// `get_earnings_calendar`.
pub fn earnings_calendar(
    ctx: &Ctx,
    start_date: Option<&str>,
    days: Option<i64>,
    filter: Option<&str>,
) -> Result<(), CliError> {
    let args = Args::new()
        .opt("start_date", start_date)
        .opt_int("days", days)
        .opt("filter", filter)
        .build();
    run_read(ctx, "earnings-calendar", "get_earnings_calendar", args)
}

/// `market earnings-results --symbol` → `get_earnings_results`.
pub fn earnings_results(ctx: &Ctx, symbol: &str) -> Result<(), CliError> {
    run_read(
        ctx,
        "earnings-results",
        "get_earnings_results",
        Args::new().str("symbol", symbol).build(),
    )
}

/// `market financials --symbols [--period --limit]` → `get_financials`.
pub fn financials(
    ctx: &Ctx,
    symbols: &str,
    period: Option<&str>,
    limit: Option<i64>,
) -> Result<(), CliError> {
    let args = Args::new()
        .csv("symbols", symbols)
        .opt("period", period)
        .opt_int("limit", limit)
        .build();
    run_read(ctx, "financials", "get_financials", args)
}

/// `search <query> [--asset-type --limit]` → `search`.
pub fn search(
    ctx: &Ctx,
    query: &str,
    asset_type: Option<&str>,
    limit: Option<i64>,
) -> Result<(), CliError> {
    let args = Args::new()
        .str("query", query)
        .opt("asset_type", asset_type)
        .opt_int("limit", limit)
        .build();
    run_read(ctx, "search", "search", args)
}
