//! `rhap mcp`: reading the user's own Robinhood accounts as MCP tools (cli-common DESIGN.md
//! §1.9), for Claude Code, Claude Desktop and any MCP client.
//!
//! **This module serves reads only.** rhap can place orders; these tools cannot. A tool reaches
//! Robinhood only through [`Upstream`], a closed list of tools the catalog annotates read-only,
//! so no argument, cursor or result can name another. `src/connector/tests.rs` holds every
//! variant to [`ToolKind::Read`] and every request a tool sends to that list. Never add an order,
//! cancel, exercise, transfer or watchlist tool here, and never call `McpClient::write`.
//!
//! (`src/mcp.rs` is the other direction: rhap as a client of Robinhood's own MCP server.)
//!
//! The account is never ambient, here as on the command line: a tool that reads an account
//! takes it as a required argument, with no default and no setting.
//!
//! The session is read once per tool call and is not held between calls. Listing the tools
//! touches no credential. Results are what Robinhood returned: no capture of a live result is in
//! this repo, so the tools do not guess at row fields. Concise drops nulls, empties and links,
//! and neither format carries the server's `guide`, which is advice for a model that can trade.

use pk_cli_config::ConfigStore;
use pk_cli_core::dates::parse_iso;
use pk_cli_core::CliError;
use pk_cli_mcp::{
    parse_args, ListArgs, Page, ResponseFormat, ResumeRequest, Server, ToolDef, ToolError,
    ToolOutput, Toolset,
};
use pk_cli_secrets::CredentialStore;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

#[cfg(test)]
use crate::catalog::{self, ToolKind};
use crate::commands::mint_client;
use crate::config::Config;
use crate::mcp::McpClient;

pub const ACCOUNTS_TOOL: &str = "rhap_accounts";
pub const PORTFOLIO_TOOL: &str = "rhap_portfolio";
pub const POSITIONS_TOOL: &str = "rhap_positions";
pub const ORDERS_TOOL: &str = "rhap_orders";
pub const QUOTES_TOOL: &str = "rhap_quotes";

/// Robinhood's limit before a quote leaves out the last close.
const MAX_SYMBOLS: usize = 20;
/// How many of Robinhood's pages one call reads. Its page size is fixed and undocumented.
const MAX_PROVIDER_PAGES: usize = 10;
/// Where a list sits in a result holding more than one array.
const LIST_KEYS: [&str; 5] = [
    "results",
    "orders",
    "positions",
    "accounts",
    "advanced_orders",
];

/// Every Robinhood tool this module can call. Each is annotated read-only in the catalog, and a
/// variant is the only way to name a tool to [`Reader::read`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upstream {
    Accounts,
    Portfolio,
    RealizedPnl,
    EquityPositions,
    OptionPositions,
    CryptoPositions,
    EquityOrders,
    OptionOrders,
    CryptoOrders,
    AdvancedOrders,
    EquityQuotes,
}

impl Upstream {
    #[cfg(test)]
    const ALL: [Upstream; 11] = [
        Upstream::Accounts,
        Upstream::Portfolio,
        Upstream::RealizedPnl,
        Upstream::EquityPositions,
        Upstream::OptionPositions,
        Upstream::CryptoPositions,
        Upstream::EquityOrders,
        Upstream::OptionOrders,
        Upstream::CryptoOrders,
        Upstream::AdvancedOrders,
        Upstream::EquityQuotes,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Upstream::Accounts => "get_accounts",
            Upstream::Portfolio => "get_portfolio",
            Upstream::RealizedPnl => "get_realized_pnl",
            Upstream::EquityPositions => "get_equity_positions",
            Upstream::OptionPositions => "get_option_positions",
            Upstream::CryptoPositions => "get_crypto_positions",
            Upstream::EquityOrders => "get_equity_orders",
            Upstream::OptionOrders => "get_option_orders",
            Upstream::CryptoOrders => "get_crypto_orders",
            Upstream::AdvancedOrders => "get_advanced_orders",
            Upstream::EquityQuotes => "get_equity_quotes",
        }
    }
}

