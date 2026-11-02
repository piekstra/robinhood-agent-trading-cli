//! `rhap` — Robinhood Agentic Passthrough.
//!
//! A deterministic, LLM-free client for Robinhood's **official** Agentic
//! Trading MCP server (`https://agent.robinhood.com/mcp/trading`): JSON-RPC 2.0
//! over Streamable HTTP, authenticated with OAuth 2.1 + PKCE. It maps CLI
//! arguments 1:1 onto MCP tool calls and returns JSON. Conforms to
//! `piekstra-cli/1` (see `cli-common/DESIGN.md`).
//!
//! **Write-capable, not read-only.** The MCP grants READ across all linked
//! accounts and WRITE (place/cancel orders) to the funded Agentic account.
//! `order place`/`order cancel` reach the write tools: they build and print with
//! `--dry-run`, and a live run is gated by a typed confirmation (exit 6
//! non-interactively without `--force`). This file owns the clap tree and the
//! exit-code mapping.

mod catalog;
mod commands;
mod config;
mod credential;
mod discovery;
mod mcp;
mod oauth;
mod token_refresh;

use std::path::PathBuf;

use crate::commands::Ctx;
use crate::config::{Config, BIN};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use pk_cli_auth::{LogoutArgs, SetCredentialArgs};
use pk_cli_config::ConfigStore;
use pk_cli_core::{output, CliError, CommonArgs};
use pk_cli_secrets::CredentialStore;

/// Deterministic passthrough to Robinhood's official Agentic Trading MCP
/// server (conforms to piekstra-cli/1). Write-capable: order commands use
/// dry-run + confirmation.
#[derive(Parser, Debug)]
#[command(name = BIN, version, about, long_about = None)]
struct Cli {
    #[command(flatten)]
    common: CommonArgs,

    // There is deliberately NO global `--account`. Authentication is global, but
    // the account is never ambient: an account-scoped command carries its own
    // required `--account` (see `config.rs` for the rationale — a defaulted
    // account is a wrong-account hazard under concurrent use).
    /// Override the config file location.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// OAuth login, session status, logout, and raw token write.
    #[command(subcommand)]
    Auth(AuthCmd),
    /// Discover the MCP tool surface: `tools list` (live) / `tools show <NAME>`.
    #[command(subcommand)]
    Tools(ToolsCmd),
    /// Linked brokerage accounts (get_accounts).
    #[command(subcommand)]
    Accounts(AccountsCmd),
    /// Equity positions (get_equity_positions).
    #[command(subcommand)]
    Positions(PositionsCmd),
    /// Equity orders (get_equity_orders).
    #[command(subcommand)]
    Orders(OrdersCmd),
    /// Equity quotes (get_equity_quotes).
    #[command(subcommand)]
    Quotes(QuotesCmd),
    /// Place or cancel an equity order (WRITE — dry-run + confirmation).
    #[command(subcommand)]
    Order(OrderCmd),
    /// Options: positions, orders, quotes, chains, instruments, historicals,
    /// watchlist, order place/cancel, exercise (WRITE).
    #[command(subcommand)]
    Option(OptionCmd),
    /// Account portfolio snapshot (get_portfolio).
    #[command(subcommand)]
    Portfolio(PortfolioCmd),
    /// Profit-and-loss (get_pnl_trade_history / get_realized_pnl).
    #[command(subcommand)]
    Pnl(PnlCmd),
    /// Margin / option-level upgrade info.
    #[command(subcommand)]
    Margin(MarginCmd),
    /// Equity market data: fundamentals, historicals, price book, tax lots, …
    #[command(subcommand)]
    Equity(EquityCmd),
    /// Indexes: list, quotes, historicals.
    #[command(subcommand)]
    Index(IndexCmd),
    /// Market/research: earnings, financials.
    #[command(subcommand)]
    Market(MarketCmd),
    /// Symbol search (search).
    Search {
        /// The search query.
        query: String,
        /// Restrict to an asset type.
        #[arg(long = "asset-type", value_name = "TYPE")]
        asset_type: Option<String>,
        /// Maximum results.
        #[arg(long, value_name = "N")]
        limit: Option<i64>,
    },
    /// Watchlists: list/items/popular + create/update/add/remove/follow (WRITE).
    #[command(subcommand)]
    Watchlists(WatchlistsCmd),
    /// Scanners: list/filter-specs/run + create/update-* (WRITE).
    #[command(subcommand)]
    Scans(ScansCmd),
    /// Named, versioned contract metrics (`metrics list`); scored via `option score`.
    #[command(subcommand)]
    Metrics(MetricsCmd),
    /// ARK Invest activity metric for tickers (`ark score`; external read + MCP prices).
    #[command(subcommand)]
    Ark(ArkCmd),
    /// Equity news (get_equity_news).
    #[command(subcommand)]
    News(NewsCmd),
    /// SEC filings: index, document sections, XBRL facts.
    #[command(subcommand)]
    Sec(SecCmd),
    /// Price/indicator alerts and the alert log (poll-only stream).
    #[command(subcommand)]
    Alerts(AlertsCmd),
    /// Crypto: pairs, quotes, positions, orders (WRITE paths guarded).
    #[command(subcommand)]
    Crypto(CryptoCmd),
    /// Generic passthrough: invoke any MCP tool by name (escape hatch).
    Call {
        /// The MCP tool name (see `rhap tools list`).
        tool: String,
        /// The tool's arguments as a JSON object.
        #[arg(long, value_name = "JSON")]
        args: Option<String>,
        /// Build and print the JSON-RPC request without sending it.
        #[arg(long)]
        dry_run: bool,
        /// Send a mutating tool for real (required for a live mutation).
        #[arg(long)]
        force: bool,
    },
    /// Non-secret settings.
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Print a shell completion script.
    Completions { shell: Shell },
    /// Machine-readable capability discovery (cli-info/v1).
    Info,
}

#[derive(Subcommand, Debug)]
enum AuthCmd {
    /// Authorize rhap in a browser (OAuth 2.1 + PKCE) and store the tokens.
    Login(RhLoginArgs),
    /// Report token presence/expiry (auth-status/v1). `--verify` spends a call.
    Status(StatusArgs),
    /// Clear the session; --forget also removes the stored config (client id).
    Logout(LogoutArgs),
    /// Raw keychain write of a token bundle for rotation / headless setup.
    SetCredential(SetCredentialArgs),
}

#[derive(clap::Args, Debug)]
struct RhLoginArgs {
    /// Loopback port for the OAuth redirect (0 = a free ephemeral port).
    #[arg(long, default_value_t = 0, value_name = "PORT")]
    port: u16,
    /// Replace an existing stored session instead of failing.
    #[arg(long)]
    overwrite: bool,
    /// Don't launch a browser (the URL is always printed).
    #[arg(long)]
    no_open: bool,
    /// Skip the post-login verification request.
    #[arg(long)]
    no_verify: bool,
}

#[derive(clap::Args, Debug)]
struct StatusArgs {
    /// Spend a request confirming the token still works.
    #[arg(long)]
    verify: bool,
}

#[derive(Subcommand, Debug)]
enum ToolsCmd {
    /// List the MCP tools the server exposes (live tools/list).
    #[command(visible_alias = "ls")]
    List {
        /// Include each tool's full input schema.
        #[arg(long)]
        schemas: bool,
    },
    /// Show one tool's full input schema (from the embedded catalog, offline).
    Show {
        /// The tool name (see `tools list`).
        name: String,
    },
}