/// How a tool reaches Robinhood. [`Live`] is the real one; the tests answer from scrubbed
/// fixtures.
pub trait Source {
    /// One tool call's way to read Robinhood, which reads the session and holds it for that call.
    fn connect(&self) -> Result<Box<dyn Reader + '_>, CliError>;
}

/// Reading Robinhood. There is no other method.
pub trait Reader {
    fn read(&mut self, tool: Upstream, arguments: Value) -> Result<Value, CliError>;
}

impl Reader for McpClient {
    fn read(&mut self, tool: Upstream, arguments: Value) -> Result<Value, CliError> {
        McpClient::read(self, tool.name(), arguments)
    }
}

type Connect = Box<dyn Fn(&ConfigStore) -> Result<McpClient, CliError> + Send + Sync>;

/// Robinhood's MCP server, through the keychain's session.
pub struct Live {
    store: ConfigStore,
    connect: Connect,
}

impl Live {
    pub fn new(store: ConfigStore, creds: CredentialStore, verbose: bool) -> Self {
        Self {
            store,
            connect: Box::new(move |store| {
                let config: Config = store.load().map_err(|_| {
                    CliError::Other(
                        "the rhap config file cannot be read; ask the user to run `rhap config \
                         show` in a terminal and repair it, then call the tool again"
                            .into(),
                    )
                })?;
                mint_client(&config, &creds, verbose)
            }),
        }
    }
}

impl Source for Live {
    fn connect(&self) -> Result<Box<dyn Reader + '_>, CliError> {
        Ok(Box::new((self.connect)(&self.store)?))
    }
}

/// A provider failure as a tool error. No keychain text reaches the model.
fn tool_err(e: CliError) -> ToolError {
    match e {
        CliError::Keychain(_) => ToolError::other("the keychain could not be used").with_fix(
            if cfg!(target_os = "macos") {
                "Ask the user to allow `rhap` in the keychain prompt, or in Keychain Access, then \
                 call the tool again"
            } else {
                "Ask the user to unlock or start their desktop keyring, then call the tool again"
            },
        ),
        other => other.into(),
    }
}

// ---- what Robinhood answers ---------------------------------------------------------

/// The payload under a live result's `data`. Its sibling `guide` stays behind: it advises a
/// model on how to use Robinhood's own tools, including those that trade.
fn data_of(mut payload: Value) -> Value {
    match payload.get_mut("data") {
        Some(data) => data.take(),
        None => {
            if let Some(map) = payload.as_object_mut() {
                map.remove("guide");
            }
            payload
        }
    }
}

fn changed(tool: Upstream) -> ToolError {
    ToolError::upstream(format!(
        "Robinhood's {} answered in a shape this tool does not recognize",
        tool.name()
    ))
    .with_fix("Tell the user rhap may need updating; `rhap call` shows the raw answer")
}

/// One page of Robinhood's list: its rows, and the cursor of the page after it. A shape this
/// cannot place fails, since a false empty list would read as an empty account.
fn list_of(payload: Value, tool: Upstream) -> Result<(Vec<Value>, Option<String>), ToolError> {
    let has_data = payload.get("data").is_some();
    match data_of(payload) {
        Value::Array(rows) => Ok((rows, None)),
        Value::Object(mut map) => {
            let next = match map.get("next") {
                None | Some(Value::Null) => None,
                Some(Value::String(next)) => Some(cursor_from(next).ok_or_else(|| changed(tool))?),
                Some(_) => return Err(changed(tool)),
            };
            let named = LIST_KEYS
                .iter()
                .find(|key| map.get(**key).is_some_and(Value::is_array))
                .map(|key| key.to_string());
            let lone = || {
                let mut arrays = map.iter().filter(|(_, v)| v.is_array());
                match (arrays.next(), arrays.next()) {
                    // A list under another name: `data` vouches for it, and it holds rows.
                    (Some((key, Value::Array(rows))), None)
                        if has_data && !rows.is_empty() && rows.iter().all(Value::is_object) =>
                    {
                        Some(key.clone())
                    }
                    _ => None,
                }
            };
            let key = named.or_else(lone).ok_or_else(|| changed(tool))?;
            match map.remove(&key) {
                Some(Value::Array(rows)) => Ok((rows, next)),
                _ => Err(changed(tool)),
            }
        }
        _ => Err(changed(tool)),
    }
}

/// One answer that is not a list. An answer holding nothing, or still in its MCP envelope with no
/// text in it, is not a successful read of no figures.
fn single(payload: Value, tool: Upstream, detailed: bool) -> Result<Value, ToolError> {
    if payload.get("data").is_none() && payload.get("content").is_some() {
        return Err(changed(tool));
    }
    let data = data_of(payload);
    let brief = concise(data.clone());
    if !(brief.is_object() || brief.is_array()) || is_noise(&brief) {
        return Err(changed(tool));
    }
    Ok(if detailed { data } else { brief })
}

/// The `cursor` parameter of a page's `next` URL, if it is one a later call can send on.
fn cursor_from(next: &str) -> Option<String> {
    crate::commands::score::cursor_from_next(next).filter(|c| !c.is_empty() && cursor_is_plain(c))
}

/// A cursor comes back from the client and goes on to Robinhood, so its shape is checked both on
/// the way out and on the way back.
fn cursor_is_plain(cursor: &str) -> bool {
    cursor
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "=_-+/.%".contains(c))
}

/// Where a key says to read on. A Robinhood cursor marks a whole page, so a key must hold more
/// to resume just after its own row: the cursor that fetched the row's page, what the row is,
/// and how far in it was, which only tells apart rows that look the same.
#[derive(Debug, PartialEq, Eq)]
struct Place {
    cursor: Option<String>,
    row: Option<String>,
    index: usize,
}

impl Place {
    fn parse(key: &str) -> Result<Self, ToolError> {
        let foreign = || {
            ToolError::usage(
                "cursor is not one this tool returned; pass a result's next_cursor exactly as \
                 received, or omit cursor to start from the first page",
            )
        };
        let mut parts = key.split('|');
        let (index, row, cursor) = (parts.next(), parts.next(), parts.next());
        let (Some(index), Some(row), Some(cursor), None) = (index, row, cursor, parts.next())
        else {
            return Err(foreign());
        };
        let index = index.parse().map_err(|_| foreign())?;
        if !cursor_is_plain(cursor) || !row.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(foreign());
        }
        Ok(Self {
            cursor: Some(cursor.to_string()).filter(|c| !c.is_empty()),
            row: Some(row.to_string()).filter(|r| !r.is_empty()),
            index,
        })
    }

    /// The key that resumes just after `row`, the `index`th of the page `cursor` fetched.
    fn after_row(cursor: Option<&str>, row: &Value, index: usize) -> String {
        format!("{index}|{}|{}", identity(row), cursor.unwrap_or_default())
    }

    /// The key that starts the page `cursor` fetches.
    fn start_of(cursor: &str) -> String {
        format!("0||{cursor}")
    }

    /// Where in a re-read page to carry on: just after this place's row, wherever it has moved
    /// to. If it has gone there is no telling what was already returned, so the caller starts
    /// over rather than get a row twice or miss one.
    fn start_in(&self, rows: &[Value]) -> Result<usize, ToolError> {
        let Some(row) = &self.row else {
            return Ok(0);
        };
        rows.iter()
            .enumerate()
            .filter(|(_, r)| identity(r) == *row)
            .map(|(at, _)| at)
            .min_by_key(|at| at.abs_diff(self.index))
            .map(|at| at + 1)
            .ok_or_else(|| {
                ToolError::usage(
                    "the list changed where this read had reached, so the place is lost",
                )
                .with_fix("Call again without cursor to read from the start")
            })
    }
}