#[derive(Subcommand, Debug)]
enum NewsCmd {
    /// Recent news articles for one symbol.
    Get {
        #[arg(long, required = true, value_name = "SYMBOL")]
        symbol: String,
        /// Max articles, 1-50.
        #[arg(long, value_name = "N")]
        limit: Option<i64>,
        #[arg(long, value_name = "CURSOR")]
        cursor: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum SecCmd {
    /// List a company's filings (optionally by form type and date).
    Index {
        #[arg(long, required = true, value_name = "SYMBOL")]
        symbol: String,
        /// Form types, comma-separated, e.g. 8-K,10-Q.
        #[arg(long = "form-type", value_name = "FORM[,FORM...]")]
        form_type: Option<String>,
        #[arg(long, value_name = "YYYY-MM-DD")]
        since: Option<String>,
        #[arg(long, value_name = "YYYY-MM-DD")]
        until: Option<String>,
        #[arg(long, value_name = "CURSOR")]
        cursor: Option<String>,
    },
    /// Fetch one filing, optionally a single section.
    Filing {
        #[arg(long = "filing-id", required = true, value_name = "ID")]
        filing_id: String,
        #[arg(long, value_name = "SECTION")]
        section: Option<String>,
    },
    /// XBRL facts for filings by concept.
    Facts {
        #[arg(long = "filing-ids", required = true, value_name = "ID[,ID...]")]
        filing_ids: String,
        #[arg(long, required = true, value_name = "CONCEPT[,CONCEPT...]")]
        concepts: String,
    },
    /// The concept catalog of one filing.
    #[command(name = "facts-catalog")]
    FactsCatalog {
        #[arg(long = "filing-id", required = true, value_name = "ID")]
        filing_id: String,
        #[arg(long = "concept-contains", value_name = "TEXT")]
        concept_contains: Option<String>,
        #[arg(long = "axis-name-in", value_name = "AXIS[,AXIS...]")]
        axis_name_in: Option<String>,
        #[arg(long, value_name = "N")]
        offset: Option<i64>,
    },
}

#[derive(Subcommand, Debug)]
enum AlertsCmd {
    /// List configured alerts.
    #[command(visible_alias = "ls")]
    List {
        #[arg(long, value_name = "SYMBOL")]
        symbol: Option<String>,
        #[arg(long = "asset-class", value_name = "CLASS")]
        asset_class: Option<String>,
        #[arg(long, value_name = "CURSOR")]
        cursor: Option<String>,
    },
    /// The alert log — poll this; nothing is pushed.
    Log {
        #[arg(long = "asset-class", value_name = "CLASS")]
        asset_class: Option<String>,
        #[arg(long, value_name = "TIMESTAMP")]
        since: Option<String>,
        #[arg(long, value_name = "CURSOR")]
        cursor: Option<String>,
        #[arg(long, value_name = "N")]
        limit: Option<i64>,
    },
    /// Create an alert (WRITE).
    Create {
        #[arg(long, required = true, value_name = "SYMBOL")]
        symbol: String,
        #[arg(long = "condition-type", required = true, value_name = "TYPE")]
        condition_type: String,
        #[arg(long, value_name = "PRICE")]
        threshold: Option<String>,
        #[arg(long = "asset-class", value_name = "CLASS")]
        asset_class: Option<String>,
        #[arg(long = "indicator-json", value_name = "JSON")]
        indicator_json: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
    /// Update an alert (WRITE).
    Update {
        #[arg(long = "alert-id", required = true, value_name = "ID")]
        alert_id: String,
        #[arg(long, value_name = "BOOL")]
        enabled: Option<bool>,
        #[arg(long = "condition-type", value_name = "TYPE")]
        condition_type: Option<String>,
        #[arg(long, value_name = "PRICE")]
        threshold: Option<String>,
        #[arg(long = "indicator-json", value_name = "JSON")]
        indicator_json: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
    /// Delete an alert (WRITE).
    Delete {
        alert_id: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
    /// Mark alert-log entries read (WRITE).
    #[command(name = "mark-read")]
    MarkRead {
        #[arg(long, value_name = "ID[,ID...]")]
        ids: Option<String>,
        #[arg(long = "all-through", value_name = "TIMESTAMP")]
        all_through: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
enum CryptoCmd {
    /// Crypto onboarding status for the login.
    Onboarding,
    /// Tradable currency pairs.
    Pairs {
        #[arg(long, value_name = "CURSOR")]
        cursor: Option<String>,
        #[arg(long, value_name = "N")]
        limit: Option<i64>,
    },
    /// Quotes for crypto symbols.
    Quotes {
        #[arg(long, required = true, value_name = "SYM[,SYM...]")]
        symbols: String,
        #[arg(long, value_name = "TZ")]
        timezone: Option<String>,
        /// Numeric rhs_account_number (crypto tools use this, not the account id).
        #[arg(long, value_name = "ID")]
        account: Option<String>,
    },
    /// Crypto positions for one account.
    Positions {
        #[arg(long, required = true, value_name = "ID")]
        account: String,
        #[arg(long, value_name = "CURSOR")]
        cursor: Option<String>,
    },
    /// Crypto orders (list/preview/place/cancel).
    #[command(subcommand)]
    Order(CryptoOrderCmd),
}

#[derive(Subcommand, Debug)]
enum CryptoOrderCmd {
    /// List crypto orders.
    #[command(visible_alias = "ls")]
    List {
        #[arg(long, required = true, value_name = "ID")]
        account: String,
        #[arg(long = "order-id", value_name = "ID")]
        order_id: Option<String>,
        #[arg(long, value_name = "STATE")]
        state: Option<String>,
        #[arg(long, value_name = "SIDE")]
        side: Option<String>,
        #[arg(long, value_name = "SYMBOL")]
        symbol: Option<String>,
        #[arg(long, value_name = "TIMESTAMP")]
        since: Option<String>,
        #[arg(long, value_name = "CURSOR")]
        cursor: Option<String>,
    },
    /// Simulate a crypto order (no write).
    Preview(CryptoOrderArgs),
    /// Place a crypto order (WRITE).
    Place {
        #[command(flatten)]
        terms: CryptoOrderArgs,
        #[arg(long = "ref-id", value_name = "UUID")]
        ref_id: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
    /// Cancel a crypto order (WRITE).
    Cancel {
        order_id: String,
        #[arg(long, required = true, value_name = "ID")]
        account: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
}

#[derive(clap::Args, Debug)]
struct CryptoOrderArgs {
    /// Numeric rhs_account_number.
    #[arg(long, required = true, value_name = "ID")]
    account: String,
    #[arg(long, required = true, value_name = "SYMBOL")]
    symbol: String,
    #[arg(long, required = true, value_name = "SIDE")]
    side: String,
    /// market | limit | stop_loss | stop_limit.
    #[arg(long = "type", required = true, value_name = "TYPE")]
    kind: String,
    #[arg(long, value_name = "QTY")]
    quantity: Option<String>,
    #[arg(long = "dollar-amount", value_name = "USD")]
    dollar_amount: Option<String>,
    #[arg(long = "limit-price", value_name = "PRICE")]
    limit_price: Option<String>,
    #[arg(long = "stop-price", value_name = "PRICE")]
    stop_price: Option<String>,
    #[arg(long, value_name = "TIF")]
    tif: Option<String>,
}

#[derive(Subcommand, Debug)]
enum AdvancedCmd {
    /// List OCO / contingency orders.
    #[command(visible_alias = "ls")]
    List {
        #[arg(long, required = true, value_name = "ID")]
        account: String,
        #[arg(long = "order-id", value_name = "ID")]
        order_id: Option<String>,
        #[arg(long = "contingency-type", value_name = "KIND")]
        contingency_type: Option<String>,
        #[arg(long, value_name = "TIMESTAMP")]
        since: Option<String>,
        #[arg(long, value_name = "CURSOR")]
        cursor: Option<String>,
    },
    /// Simulate an OCO bracket (no write).
    Review(OcoArgs),
    /// Place an OCO bracket: take-profit limit + stop-loss stop (WRITE).
    Place {
        #[command(flatten)]
        terms: OcoArgs,
        #[arg(long = "ref-id", value_name = "UUID")]
        ref_id: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
    /// Cancel an advanced order (WRITE).
    Cancel {
        order_id: String,
        #[arg(long, required = true, value_name = "ID")]
        account: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
}

#[derive(clap::Args, Debug)]
struct OcoArgs {
    #[arg(long, required = true, value_name = "ID")]
    account: String,
    #[arg(long, required = true, value_name = "SYMBOL")]
    symbol: String,
    /// buy | sell — both legs use it (sell protects a long).
    #[arg(long, required = true, value_name = "SIDE")]
    side: String,
    /// Whole shares; both legs use it.
    #[arg(long, required = true, value_name = "QTY")]
    quantity: String,
    #[arg(long = "take-profit", required = true, value_name = "PRICE")]
    take_profit: String,
    #[arg(long = "stop-loss", required = true, value_name = "PRICE")]
    stop_loss: String,
    #[arg(long, value_name = "TIF")]
    tif: Option<String>,
}

#[derive(Subcommand, Debug)]
enum ArkCmd {
    /// Rank ARK-held tickers by a named ticker metric (default ark/2).
    Score {
        /// Metric codename (see `rhap metrics list`).
        #[arg(long, default_value = "ark/2", value_name = "NAME")]
        metric: String,
        /// Window in sessions of holdings history.
        #[arg(long, default_value_t = 25, value_name = "N")]
        window: usize,
        /// ARK funds to include, comma-separated.
        #[arg(
            long,
            default_value = "ARKK,ARKW,ARKQ,ARKG,ARKF,ARKX",
            value_name = "F[,F...]"
        )]
        funds: String,
        /// Only these tickers (default: every name ARK is net-accumulating).
        #[arg(long, value_name = "T[,T...]")]
        tickers: Option<String>,
        /// How many names to emit, best first.
        #[arg(long, default_value_t = 15, value_name = "N")]
        top: usize,
        /// Cash available for one cash-secured put; sets `wheel_feasible`.
        #[arg(long, value_name = "USD")]
        collateral: Option<f64>,
        /// Holdings API base URL.
        #[arg(
            long = "base-url",
            default_value = "https://arkfunds.io",
            value_name = "URL"
        )]
        base_url: String,
    },
}

#[derive(Subcommand, Debug)]
enum MetricsCmd {
    /// List the published metric codenames and their factor weights (offline).
    #[command(visible_alias = "ls")]
    List,
}

#[derive(Subcommand, Debug)]
enum AccountsCmd {
    /// List linked brokerage accounts.
    #[command(visible_alias = "ls")]
    List,
}

#[derive(Subcommand, Debug)]
enum PositionsCmd {
    /// List equity positions for one account.
    #[command(visible_alias = "ls")]
    List {
        /// Account to act on (required; never defaulted from config or env).
        #[arg(long, required = true, value_name = "ID")]
        account: String,
    },
}

#[derive(Subcommand, Debug)]
enum OrdersCmd {
    /// List equity orders for one account (optionally since a date).
    #[command(visible_alias = "ls")]
    List {
        /// Account to act on (required; never defaulted from config or env).
        #[arg(long, required = true, value_name = "ID")]
        account: String,
        /// Only orders created on or after this date (ISO `YYYY-MM-DD`), mapped
        /// to the server's `created_at_gte`. This tool has no end-date filter.
        #[arg(long, value_name = "YYYY-MM-DD")]
        since: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum QuotesCmd {
    /// Quote a single symbol.
    Get {
        /// Ticker symbol, e.g. `AAPL`.
        symbol: String,
    },
}

#[derive(Subcommand, Debug)]
enum OrderCmd {
    /// Place an equity order (WRITE). Limit by default; `--type market` for
    /// fractional quantities, which the broker rejects on limit orders.
    Place {
        /// Account to act on (required; never defaulted from config or env).
        #[arg(long, required = true, value_name = "ID")]
        account: String,
        /// Ticker symbol, e.g. `AAPL`.
        #[arg(long, value_name = "SYMBOL")]
        symbol: String,
        /// `buy` or `sell`.
        #[arg(long, value_name = "SIDE")]
        side: String,
        /// Quantity (shares). Fractional requires `--type market`.
        #[arg(long, value_name = "QTY")]
        qty: Option<String>,
        /// USD notional instead of a share count. Requires `--type market`.
        #[arg(long, value_name = "USD")]
        dollar_amount: Option<String>,
        /// Order type: `limit` (default) or `market`.
        #[arg(long = "type", value_name = "TYPE", default_value = "limit")]
        order_type: String,
        /// Limit price. Required for `--type limit`, rejected for market.
        #[arg(long, value_name = "PRICE")]
        limit: Option<String>,
        /// Session: regular_hours (default) | extended_hours | all_day_hours.
        #[arg(long, value_name = "HOURS", default_value = "regular_hours")]
        market_hours: String,
        /// Time in force: day|gtc|ioc|fok.
        #[arg(long, value_name = "TIF")]
        tif: String,
        /// Build and print the JSON-RPC request without sending it.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation prompt for a live order.
        #[arg(long)]
        force: bool,
    },
    /// OCO bracket orders: take-profit + stop-loss that cancel each other.
    #[command(subcommand)]
    Advanced(AdvancedCmd),
    /// Cancel an equity order by id (WRITE).
    Cancel {
        /// Order id to cancel.
        id: String,
        /// Account to act on (required; never defaulted from config or env).
        #[arg(long, required = true, value_name = "ID")]
        account: String,
        /// Build and print the JSON-RPC request without sending it.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation prompt for a live cancel.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
enum OptionCmd {
    /// Option positions (get_option_positions).
    #[command(subcommand)]
    Positions(OptionPositionsCmd),
    /// Option orders (get_option_orders).
    #[command(subcommand)]
    Orders(OptionOrdersCmd),
    /// Option quotes by instrument id (get_option_quotes).
    #[command(subcommand)]
    Quotes(OptionQuotesCmd),
    /// Option chains for an underlying (get_option_chains).
    #[command(subcommand)]
    Chains(OptionChainsCmd),
    /// Option instruments (get_option_instruments).
    #[command(subcommand)]
    Instruments(OptionInstrumentsCmd),
    /// Option historicals (get_option_historicals).
    #[command(subcommand)]
    Historicals(OptionHistoricalsCmd),
    /// The option watchlist (get_option_watchlist).
    Watchlist,
    /// Score every vertical credit spread of one expiry with a named metric
    /// (READ: chains + instruments + quotes + spot + daily bars; no account).
    Score {
        /// Underlying ticker, e.g. `SPY`.
        #[arg(long, required = true, value_name = "SYMBOL")]
        underlying: String,
        /// Expiration date, ISO `YYYY-MM-DD`.
        #[arg(long, required = true, value_name = "YYYY-MM-DD")]
        expiration: String,
        /// `put` (default) or `call` credit spreads.
        #[arg(long = "type", default_value = "put", value_name = "TYPE")]
        kind: String,
        /// Strike width; defaults to the chain's native step near spot.
        #[arg(long, value_name = "W")]
        width: Option<f64>,
        /// Underlying class: index (default), sector, or leveraged.
        #[arg(long, default_value = "index", value_name = "CLASS")]
        class: String,
        /// Metric codename (see `rhap metrics list`).
        #[arg(long, default_value = "harvest/1", value_name = "NAME")]
        metric: String,
        /// Lowest short-leg |delta| to consider.
        #[arg(long = "min-delta", default_value_t = 0.15, value_name = "D")]
        min_delta: f64,
        /// Highest short-leg |delta| to consider.
        #[arg(long = "max-delta", default_value_t = 0.45, value_name = "D")]
        max_delta: f64,
        /// Skip the daily-bar pull (technical cushion scores 0).
        #[arg(long = "no-cushion")]
        no_cushion: bool,
        /// How many spreads to emit, best first.
        #[arg(long, default_value_t = 10, value_name = "N")]
        top: usize,
    },
    /// Place or cancel an option order (WRITE — dry-run + confirmation).
    #[command(subcommand)]
    Order(OptionOrderCmd),
    /// Exercise an option position (WRITE — account required).
    Exercise {
        /// Account to act on (required; never defaulted from config or env).
        #[arg(long, required = true, value_name = "ID")]
        account: String,
        /// The option instrument/position id to exercise.
        #[arg(long = "option-id", required = true, value_name = "ID")]
        option_id: String,
        /// Number of contracts to exercise.
        #[arg(long, required = true, value_name = "N")]
        quantity: i64,
        /// Idempotency ref id.
        #[arg(long = "ref-id", value_name = "ID")]
        ref_id: Option<String>,
        /// Reason string.
        #[arg(long, value_name = "TEXT")]
        reason: Option<String>,
        /// Allow exercising into a short position.
        #[arg(long = "allow-shorts")]
        allow_shorts: bool,
        /// Build and print the JSON-RPC request without sending it.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation prompt for a live exercise.
        #[arg(long)]
        force: bool,
    },
    /// Cancel a pending option exercise (WRITE — account required).
    #[command(name = "exercise-cancel")]
    ExerciseCancel {
        /// The option id whose exercise to cancel.
        option_id: String,
        /// Account to act on (required; never defaulted from config or env).
        #[arg(long, required = true, value_name = "ID")]
        account: String,
        /// Build and print the JSON-RPC request without sending it.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation prompt for a live cancel.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
enum OptionInstrumentsCmd {
    /// List option instruments (all filters optional).
    #[command(visible_alias = "ls")]
    List {
        #[arg(long = "chain-symbol", value_name = "SYM")]
        chain_symbol: Option<String>,
        #[arg(long = "chain-id", value_name = "ID")]
        chain_id: Option<String>,
        #[arg(long = "expiration-dates", value_name = "DATES")]
        expiration_dates: Option<String>,
        #[arg(long = "strike-price", value_name = "PRICE")]
        strike_price: Option<String>,
        #[arg(long = "type", value_name = "TYPE")]
        kind: Option<String>,
        #[arg(long, value_name = "STATE")]
        state: Option<String>,
        #[arg(long, value_name = "TRADABILITY")]
        tradability: Option<String>,
        #[arg(long, value_name = "ID[,ID...]")]
        ids: Option<String>,
        #[arg(long, value_name = "CURSOR")]
        cursor: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum OptionHistoricalsCmd {
    /// Historicals for option instrument ids.
    Get {
        #[arg(long = "instrument-ids", required = true, value_name = "ID[,ID...]")]
        instrument_ids: String,
        #[arg(long = "start-time", required = true, value_name = "TS")]
        start_time: String,
        #[arg(long = "end-time", value_name = "TS")]
        end_time: Option<String>,
        #[arg(long, value_name = "INTERVAL")]
        interval: Option<String>,
        #[arg(long, value_name = "BOUNDS")]
        bounds: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum OptionPositionsCmd {
    /// List option positions for one account.
    #[command(visible_alias = "ls")]
    List {
        /// Account to act on (required; never defaulted from config or env).
        #[arg(long, required = true, value_name = "ID")]
        account: String,
        /// Only positions with a nonzero quantity (default true → open
        /// positions). Pass `--nonzero false` to include closed/zero rows.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set, value_name = "BOOL")]
        nonzero: bool,
    },
}

#[derive(Subcommand, Debug)]
enum OptionOrdersCmd {
    /// List option orders for one account.
    #[command(visible_alias = "ls")]
    List {
        /// Account to act on (required; never defaulted from config or env).
        #[arg(long, required = true, value_name = "ID")]
        account: String,
        /// Only orders created on or after this date (ISO `YYYY-MM-DD`), mapped
        /// to `created_at_gte`.
        #[arg(long, value_name = "YYYY-MM-DD")]
        since: Option<String>,
        /// Filter by order state (e.g. `filled`, `cancelled`, `confirmed`).
        #[arg(long, value_name = "STATE")]
        state: Option<String>,
        /// Filter by placing agent (e.g. `agentic`).
        #[arg(long = "placed-agent", value_name = "AGENT")]
        placed_agent: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum OptionQuotesCmd {
    /// Quote option instruments by id.
    Get {
        /// Option instrument ids, comma-separated.
        #[arg(long = "instrument-ids", required = true, value_name = "ID[,ID...]")]
        instrument_ids: String,
    },
}

#[derive(Subcommand, Debug)]
enum OptionChainsCmd {
    /// Resolve the option chain for an underlying symbol.
    Get {
        /// Underlying ticker, e.g. `AAPL`.
        #[arg(long, required = true, value_name = "SYMBOL")]
        underlying: String,
    },
}

// The `Place` variant carries many optional flags and so dwarfs `Cancel`;
// boxing it would fight clap's derive (which needs the fields inline), and this
// enum is parsed exactly once at startup, so the size difference is immaterial.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand, Debug)]
enum OptionOrderCmd {
    /// Place an option order (WRITE). Single-leg via the leg flags, or multi-leg
    /// via `--legs <JSON>`.
    Place {
        /// Account to act on (required; never defaulted from config or env).
        #[arg(long, required = true, value_name = "ID")]
        account: String,
        /// Number of contracts.
        #[arg(long, required = true, value_name = "N")]
        quantity: String,
        /// Raw legs array as JSON (multi-leg passthrough). Mutually exclusive
        /// with the single-leg flags below.
        #[arg(long, value_name = "JSON")]
        legs: Option<String>,
        /// Single-leg: the option instrument id.
        #[arg(long = "instrument-id", value_name = "ID")]
        instrument_id: Option<String>,
        /// Single-leg: `open` or `close`.
        #[arg(long = "position-effect", value_name = "EFFECT")]
        position_effect: Option<String>,
        /// Single-leg: `buy` or `sell`.
        #[arg(long, value_name = "SIDE")]
        side: Option<String>,
        /// Single-leg ratio quantity (default 1).
        #[arg(long, default_value = "1", value_name = "RATIO")]
        ratio: String,
        /// Net order direction: `credit` or `debit`.
        #[arg(long, value_name = "DIR")]
        direction: Option<String>,
        /// Limit price (net premium).
        #[arg(long, value_name = "PRICE")]
        price: Option<String>,
        /// Time in force: day|gtc|ioc|fok.
        #[arg(long, value_name = "TIF")]
        tif: Option<String>,
        /// Order type: `limit` or `market`.
        #[arg(long = "type", value_name = "TYPE")]
        kind: Option<String>,
        /// Build and print the JSON-RPC request without sending it.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation prompt for a live order.
        #[arg(long)]
        force: bool,
    },
    /// Cancel an option order by id (WRITE).
    Cancel {
        /// Order id to cancel.
        id: String,
        /// Account to act on (required; never defaulted from config or env).
        #[arg(long, required = true, value_name = "ID")]
        account: String,
        /// Build and print the JSON-RPC request without sending it.
        #[arg(long)]
        dry_run: bool,
        /// Skip the confirmation prompt for a live cancel.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
enum PortfolioCmd {
    /// Portfolio snapshot for one account.
    Get {
        #[arg(long, required = true, value_name = "ID")]
        account: String,
    },
}

#[derive(Subcommand, Debug)]
enum PnlCmd {
    /// Realized/settled trade history.
    #[command(name = "trade-history")]
    TradeHistory {
        #[arg(long, required = true, value_name = "ID")]
        account: String,
        #[arg(long, value_name = "SPAN")]
        span: Option<String>,
        #[arg(long, value_name = "SYMBOL")]
        symbol: Option<String>,
        #[arg(long, value_name = "CURSOR")]
        cursor: Option<String>,
    },
    /// Realized P&L over a span or date range.
    Realized {
        #[arg(long, required = true, value_name = "ID")]
        account: String,
        #[arg(long, value_name = "SPAN")]
        span: Option<String>,
        #[arg(long = "start-date", value_name = "YYYY-MM-DD")]
        start_date: Option<String>,
        #[arg(long = "end-date", value_name = "YYYY-MM-DD")]
        end_date: Option<String>,
        #[arg(long = "display-currency", value_name = "CCY")]
        display_currency: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum MarginCmd {
    /// Limited-margin (equity) upgrade info.
    #[command(name = "equity-upgrade-info")]
    EquityUpgradeInfo {
        #[arg(long, required = true, value_name = "ID")]
        account: String,
    },
    /// Option-level upgrade info.
    #[command(name = "option-upgrade-info")]
    OptionUpgradeInfo {
        #[arg(long, required = true, value_name = "ID")]
        account: String,
    },
}

#[derive(Subcommand, Debug)]
enum EquityCmd {
    /// Fundamentals for one or more symbols.
    Fundamentals {
        #[arg(long, required = true, value_name = "SYM[,SYM...]")]
        symbols: String,
        #[arg(long, value_name = "BOUNDS")]
        bounds: Option<String>,
    },
    /// Historical price series.
    Historicals {
        #[arg(long, required = true, value_name = "SYM[,SYM...]")]
        symbols: String,
        #[arg(long = "start-time", required = true, value_name = "TS")]
        start_time: String,
        #[arg(long = "end-time", value_name = "TS")]
        end_time: Option<String>,
        #[arg(long, value_name = "INTERVAL")]
        interval: Option<String>,
        #[arg(long, value_name = "BOUNDS")]
        bounds: Option<String>,
        #[arg(long = "adjustment-type", value_name = "TYPE")]
        adjustment_type: Option<String>,
    },
    /// Level-2 price book.
    #[command(name = "price-book")]
    PriceBook {
        #[arg(long, required = true, value_name = "SYM[,SYM...]")]
        symbols: String,
    },
    /// Tax lots for a symbol in one account.
    #[command(name = "tax-lots")]
    TaxLots {
        #[arg(long, required = true, value_name = "ID")]
        account: String,
        #[arg(long, required = true, value_name = "SYMBOL")]
        symbol: String,
        #[arg(long, value_name = "CURSOR")]
        cursor: Option<String>,
    },
    /// Technical indicators for a symbol.
    Technicals {
        #[arg(long, required = true, value_name = "SYMBOL")]
        symbol: String,
        #[arg(long = "type", required = true, value_name = "INDICATOR")]
        kind: String,
        #[arg(long, required = true, value_name = "INTERVAL")]
        interval: String,
        #[arg(long = "start-time", required = true, value_name = "TS")]
        start_time: String,
        #[arg(long = "end-time", value_name = "TS")]
        end_time: Option<String>,
        #[arg(long, value_name = "OUTPUT")]
        output: Option<String>,
        #[arg(long, value_name = "N")]
        period: Option<i64>,
    },
    /// Tradability for symbols in one account.
    Tradability {
        #[arg(long, required = true, value_name = "ID")]
        account: String,
        #[arg(long, required = true, value_name = "SYM[,SYM...]")]
        symbols: String,
    },
}

#[derive(Subcommand, Debug)]
enum IndexCmd {
    /// List indexes.
    #[command(visible_alias = "ls")]
    List {
        #[arg(long, value_name = "SYMBOLS")]
        symbols: Option<String>,
    },
    /// Index quotes by instrument id.
    Quotes {
        #[arg(long = "instrument-ids", required = true, value_name = "ID[,ID...]")]
        instrument_ids: String,
    },
    /// Index historicals.
    Historicals {
        #[arg(long = "instrument-ids", required = true, value_name = "ID[,ID...]")]
        instrument_ids: String,
        #[arg(long = "start-time", required = true, value_name = "TS")]
        start_time: String,
        #[arg(long, required = true, value_name = "INTERVAL")]
        interval: String,
        #[arg(long = "end-time", value_name = "TS")]
        end_time: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum MarketCmd {
    /// Disclosed US politician trades (STOCK Act; ranges, lagged up to 45 days).
    #[command(name = "politician-trades")]
    PoliticianTrades {
        #[arg(long, value_name = "NAME")]
        politician: Option<String>,
        #[arg(long, value_name = "SYMBOL")]
        symbol: Option<String>,
    },
    /// The S&P 500 market mode under a named state metric (READ: index tools only).
    Regime {
        /// State metric codename (see `rhap metrics list`).
        #[arg(long, default_value = "regime/1", value_name = "NAME")]
        metric: String,
        /// Index symbol for trend/realized vol.
        #[arg(long, default_value = "SPX", value_name = "SYMBOL")]
        symbol: String,
        /// Implied-vol index symbol.
        #[arg(long = "vol-symbol", default_value = "VIX", value_name = "SYMBOL")]
        vol_symbol: String,
        /// Replay the metric over the last N sessions (stateful dwell/cooldown)
        /// and include the per-session series.
        ///
        /// Default 11, NOT 1. At 1 the replay classifies a single session with
        /// an empty raw history and a zero cooldown, so regime/2's asymmetric
        /// dwell cannot fire and regime/3's 10-session velocity cooldown can
        /// only be armed by today — the two rules those metrics exist for. On
        /// a tape with a -3.4% day four sessions back, `--history 1` returned
        /// full-bull handling (sector spreads, opening allowed) while
        /// `--history 11` returned velocity-exit (index only, no opening).
        /// The published cooldown is 10 sessions, so 11 is the minimum that
        /// can observe it.
        #[arg(long, default_value_t = 11, value_name = "N")]
        history: usize,
    },
    /// Earnings calendar.
    #[command(name = "earnings-calendar")]
    EarningsCalendar {
        #[arg(long = "start-date", value_name = "YYYY-MM-DD")]
        start_date: Option<String>,
        #[arg(long, value_name = "N")]
        days: Option<i64>,
        #[arg(long, value_name = "FILTER")]
        filter: Option<String>,
    },
    /// Earnings results for a symbol.
    #[command(name = "earnings-results")]
    EarningsResults {
        #[arg(long, required = true, value_name = "SYMBOL")]
        symbol: String,
    },
    /// Financial statements.
    Financials {
        #[arg(long, required = true, value_name = "SYM[,SYM...]")]
        symbols: String,
        #[arg(long, value_name = "PERIOD")]
        period: Option<String>,
        #[arg(long, value_name = "N")]
        limit: Option<i64>,
    },
}

// The `Add`/`Remove` variants carry several optional list flags and so dwarf the
// tiny read variants; boxing fights clap's derive and the enum is parsed once.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand, Debug)]
enum WatchlistsCmd {
    /// List watchlists.
    #[command(visible_alias = "ls")]
    List,
    /// Items in a watchlist.
    Items {
        #[arg(long = "list-id", required = true, value_name = "ID")]
        list_id: String,
    },
    /// Popular (curated) watchlists.
    Popular,
    /// Create a watchlist (WRITE).
    Create {
        #[arg(long = "display-name", required = true, value_name = "NAME")]
        display_name: String,
        #[arg(long = "icon-emoji", value_name = "EMOJI")]
        icon_emoji: Option<String>,
        #[arg(long, value_name = "TEXT")]
        description: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
    /// Update a watchlist's metadata (WRITE).
    Update {
        #[arg(long = "list-id", required = true, value_name = "ID")]
        list_id: String,
        #[arg(long = "display-name", value_name = "NAME")]
        display_name: Option<String>,
        #[arg(long = "icon-emoji", value_name = "EMOJI")]
        icon_emoji: Option<String>,
        #[arg(long, value_name = "TEXT")]
        description: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
    /// Add symbols/indexes to a watchlist (WRITE).
    Add {
        #[arg(long = "list-id", required = true, value_name = "ID")]
        list_id: String,
        #[arg(long, value_name = "SYM[,SYM...]")]
        symbols: Option<String>,
        #[arg(long = "index-ids", value_name = "ID[,ID...]")]
        index_ids: Option<String>,
        #[arg(long = "currency-pair-ids", value_name = "ID[,ID...]")]
        currency_pair_ids: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
    /// Remove symbols/indexes from a watchlist (WRITE).
    Remove {
        #[arg(long = "list-id", required = true, value_name = "ID")]
        list_id: String,
        #[arg(long, value_name = "SYM[,SYM...]")]
        symbols: Option<String>,
        #[arg(long = "index-ids", value_name = "ID[,ID...]")]
        index_ids: Option<String>,
        #[arg(long = "currency-pair-ids", value_name = "ID[,ID...]")]
        currency_pair_ids: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
    /// Add option(s) to the watchlist (WRITE).
    #[command(name = "add-option")]
    AddOption {
        #[arg(long = "option-ids", required = true, value_name = "ID[,ID...]")]
        option_ids: String,
        #[arg(long = "position-type", value_name = "TYPE")]
        position_type: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
    /// Remove option(s) from the watchlist (WRITE).
    #[command(name = "remove-option")]
    RemoveOption {
        #[arg(long = "option-ids", required = true, value_name = "ID[,ID...]")]
        option_ids: String,
        #[arg(long = "position-type", value_name = "TYPE")]
        position_type: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
    /// Follow a watchlist (WRITE).
    Follow {
        #[arg(long = "list-id", required = true, value_name = "ID")]
        list_id: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
    /// Unfollow a watchlist (WRITE).
    Unfollow {
        #[arg(long = "list-id", required = true, value_name = "ID")]
        list_id: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
}

#[allow(clippy::large_enum_variant)]
#[derive(Subcommand, Debug)]
enum ScansCmd {
    /// List saved scans.
    #[command(visible_alias = "ls")]
    List,
    /// The scanner filter specification.
    #[command(name = "filter-specs")]
    FilterSpecs,
    /// Run a scan and return matches.
    Run {
        #[arg(long = "scan-id", required = true, value_name = "ID")]
        scan_id: String,
    },
    /// Create a scan (WRITE).
    Create {
        #[arg(long = "scan-id", value_name = "ID")]
        scan_id: Option<String>,
        #[arg(long, value_name = "PRESET")]
        preset: Option<String>,
        #[arg(long, value_name = "TITLE")]
        title: Option<String>,
        #[arg(long, value_name = "JSON")]
        filters: Option<String>,
        #[arg(long, value_name = "JSON")]
        columns: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
    /// Update a scan's config (WRITE).
    #[command(name = "update-config")]
    UpdateConfig {
        #[arg(long = "scan-id", required = true, value_name = "ID")]
        scan_id: String,
        #[arg(long = "sorting-column", value_name = "COL")]
        sorting_column: Option<String>,
        #[arg(long = "sorting-direction", value_name = "DIR")]
        sorting_direction: Option<String>,
        #[arg(long, value_name = "JSON")]
        columns: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
    /// Update a scan's filters (WRITE).
    #[command(name = "update-filters")]
    UpdateFilters {
        #[arg(long = "scan-id", required = true, value_name = "ID")]
        scan_id: String,
        #[arg(long, required = true, value_name = "JSON")]
        filters: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
enum ConfigCmd {
    /// Print the resolved config file path.
    Path,
    /// Show the effective configuration.
    Show,
    /// Set a config key (client_id). Note: `account` is NOT a config key.
    Set { key: String, value: String },
    /// Remove a config key.
    Unset { key: String },
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(&cli) {
        std::process::exit(output::fail(&e, cli.common.json));
    }
}

fn run(cli: &Cli) -> Result<(), CliError> {
    // Commands needing neither config, keychain, nor network are answered first,
    // so `--help`, `completions` and `info` never prompt or hang.
    //
    // There is no `self-update`: it installed an unverified download and
    // re-signed it with the identity the keychain trusts (audit 2026-09-23,
    // security #5). A new binary comes from `make install`, with the owner at
    // the machine.
    match &cli.command {
        Command::Completions { shell } => {
            clap_complete::generate(*shell, &mut Cli::command(), BIN, &mut std::io::stdout());
            return Ok(());
        }
        Command::Info => return commands::misc::info(cli.common.json),
        Command::Metrics(MetricsCmd::List) => {
            return commands::score::metrics_list(cli.common.json)
        }
        _ => {}
    }

    // Everything checkable from the arguments alone is checked here, so bad
    // input fails the same way whether or not the machine has a session — and a
    // write verb typo never reaches the keychain or the network (SPEC v1 §1.2).
    validate(&cli.command)?;

    let store = ConfigStore::new(BIN).with_override(cli.config.clone());
    let cfg: Config = store.load()?;
    let creds = CredentialStore::for_binary(BIN);

    match &cli.command {
        Command::Config(cmd) => {
            commands::misc::config(&cli.common, &store, cfg, cmd_to_config_op(cmd))
        }
        Command::Auth(cmd) => auth_cmd(cli, cmd, &store, &creds, cfg),
        _ => {
            let ctx = Ctx {
                common: &cli.common,
                cfg: &cfg,
                creds: &creds,
            };
            match &cli.command {
                Command::Tools(cmd) => tools_cmd(&ctx, cmd),
                Command::News(NewsCmd::Get {
                    symbol,
                    limit,
                    cursor,
                }) => commands::extras::news(&ctx, symbol, *limit, cursor.as_deref()),
                Command::Sec(cmd) => sec_cmd(&ctx, cmd),
                Command::Alerts(cmd) => alerts_cmd(&ctx, cmd),
                Command::Crypto(cmd) => crypto_cmd(&ctx, cmd),
                Command::Ark(ArkCmd::Score {
                    metric,
                    window,
                    funds,
                    tickers,
                    top,
                    collateral,
                    base_url,
                }) => commands::ark::ark_score(
                    &ctx,
                    &commands::ark::ArkQuery {
                        metric: commands::ark::metric(metric)?,
                        window: *window,
                        funds: funds
                            .split(',')
                            .map(|s| s.trim().to_uppercase())
                            .filter(|s| !s.is_empty())
                            .collect(),
                        tickers: tickers.as_deref().map(|t| {
                            t.split(',')
                                .map(|s| s.trim().to_uppercase())
                                .filter(|s| !s.is_empty())
                                .collect()
                        }),
                        top: *top,
                        collateral: *collateral,
                        base_url,
                    },
                ),
                Command::Accounts(AccountsCmd::List) => commands::reads::accounts_list(&ctx),
                Command::Positions(PositionsCmd::List { account }) => {
                    commands::reads::positions_list(&ctx, account)
                }
                Command::Orders(OrdersCmd::List { account, since }) => {
                    commands::reads::orders_list(&ctx, account, since.as_deref())
                }
                Command::Quotes(QuotesCmd::Get { symbol }) => {
                    commands::reads::quotes_get(&ctx, symbol)
                }
                Command::Order(OrderCmd::Place {
                    account,
                    symbol,
                    side,
                    qty,
                    dollar_amount,
                    order_type,
                    limit,
                    market_hours,
                    tif,
                    dry_run,
                    force,
                }) => commands::order::place(
                    &ctx,
                    account,
                    symbol,
                    side,
                    qty.as_deref(),
                    dollar_amount.as_deref(),
                    order_type,
                    limit.as_deref(),
                    market_hours,
                    tif,
                    *dry_run,
                    *force,
                ),
                Command::Order(OrderCmd::Cancel {
                    id,
                    account,
                    dry_run,
                    force,
                }) => commands::order::cancel(&ctx, account, id, *dry_run, *force),
                Command::Order(OrderCmd::Advanced(cmd)) => advanced_cmd(&ctx, cmd),
                Command::Option(cmd) => option_cmd(&ctx, cmd),
                Command::Portfolio(PortfolioCmd::Get { account }) => {
                    commands::data::portfolio_get(&ctx, account)
                }
                Command::Pnl(cmd) => pnl_cmd(&ctx, cmd),
                Command::Margin(cmd) => margin_cmd(&ctx, cmd),
                Command::Equity(cmd) => equity_cmd(&ctx, cmd),
                Command::Index(cmd) => index_cmd(&ctx, cmd),
                Command::Market(cmd) => market_cmd(&ctx, cmd),
                Command::Search {
                    query,
                    asset_type,
                    limit,
                } => commands::data::search(&ctx, query, asset_type.as_deref(), *limit),
                Command::Watchlists(cmd) => watchlists_cmd(&ctx, cmd),
                Command::Scans(cmd) => scans_cmd(&ctx, cmd),
                Command::Call {
                    tool,
                    args,
                    dry_run,
                    force,
                } => commands::call::call(&ctx, tool, args.as_deref(), *dry_run, *force),
                // Dispatched above.
                Command::Auth(_)
                | Command::Config(_)
                | Command::Completions { .. }
                | Command::Info
                | Command::Metrics(_) => unreachable!("handled before session setup"),
            }
        }
    }
}

fn tools_cmd(ctx: &Ctx, cmd: &ToolsCmd) -> Result<(), CliError> {
    match cmd {
        ToolsCmd::List { schemas } => commands::tools::list(ctx, *schemas),
        ToolsCmd::Show { name } => commands::tools::show(ctx.common.json, name),
    }
}

fn pnl_cmd(ctx: &Ctx, cmd: &PnlCmd) -> Result<(), CliError> {
    match cmd {
        PnlCmd::TradeHistory {
            account,
            span,
            symbol,
            cursor,
        } => commands::data::pnl_trade_history(
            ctx,
            account,
            span.as_deref(),
            symbol.as_deref(),
            cursor.as_deref(),
        ),
        PnlCmd::Realized {
            account,
            span,
            start_date,
            end_date,
            display_currency,
        } => commands::data::pnl_realized(
            ctx,
            account,
            span.as_deref(),
            start_date.as_deref(),
            end_date.as_deref(),
            display_currency.as_deref(),
        ),
    }
}

fn margin_cmd(ctx: &Ctx, cmd: &MarginCmd) -> Result<(), CliError> {
    match cmd {
        MarginCmd::EquityUpgradeInfo { account } => {
            commands::data::margin_equity_upgrade(ctx, account)
        }
        MarginCmd::OptionUpgradeInfo { account } => {
            commands::data::margin_option_upgrade(ctx, account)
        }
    }
}

fn equity_cmd(ctx: &Ctx, cmd: &EquityCmd) -> Result<(), CliError> {
    match cmd {
        EquityCmd::Fundamentals { symbols, bounds } => {
            commands::data::equity_fundamentals(ctx, symbols, bounds.as_deref())
        }
        EquityCmd::Historicals {
            symbols,
            start_time,
            end_time,
            interval,
            bounds,
            adjustment_type,
        } => commands::data::equity_historicals(
            ctx,
            symbols,
            start_time,
            end_time.as_deref(),
            interval.as_deref(),
            bounds.as_deref(),
            adjustment_type.as_deref(),
        ),
        EquityCmd::PriceBook { symbols } => commands::data::equity_price_book(ctx, symbols),
        EquityCmd::TaxLots {
            account,
            symbol,
            cursor,
        } => commands::data::equity_tax_lots(ctx, account, symbol, cursor.as_deref()),
        EquityCmd::Technicals {
            symbol,
            kind,
            interval,
            start_time,
            end_time,
            output,
            period,
        } => commands::data::equity_technicals(
            ctx,
            symbol,
            kind,
            interval,
            start_time,
            end_time.as_deref(),
            output.as_deref(),
            *period,
        ),
        EquityCmd::Tradability { account, symbols } => {
            commands::data::equity_tradability(ctx, account, symbols)
        }
    }
}

fn index_cmd(ctx: &Ctx, cmd: &IndexCmd) -> Result<(), CliError> {
    match cmd {
        IndexCmd::List { symbols } => commands::data::index_list(ctx, symbols.as_deref()),
        IndexCmd::Quotes { instrument_ids } => commands::data::index_quotes(ctx, instrument_ids),
        IndexCmd::Historicals {
            instrument_ids,
            start_time,
            interval,
            end_time,
        } => commands::data::index_historicals(
            ctx,
            instrument_ids,
            start_time,
            interval,
            end_time.as_deref(),
        ),
    }
}

fn market_cmd(ctx: &Ctx, cmd: &MarketCmd) -> Result<(), CliError> {
    match cmd {
        MarketCmd::EarningsCalendar {
            start_date,
            days,
            filter,
        } => {
            commands::data::earnings_calendar(ctx, start_date.as_deref(), *days, filter.as_deref())
        }
        MarketCmd::EarningsResults { symbol } => commands::data::earnings_results(ctx, symbol),
        MarketCmd::PoliticianTrades { politician, symbol } => {
            commands::extras::politician_trades(ctx, politician.as_deref(), symbol.as_deref())
        }
        MarketCmd::Regime {
            metric,
            symbol,
            vol_symbol,
            history,
        } => commands::regime::market_regime(ctx, metric, symbol, vol_symbol, *history),
        MarketCmd::Financials {
            symbols,
            period,
            limit,
        } => commands::data::financials(ctx, symbols, period.as_deref(), *limit),
    }
}

fn watchlists_cmd(ctx: &Ctx, cmd: &WatchlistsCmd) -> Result<(), CliError> {
    use commands::watchlists as w;
    match cmd {
        WatchlistsCmd::List => w::list(ctx),
        WatchlistsCmd::Items { list_id } => w::items(ctx, list_id),
        WatchlistsCmd::Popular => w::popular(ctx),
        WatchlistsCmd::Create {
            display_name,
            icon_emoji,
            description,
            dry_run,
            force,
        } => w::create(
            ctx,
            display_name,
            icon_emoji.as_deref(),
            description.as_deref(),
            *dry_run,
            *force,
        ),
        WatchlistsCmd::Update {
            list_id,
            display_name,
            icon_emoji,
            description,
            dry_run,
            force,
        } => w::update(
            ctx,
            list_id,
            display_name.as_deref(),
            icon_emoji.as_deref(),
            description.as_deref(),
            *dry_run,
            *force,
        ),
        WatchlistsCmd::Add {
            list_id,
            symbols,
            index_ids,
            currency_pair_ids,
            dry_run,
            force,
        } => w::add(
            ctx,
            list_id,
            symbols.as_deref(),
            index_ids.as_deref(),
            currency_pair_ids.as_deref(),
            *dry_run,
            *force,
        ),
        WatchlistsCmd::Remove {
            list_id,
            symbols,
            index_ids,
            currency_pair_ids,
            dry_run,
            force,
        } => w::remove(
            ctx,
            list_id,
            symbols.as_deref(),
            index_ids.as_deref(),
            currency_pair_ids.as_deref(),
            *dry_run,
            *force,
        ),
        WatchlistsCmd::AddOption {
            option_ids,
            position_type,
            dry_run,
            force,
        } => w::add_option(ctx, option_ids, position_type.as_deref(), *dry_run, *force),
        WatchlistsCmd::RemoveOption {
            option_ids,
            position_type,
            dry_run,
            force,
        } => w::remove_option(ctx, option_ids, position_type.as_deref(), *dry_run, *force),
        WatchlistsCmd::Follow {
            list_id,
            dry_run,
            force,
        } => w::follow(ctx, list_id, *dry_run, *force),
        WatchlistsCmd::Unfollow {
            list_id,
            dry_run,
            force,
        } => w::unfollow(ctx, list_id, *dry_run, *force),
    }
}

fn scans_cmd(ctx: &Ctx, cmd: &ScansCmd) -> Result<(), CliError> {
    use commands::scans as s;
    match cmd {
        ScansCmd::List => s::list(ctx),
        ScansCmd::FilterSpecs => s::filter_specs(ctx),
        ScansCmd::Run { scan_id } => s::run(ctx, scan_id),
        ScansCmd::Create {
            scan_id,
            preset,
            title,
            filters,
            columns,
            dry_run,
            force,
        } => s::create(
            ctx,
            scan_id.as_deref(),
            preset.as_deref(),
            title.as_deref(),
            filters.as_deref(),
            columns.as_deref(),
            *dry_run,
            *force,
        ),
        ScansCmd::UpdateConfig {
            scan_id,
            sorting_column,
            sorting_direction,
            columns,
            dry_run,
            force,
        } => s::update_config(
            ctx,
            scan_id,
            sorting_column.as_deref(),
            sorting_direction.as_deref(),
            columns.as_deref(),
            *dry_run,
            *force,
        ),
        ScansCmd::UpdateFilters {
            scan_id,
            filters,
            dry_run,
            force,
        } => s::update_filters(ctx, scan_id, filters, *dry_run, *force),
    }
}

/// Dispatch the `option` subcommand group.
fn option_cmd(ctx: &Ctx, cmd: &OptionCmd) -> Result<(), CliError> {
    match cmd {
        OptionCmd::Positions(OptionPositionsCmd::List { account, nonzero }) => {
            commands::options::positions_list(ctx, account, *nonzero)
        }
        OptionCmd::Orders(OptionOrdersCmd::List {
            account,
            since,
            state,
            placed_agent,
        }) => commands::options::orders_list(
            ctx,
            account,
            since.as_deref(),
            state.as_deref(),
            placed_agent.as_deref(),
        ),
        OptionCmd::Quotes(OptionQuotesCmd::Get { instrument_ids }) => {
            commands::options::quotes_get(ctx, instrument_ids)
        }
        OptionCmd::Chains(OptionChainsCmd::Get { underlying }) => {
            commands::options::chains_get(ctx, underlying)
        }
        OptionCmd::Order(OptionOrderCmd::Place {
            account,
            quantity,
            legs,
            instrument_id,
            position_effect,
            side,
            ratio,
            direction,
            price,
            tif,
            kind,
            dry_run,
            force,
        }) => commands::options::order_place(
            ctx,
            account,
            quantity,
            legs.as_deref(),
            instrument_id.as_deref(),
            position_effect.as_deref(),
            side.as_deref(),
            ratio,
            direction.as_deref(),
            price.as_deref(),
            tif.as_deref(),
            kind.as_deref(),
            *dry_run,
            *force,
        ),
        OptionCmd::Order(OptionOrderCmd::Cancel {
            id,
            account,
            dry_run,
            force,
        }) => commands::options::order_cancel(ctx, account, id, *dry_run, *force),
        OptionCmd::Instruments(OptionInstrumentsCmd::List {
            chain_symbol,
            chain_id,
            expiration_dates,
            strike_price,
            kind,
            state,
            tradability,
            ids,
            cursor,
        }) => commands::options::instruments(
            ctx,
            chain_symbol.as_deref(),
            chain_id.as_deref(),
            expiration_dates.as_deref(),
            strike_price.as_deref(),
            kind.as_deref(),
            state.as_deref(),
            tradability.as_deref(),
            ids.as_deref(),
            cursor.as_deref(),
        ),
        OptionCmd::Historicals(OptionHistoricalsCmd::Get {
            instrument_ids,
            start_time,
            end_time,
            interval,
            bounds,
        }) => commands::options::historicals(
            ctx,
            instrument_ids,
            start_time,
            end_time.as_deref(),
            interval.as_deref(),
            bounds.as_deref(),
        ),
        OptionCmd::Watchlist => commands::options::watchlist(ctx),
        OptionCmd::Score {
            underlying,
            expiration,
            kind,
            width,
            class,
            metric,
            min_delta,
            max_delta,
            no_cushion,
            top,
        } => {
            let q = commands::score::ScoreQuery {
                underlying,
                expiration,
                is_put: kind.trim().eq_ignore_ascii_case("put"),
                width: *width,
                class: commands::score::Class::parse(class)?,
                metric: commands::score::metric(metric)?,
                min_delta: *min_delta,
                max_delta: *max_delta,
                cushion: !*no_cushion,
                top: *top,
            };
            commands::score::option_score(ctx, &q)
        }
        OptionCmd::Exercise {
            account,
            option_id,
            quantity,
            ref_id,
            reason,
            allow_shorts,
            dry_run,
            force,
        } => commands::options::exercise(
            ctx,
            account,
            option_id,
            *quantity,
            ref_id.as_deref(),
            reason.as_deref(),
            *allow_shorts,
            *dry_run,
            *force,
        ),
        OptionCmd::ExerciseCancel {
            option_id,
            account,
            dry_run,
            force,
        } => commands::options::exercise_cancel(ctx, account, option_id, *dry_run, *force),
    }
}

/// Dispatch the `sec` subcommand group.
fn sec_cmd(ctx: &Ctx, cmd: &SecCmd) -> Result<(), CliError> {
    use commands::extras as x;
    match cmd {
        SecCmd::Index {
            symbol,
            form_type,
            since,
            until,
            cursor,
        } => x::sec_index(
            ctx,
            symbol,
            form_type.as_deref(),
            since.as_deref(),
            until.as_deref(),
            cursor.as_deref(),
        ),
        SecCmd::Filing { filing_id, section } => x::sec_filing(ctx, filing_id, section.as_deref()),
        SecCmd::Facts {
            filing_ids,
            concepts,
        } => x::sec_facts(ctx, filing_ids, concepts),
        SecCmd::FactsCatalog {
            filing_id,
            concept_contains,
            axis_name_in,
            offset,
        } => x::sec_facts_catalog(
            ctx,
            filing_id,
            concept_contains.as_deref(),
            axis_name_in.as_deref(),
            *offset,
        ),
    }
}

/// Dispatch the `alerts` subcommand group.
fn alerts_cmd(ctx: &Ctx, cmd: &AlertsCmd) -> Result<(), CliError> {
    use commands::extras as x;
    match cmd {
        AlertsCmd::List {
            symbol,
            asset_class,
            cursor,
        } => x::alerts_list(
            ctx,
            symbol.as_deref(),
            asset_class.as_deref(),
            cursor.as_deref(),
        ),
        AlertsCmd::Log {
            asset_class,
            since,
            cursor,
            limit,
        } => x::alerts_log(
            ctx,
            asset_class.as_deref(),
            since.as_deref(),
            cursor.as_deref(),
            *limit,
        ),
        AlertsCmd::Create {
            symbol,
            condition_type,
            threshold,
            asset_class,
            indicator_json,
            dry_run,
            force,
        } => x::alerts_create(
            ctx,
            symbol,
            condition_type,
            threshold.as_deref(),
            asset_class.as_deref(),
            indicator_json.as_deref(),
            *dry_run,
            *force,
        ),
        AlertsCmd::Update {
            alert_id,
            enabled,
            condition_type,
            threshold,
            indicator_json,
            dry_run,
            force,
        } => x::alerts_update(
            ctx,
            alert_id,
            *enabled,
            condition_type.as_deref(),
            threshold.as_deref(),
            indicator_json.as_deref(),
            *dry_run,
            *force,
        ),
        AlertsCmd::Delete {
            alert_id,
            dry_run,
            force,
        } => x::alerts_delete(ctx, alert_id, *dry_run, *force),
        AlertsCmd::MarkRead {
            ids,
            all_through,
            dry_run,
            force,
        } => x::alerts_mark_read(
            ctx,
            ids.as_deref(),
            all_through.as_deref(),
            *dry_run,
            *force,
        ),
    }
}

/// Dispatch the `crypto` subcommand group.
fn crypto_cmd(ctx: &Ctx, cmd: &CryptoCmd) -> Result<(), CliError> {
    use commands::extras as x;
    match cmd {
        CryptoCmd::Onboarding => x::crypto_onboarding(ctx),
        CryptoCmd::Pairs { cursor, limit } => x::crypto_pairs(ctx, cursor.as_deref(), *limit),
        CryptoCmd::Quotes {
            symbols,
            timezone,
            account,
        } => x::crypto_quotes(ctx, symbols, timezone.as_deref(), account.as_deref()),
        CryptoCmd::Positions { account, cursor } => {
            x::crypto_positions(ctx, account, cursor.as_deref())
        }
        CryptoCmd::Order(CryptoOrderCmd::List {
            account,
            order_id,
            state,
            side,
            symbol,
            since,
            cursor,
        }) => x::crypto_orders(
            ctx,
            account,
            order_id.as_deref(),
            state.as_deref(),
            side.as_deref(),
            symbol.as_deref(),
            since.as_deref(),
            cursor.as_deref(),
        ),
        CryptoCmd::Order(CryptoOrderCmd::Preview(t)) => x::crypto_preview(ctx, &crypto_input(t)),
        CryptoCmd::Order(CryptoOrderCmd::Place {
            terms,
            ref_id,
            dry_run,
            force,
        }) => x::crypto_place(
            ctx,
            &crypto_input(terms),
            ref_id.as_deref(),
            *dry_run,
            *force,
        ),
        CryptoCmd::Order(CryptoOrderCmd::Cancel {
            order_id,
            account,
            dry_run,
            force,
        }) => x::crypto_cancel(ctx, account, order_id, *dry_run, *force),
    }
}

/// Dispatch the `order advanced` (OCO) subcommand group.
fn advanced_cmd(ctx: &Ctx, cmd: &AdvancedCmd) -> Result<(), CliError> {
    use commands::extras as x;
    match cmd {
        AdvancedCmd::List {
            account,
            order_id,
            contingency_type,
            since,
            cursor,
        } => x::advanced_list(
            ctx,
            account,
            order_id.as_deref(),
            contingency_type.as_deref(),
            since.as_deref(),
            cursor.as_deref(),
        ),
        AdvancedCmd::Review(t) => x::advanced_review(ctx, &oco_input(t)),
        AdvancedCmd::Place {
            terms,
            ref_id,
            dry_run,
            force,
        } => x::advanced_place(ctx, &oco_input(terms), ref_id.as_deref(), *dry_run, *force),
        AdvancedCmd::Cancel {
            order_id,
            account,
            dry_run,
            force,
        } => x::advanced_cancel(ctx, account, order_id, *dry_run, *force),
    }
}

fn oco_input(t: &OcoArgs) -> commands::extras::OcoInput<'_> {
    commands::extras::OcoInput {
        account: &t.account,
        symbol: &t.symbol,
        side: &t.side,
        quantity: &t.quantity,
        take_profit: &t.take_profit,
        stop_loss: &t.stop_loss,
        tif: t.tif.as_deref(),
    }
}

fn crypto_input(t: &CryptoOrderArgs) -> commands::extras::CryptoOrderInput<'_> {
    commands::extras::CryptoOrderInput {
        account: &t.account,
        symbol: &t.symbol,
        side: &t.side,
        kind: &t.kind,
        quantity: t.quantity.as_deref(),
        dollar_amount: t.dollar_amount.as_deref(),
        limit_price: t.limit_price.as_deref(),
        stop_price: t.stop_price.as_deref(),
        tif: t.tif.as_deref(),
    }
}

/// Argument checks that need nothing but the arguments themselves.
fn validate(command: &Command) -> Result<(), CliError> {
    match command {
        Command::Orders(OrdersCmd::List { since, .. }) => validate_since(since.as_deref()),
        Command::Order(OrderCmd::Place {
            side,
            qty,
            dollar_amount,
            order_type,
            limit,
            market_hours,
            tif,
            ..
        }) => {
            // The same validation `order::place` runs — one implementation, so
            // this early check cannot drift from it (it once lacked the
            // regular-hours rule).
            commands::order::validate_place(
                side,
                qty.as_deref(),
                dollar_amount.as_deref(),
                order_type,
                limit.as_deref(),
                market_hours,
                tif,
            )
            .map(|_| ())
        }
        Command::Option(OptionCmd::Orders(OptionOrdersCmd::List { since, .. })) => {
            validate_since(since.as_deref())
        }
        Command::Option(OptionCmd::Score {
            expiration,
            kind,
            width,
            class,
            metric,
            min_delta,
            max_delta,
            top,
            ..
        }) => {
            validate_since(Some(expiration))?;
            match kind.trim().to_lowercase().as_str() {
                "put" | "call" => {}
                other => {
                    return Err(CliError::Usage(format!(
                        "--type must be put or call (got `{other}`)"
                    )))
                }
            }
            if let Some(w) = width {
                if *w <= 0.0 || w.is_nan() {
                    return Err(CliError::Usage("--width must be positive".into()));
                }
            }
            if *min_delta <= 0.0 || *min_delta >= *max_delta || *max_delta >= 1.0 {
                return Err(CliError::Usage(
                    "delta band must satisfy 0 < --min-delta < --max-delta < 1".into(),
                ));
            }
            if *top == 0 {
                return Err(CliError::Usage("--top must be at least 1".into()));
            }
            commands::score::Class::parse(class)?;
            commands::score::metric_of_kind(metric, "score").map(|_| ())
        }
        Command::Ark(ArkCmd::Score {
            metric,
            window,
            top,
            ..
        }) => {
            if *window < 5 || *window > 250 {
                return Err(CliError::Usage("--window must be 5..=250 sessions".into()));
            }
            if *top == 0 {
                return Err(CliError::Usage("--top must be at least 1".into()));
            }
            commands::ark::metric(metric).map(|_| ())
        }
        Command::Order(OrderCmd::Advanced(AdvancedCmd::Place { terms, .. }))
        | Command::Order(OrderCmd::Advanced(AdvancedCmd::Review(terms))) => {
            commands::extras::validate_oco(&oco_input(terms))
        }
        Command::Crypto(CryptoCmd::Order(CryptoOrderCmd::Place { terms, .. }))
        | Command::Crypto(CryptoCmd::Order(CryptoOrderCmd::Preview(terms))) => {
            commands::extras::validate_crypto(&crypto_input(terms))
        }
        Command::Market(MarketCmd::Regime {
            metric, history, ..
        }) => {
            if *history == 0 || *history > 750 {
                return Err(CliError::Usage("--history must be 1..=750 sessions".into()));
            }
            commands::score::metric_of_kind(metric, "state").map(|_| ())
        }
        Command::Option(OptionCmd::Order(OptionOrderCmd::Place {
            quantity,
            legs,
            instrument_id,
            position_effect,
            side,
            ratio,
            direction,
            price,
            tif,
            kind,
            ..
        })) => {
            // Same fail-fast rule for the option write path, through the same
            // function `options::order_place` runs.
            commands::options::validate_place(
                quantity,
                legs.as_deref(),
                instrument_id.as_deref(),
                position_effect.as_deref(),
                side.as_deref(),
                ratio,
                direction.as_deref(),
                price.as_deref(),
                tif.as_deref(),
                kind.as_deref(),
            )
            .map(|_| ())
        }
        _ => Ok(()),
    }
}

/// Validate an optional ISO `--since` date without a config/keychain touch.
fn validate_since(since: Option<&str>) -> Result<(), CliError> {
    match since {
        Some(s) => pk_cli_core::dates::parse_iso(s).map(|_| ()),
        None => Ok(()),
    }
}

fn auth_cmd(
    cli: &Cli,
    cmd: &AuthCmd,
    store: &ConfigStore,
    creds: &CredentialStore,
    cfg: Config,
) -> Result<(), CliError> {
    match cmd {
        AuthCmd::Login(args) => commands::auth::login(
            &cli.common,
            store,
            creds,
            cfg,
            &commands::auth::LoginInput {
                port: args.port,
                overwrite: args.overwrite,
                no_open: args.no_open,
                no_verify: args.no_verify,
            },
        ),
        AuthCmd::Status(args) => commands::auth::status(&cli.common, creds, args.verify),
        AuthCmd::Logout(args) => commands::auth::logout(&cli.common, store, creds, args.forget),
        AuthCmd::SetCredential(args) => commands::auth::set_credential(&cli.common, creds, args),
    }
}

fn cmd_to_config_op(cmd: &ConfigCmd) -> commands::misc::ConfigOp<'_> {
    match cmd {
        ConfigCmd::Path => commands::misc::ConfigOp::Path,
        ConfigCmd::Show => commands::misc::ConfigOp::Show,
        ConfigCmd::Set { key, value } => commands::misc::ConfigOp::Set(key, value),
        ConfigCmd::Unset { key } => commands::misc::ConfigOp::Unset(key),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// clap's debug assertions (conflicting flags, duplicate names, a required
    /// arg with a default, …) run only when the tree is built — so build it.
    #[test]
    fn command_tree_is_valid() {
        Cli::command().debug_assert();
    }
}