/// What a row is, as a fingerprint: its id if it has one, and otherwise the whole row. The rows'
/// fields are not documented, so a row with no id field reads as a new one if anything in it
/// changes, and the place is lost rather than guessed.
fn identity(row: &Value) -> String {
    let id = [
        "id",
        "order_id",
        "option_id",
        "instrument_id",
        "symbol",
        "account_number",
    ]
    .iter()
    .find_map(|key| row.get(key).and_then(Value::as_str));
    let text = id.map_or_else(|| row.to_string(), str::to_string);
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{hash:016x}")
}

/// Nulls, empty values and links are noise to a reader, at any depth.
fn concise(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, concise(v)))
                .filter(|(_, v)| !is_noise(v))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(concise).collect()),
        other => other,
    }
}

fn is_noise(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty() || s.starts_with("https://") || s.starts_with("http://"),
        Value::Array(a) => a.is_empty(),
        Value::Object(m) => m.is_empty(),
        _ => false,
    }
}

fn shaped(row: Value, detailed: bool) -> Value {
    if detailed {
        row
    } else {
        concise(row)
    }
}

// ---- arguments ----------------------------------------------------------------------

fn checked_account(account: &str) -> Result<String, ToolError> {
    let account = account.trim();
    let ok = !account.is_empty()
        && account.len() <= 40
        && account
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(account.to_string())
    } else {
        Err(ToolError::usage(
            "account must be an account number from rhap_accounts",
        ))
    }
}

/// A crypto tool wants the numeric `rhs_account_number`, where every other wants `account_number`.
fn checked_rhs_account(account: &str) -> Result<String, ToolError> {
    let account = checked_account(account)?;
    if account.chars().all(|c| c.is_ascii_digit()) {
        Ok(account)
    } else {
        Err(ToolError::usage(
            "kind crypto takes the account's numeric rhs_account_number from rhap_accounts, not \
             its account_number",
        ))
    }
}

fn checked_symbol(symbol: &str) -> Result<String, ToolError> {
    let symbol = symbol.trim().to_uppercase();
    let ok = !symbol.is_empty()
        && symbol.len() <= 16
        && symbol
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '/');
    if ok {
        Ok(symbol)
    } else {
        Err(ToolError::usage(format!("{symbol:?} is not a symbol")))
    }
}

fn optional(arg: &Option<String>) -> Option<&str> {
    arg.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PositionKind {
    #[default]
    Equity,
    Option,
    Crypto,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OrderKind {
    #[default]
    Equity,
    Option,
    Crypto,
    Advanced,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Span {
    Day,
    Week,
    Month,
    #[serde(rename = "3month")]
    ThreeMonth,
    Year,
    All,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AccountsArgs {
    #[serde(flatten)]
    pub list: ListArgs,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PortfolioArgs {
    /// An account number from rhap_accounts.
    pub account: String,
    /// Add realized profit and loss over this span.
    #[serde(default)]
    pub realized: Option<Span>,
    /// concise (default) or detailed, as on the list tools.
    #[serde(default)]
    pub response_format: ResponseFormat,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PositionsArgs {
    /// account_number from rhap_accounts. For kind crypto, its numeric rhs_account_number.
    pub account: String,
    /// equity (default), option or crypto.
    #[serde(default)]
    pub kind: PositionKind,
    #[serde(flatten)]
    pub list: ListArgs,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OrdersArgs {
    /// account_number from rhap_accounts. For kind crypto, its numeric rhs_account_number.
    pub account: String,
    /// equity (default), option, crypto or advanced.
    #[serde(default)]
    pub kind: OrderKind,
    /// One state, such as filled or cancelled. Not for advanced.
    #[serde(default)]
    pub state: Option<String>,
    /// One symbol. Equity and crypto only.
    #[serde(default)]
    pub symbol: Option<String>,
    /// Created on or after this UTC date, YYYY-MM-DD.
    #[serde(default)]
    pub since: Option<String>,
    #[serde(flatten)]
    pub list: ListArgs,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct QuotesArgs {
    /// Up to 20 stock symbols.
    pub symbols: Vec<String>,
    /// concise (default) or detailed, as on the list tools.
    #[serde(default)]
    pub response_format: ResponseFormat,
}

#[derive(Serialize)]
struct NoFilter {}

#[derive(Serialize)]
struct PositionsFilter<'a> {
    account: &'a str,
    kind: PositionKind,
}

#[derive(Serialize)]
struct OrdersFilter<'a> {
    account: &'a str,
    kind: OrderKind,
    state: Option<&'a str>,
    symbol: Option<&'a str>,
    since: Option<&'a str>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PortfolioOut {
    pub account: String,
    pub portfolio: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub realized: Option<Value>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct QuotesOut {
    pub quotes: Value,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct AccountListOut {
    pub account: String,
    pub kind: String,
    #[serde(flatten)]
    pub page: Page<Value>,
}

// ---- the toolset --------------------------------------------------------------------

/// The toolset `rhap mcp` serves.
pub struct RhapTools<S: Source> {
    source: S,
}

impl<S: Source> RhapTools<S> {
    pub fn new(source: S) -> Self {
        Self { source }
    }

    /// One list page from where `page` says to start, reading Robinhood's pages until it has
    /// one row past the limit or a page bound. Every row's key resumes immediately after it.
    fn list(
        &self,
        tool: Upstream,
        arguments: Map<String, Value>,
        page: &ResumeRequest,
        detailed: bool,
        narrow: &str,
    ) -> Result<Page<Value>, ToolError> {
        let mut place = page.resume().map(Place::parse).transpose()?;
        let mut at = place.as_ref().and_then(|p| p.cursor.clone());
        let mut reader = self.source.connect().map_err(tool_err)?;
        let wanted = page.limit() + 1;
        let mut keyed: Vec<(String, Value)> = Vec::new();
        let mut rest = None;
        for _ in 0..MAX_PROVIDER_PAGES {
            let mut asked = arguments.clone();
            if let Some(cursor) = &at {
                asked.insert("cursor".into(), json!(cursor));
            }
            let payload = reader.read(tool, Value::Object(asked)).map_err(tool_err)?;
            let (rows, next) = list_of(payload, tool)?;
            if next.is_some() && next == at {
                return Err(ToolError::upstream(format!(
                    "Robinhood's {} named its own page as the next one",
                    tool.name()
                )));
            }
            let start = match place.take() {
                Some(place) => place.start_in(&rows)?,
                None => 0,
            };
            for (index, row) in rows.into_iter().enumerate().skip(start) {
                let key = Place::after_row(at.as_deref(), &row, index);
                keyed.push((key, shaped(row, detailed)));
            }
            rest = next.as_deref().map(Place::start_of);
            at = next;
            if at.is_none() || keyed.len() >= wanted {
                break;
            }
        }
        page.page(keyed, rest, narrow)
    }

    fn accounts(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: AccountsArgs = parse_args(args)?;
        let page = ResumeRequest::new(&args.list, &NoFilter {})?;
        let detailed = args.list.format().is_detailed();
        ToolOutput::new(&self.list(Upstream::Accounts, Map::new(), &page, detailed, "limit")?)
    }

    fn portfolio(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: PortfolioArgs = parse_args(args)?;
        let account = checked_account(&args.account)?;
        let mut reader = self.source.connect().map_err(tool_err)?;
        let portfolio = reader
            .read(Upstream::Portfolio, json!({ "account_number": account }))
            .map_err(tool_err)?;
        let detailed = args.response_format.is_detailed();
        let realized = args
            .realized
            .map(|span| {
                reader
                    .read(
                        Upstream::RealizedPnl,
                        json!({ "account_number": account, "span": span }),
                    )
                    .map_err(tool_err)
            })
            .transpose()?;
        ToolOutput::new(&PortfolioOut {
            account,
            portfolio: single(portfolio, Upstream::Portfolio, detailed)?,
            realized: realized
                .map(|r| single(r, Upstream::RealizedPnl, detailed))
                .transpose()?,
        })
    }

    fn positions(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: PositionsArgs = parse_args(args)?;
        let account = match args.kind {
            PositionKind::Crypto => checked_rhs_account(&args.account)?,
            _ => checked_account(&args.account)?,
        };
        let page = ResumeRequest::new(
            &args.list,
            &PositionsFilter {
                account: &account,
                kind: args.kind,
            },
        )?;
        let (tool, kind, arguments) = match args.kind {
            PositionKind::Equity => (
                Upstream::EquityPositions,
                "equity",
                json_map(json!({ "account_number": account })),
            ),
            PositionKind::Option => (
                Upstream::OptionPositions,
                "option",
                json_map(json!({ "account_number": account, "nonzero": true })),
            ),
            PositionKind::Crypto => (
                Upstream::CryptoPositions,
                "crypto",
                json_map(json!({ "rhs_account_number": account })),
            ),
        };
        let detailed = args.list.format().is_detailed();
        ToolOutput::new(&AccountListOut {
            page: self.list(tool, arguments, &page, detailed, "kind or limit")?,
            account,
            kind: kind.to_string(),
        })
    }

    fn orders(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: OrdersArgs = parse_args(args)?;
        let account = match args.kind {
            OrderKind::Crypto => checked_rhs_account(&args.account)?,
            _ => checked_account(&args.account)?,
        };
        let state = optional(&args.state);
        if let Some(state) = state {
            if !state.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                return Err(ToolError::usage(format!("{state:?} is not an order state")));
            }
        }
        let symbol = optional(&args.symbol).map(checked_symbol).transpose()?;
        let since = optional(&args.since);
        if let Some(since) = since {
            parse_iso(since).map_err(|_| {
                ToolError::usage(format!("since must be a date, YYYY-MM-DD; got {since:?}"))
            })?;
        }
        // Robinhood's tools take different filters, so a filter one cannot honor is refused
        // rather than silently ignored.
        if args.kind == OrderKind::Advanced && state.is_some() {
            return Err(ToolError::usage("advanced orders do not filter by state"));
        }
        if matches!(args.kind, OrderKind::Advanced | OrderKind::Option) && symbol.is_some() {
            return Err(ToolError::usage(
                "only equity and crypto orders filter by symbol",
            ));
        }
        let page = ResumeRequest::new(
            &args.list,
            &OrdersFilter {
                account: &account,
                kind: args.kind,
                state,
                symbol: symbol.as_deref(),
                since,
            },
        )?;
        let (tool, kind, account_key) = match args.kind {
            OrderKind::Equity => (Upstream::EquityOrders, "equity", "account_number"),
            OrderKind::Option => (Upstream::OptionOrders, "option", "account_number"),
            OrderKind::Crypto => (Upstream::CryptoOrders, "crypto", "rhs_account_number"),
            OrderKind::Advanced => (Upstream::AdvancedOrders, "advanced", "account_number"),
        };
        let mut arguments = Map::new();
        arguments.insert(account_key.into(), json!(account));
        if let Some(state) = state {
            arguments.insert("state".into(), json!(state));
        }
        if let Some(symbol) = &symbol {
            arguments.insert("symbol".into(), json!(symbol));
        }
        if let Some(since) = since {
            arguments.insert("created_at_gte".into(), json!(since));
        }
        let detailed = args.list.format().is_detailed();
        ToolOutput::new(&AccountListOut {
            page: self.list(tool, arguments, &page, detailed, "state, symbol or since")?,
            account,
            kind: kind.to_string(),
        })
    }

    fn quotes(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: QuotesArgs = parse_args(args)?;
        if args.symbols.is_empty() || args.symbols.len() > MAX_SYMBOLS {
            return Err(ToolError::usage(format!(
                "pass between 1 and {MAX_SYMBOLS} symbols"
            )));
        }
        let symbols = args
            .symbols
            .iter()
            .map(|s| checked_symbol(s))
            .collect::<Result<Vec<_>, _>>()?;
        let mut reader = self.source.connect().map_err(tool_err)?;
        let payload = reader
            .read(Upstream::EquityQuotes, json!({ "symbols": symbols }))
            .map_err(tool_err)?;
        // A list that is there and empty: no such quote, and not a changed answer.
        let data = payload.get("data");
        let no_match = LIST_KEYS
            .iter()
            .filter_map(|key| data?.get(key)?.as_array())
            .any(Vec::is_empty);
        if no_match {
            return Err(ToolError::not_found(format!(
                "Robinhood has no quote for {}",
                symbols.join(", ")
            ))
            .with_fix("Check the symbols with the user"));
        }
        ToolOutput::new(&QuotesOut {
            quotes: single(
                payload,
                Upstream::EquityQuotes,
                args.response_format.is_detailed(),
            )?,
        })
    }
}

fn json_map(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

impl<S: Source + Send + Sync> Toolset for RhapTools<S> {
    fn server(&self) -> Server {
        Server::new("rhap", env!("CARGO_PKG_VERSION"), "Robinhood", "rhap").instructions(
            "These tools only read: no tool can place, change or cancel an order, exercise an \
             option, move money or change a watchlist, and a request to do any of that is one to \
             hand back to the user. Start with rhap_accounts. Every other account tool needs an \
             account number passed each time, with no default, so ask which one when several \
             fit. Figures are Robinhood's, passed through unchanged, and are not advice. \
             Unofficial; not affiliated with Robinhood.",
        )
    }

    fn tools(&self) -> Vec<ToolDef> {
        vec![
            ToolDef::read(
                ACCOUNTS_TOOL,
                "Robinhood accounts",
                "Lists the user's brokerage accounts. Use it first, for the account numbers the \
                 other tools need. Returns items as Robinhood sends them, including \
                 account_number and rhs_account_number. Concise drops nulls, empty values and \
                 links; response_format detailed keeps them.",
            )
            .input::<AccountsArgs>()
            .output::<Page<Value>>(),
            ToolDef::read(
                PORTFOLIO_TOOL,
                "A Robinhood account's portfolio",
                "Reads one account's market value by asset type and its buying power. Use it for \
                 what an account is worth or can spend; pass realized for its realized profit and \
                 loss over a span. Returns account, portfolio and realized as Robinhood sends \
                 them. Concise drops nulls, empty values and links; response_format detailed keeps \
                 them.",
            )
            .input::<PortfolioArgs>()
            .output::<PortfolioOut>(),
            ToolDef::read(
                POSITIONS_TOOL,
                "A Robinhood account's positions",
                "Lists one account's open positions of one kind. Use it for what the user holds, \
                 once per kind for all of it; kind crypto takes the numeric rhs_account_number. \
                 Returns account, kind and items as Robinhood sends them. Concise drops nulls, \
                 empty values and links; response_format detailed keeps them.",
            )
            .input::<PositionsArgs>()
            .output::<AccountListOut>(),
            ToolDef::read(
                ORDERS_TOOL,
                "A Robinhood account's orders",
                "Lists one account's orders of one kind, newest first, open and closed. Use it \
                 for what was bought or sold and whether it filled, once per kind for all of them; \
                 kind crypto takes the numeric rhs_account_number. Returns account, kind and items \
                 as Robinhood sends them. Concise drops nulls, empty values and links; \
                 response_format detailed keeps them. Narrow with state, symbol or since.",
            )
            .input::<OrdersArgs>()
            .output::<AccountListOut>(),
            ToolDef::read(
                QUOTES_TOOL,
                "Robinhood stock quotes",
                "Reads current quotes and the last session's close for stock symbols. Use it to \
                 value a position, since positions carry no price. Returns quotes as Robinhood \
                 sends them. Concise drops nulls, empty values and links; response_format detailed \
                 keeps them.",
            )
            .input::<QuotesArgs>()
            .output::<QuotesOut>(),
        ]
    }

    async fn call(&self, name: &str, args: Value) -> Result<ToolOutput, ToolError> {
        match name {
            ACCOUNTS_TOOL => self.accounts(args),
            PORTFOLIO_TOOL => self.portfolio(args),
            POSITIONS_TOOL => self.positions(args),
            ORDERS_TOOL => self.orders(args),
            QUOTES_TOOL => self.quotes(args),
            other => Err(ToolError::usage(format!("no tool named {other}"))),
        }
    }
}

#[cfg(test)]
mod tests;
