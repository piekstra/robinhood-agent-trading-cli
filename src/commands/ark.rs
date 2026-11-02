//! `ark` — the `ark/1` and `ark/2` ticker metrics: how strongly ARK Invest is accumulating
//! a name and whether today's price is near what ARK paid.
//!
//! Data: daily holdings for the six ARK ETFs (ARKK, ARKW, ARKQ, ARKG, ARKF,
//! ARKX) from the public `arkfunds.io` API (which mirrors ARK's own daily
//! holdings CSVs). Day-over-day share differences are ARK's trades: a
//! positive difference on a date is a buy of that many shares. Prices come
//! from the MCP (`get_equity_quotes`, `get_equity_historicals`), so ARK's
//! buy VWAP for a name is Σ(shares bought on day d × close on d) / Σ shares.
//!
//! Published definition of `ark/1` (0–100; immutable — a change is `ark/2`):
//!
//! | Factor | Points | Measure |
//! | --- | --- | --- |
//! | accumulation | 35 | net shares bought over the window as % of ARK's current holding: ≥ +20% full, +10% → 25, +5% → 15, +1% → 8, ≤ 0 → 0 (linear between) |
//! | persistence | 20 | buy-days ÷ sessions in the window: ≥ 50% full, ≥ 30% → 12, ≥ 15% → 6 |
//! | recency | 10 | net buying in the last 5 sessions → 10; last 10 → 5 |
//! | price_proximity | 25 | price ÷ ARK buy VWAP: ≤ 1.00 full, ≤ 1.05 → 18, ≤ 1.10 → 10, ≤ 1.20 → 4, else 0 |
//! | conviction | 10 | the name's largest weight in any ARK fund: ≥ 3% full, ≥ 1.5% → 6, ≥ 0.5% → 3 |
//!
//! **`ark/2`** — the same five factors and points, with three measurement
//! repairs. `ark/1` is left exactly as published so its history stays
//! comparable:
//!
//! | | `ark/1` | `ark/2` |
//! | --- | --- | --- |
//! | initiation baseline | a position whose first row is later than the first date of ANY fund's file counts from zero — so one late fund file makes every holding in it look like an initiation | later than the first date of ITS OWN fund's file |
//! | calendar for persistence and recency | every date reported by any fund | the dates reported by the funds that hold the ticker |
//! | recency, 10-session tier | net buying over the last 10 | net buying over the last 10 AND the last 5 not net selling |
//!
//! `wheel_feasible` is a flag beside the score, not part of it: 100 × 0.9 ×
//! price ≤ `--collateral`. Where a 2× single-stock ETF exists for the name,
//! it is listed as an alternative vehicle (the venture screens its options
//! separately with `option score`).

use std::collections::{BTreeMap, BTreeSet};

use pk_cli_core::{output, CliError};
use serde_json::{json, Value};

use super::score::{f, metric_of_kind, results_of, rfc3339_days_ago, MetricDef};
use super::Ctx;

/// The six ARK active ETFs.
pub const FUNDS: &[&str] = &["ARKK", "ARKW", "ARKQ", "ARKG", "ARKF", "ARKX"];

/// Known 2× single-stock ETF vehicles for common ARK names (a convenience
/// map, not a data source; missing entries mean "name only").
pub const TWO_X: &[(&str, &str)] = &[
    ("TSLA", "TSLL"),
    ("COIN", "CONL"),
    ("NVDA", "NVDL"),
    ("AMZN", "AMZU"),
    ("META", "METU"),
    ("MSFT", "MSFU"),
    ("PLTR", "PLTU"),
    ("MSTR", "MSTU"),
    ("GOOGL", "GGLL"),
    ("AMD", "AMDL"),
    ("CBRS", "CBRG"),
    ("AVGO", "AVL"),
    ("RKLB", "RKLX"),
    ("HOOD", "HOOX"),
    ("SHOP", "SHPU"),
];

/// One holdings row: a fund's position in a ticker on a date.
#[derive(Clone, Debug, PartialEq)]
pub struct HoldingRow {
    pub fund: String,
    pub date: String,
    pub ticker: String,
    pub shares: f64,
    pub weight: f64,
}

/// Per-ticker activity derived from holdings history.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Activity {
    pub ticker: String,
    /// Σ over funds of (last shares − first shares) in the window.
    pub net_shares: f64,
    /// Sell days, mirroring `buys` — recency is a NET measure and needs both.
    pub sells: Vec<(String, f64)>,
    /// Σ over funds of current shares.
    pub held: f64,
    /// (date, shares bought) for every positive day-over-day diff, all funds.
    pub buys: Vec<(String, f64)>,
    /// Sessions (distinct dates) in the window: every fund's under `ark/1`,
    /// the holding funds' under `ark/2`.
    pub sessions: usize,
    /// Distinct buy dates.
    pub buy_days: usize,
    /// Largest current weight (%) in any fund.
    pub max_weight: f64,
    /// The same calendar as `sessions`, ascending (for recency).
    pub dates: Vec<String>,
}

/// Normalize ARK's ticker spellings (`RKLB UQ` → `RKLB`).
pub fn norm_ticker(raw: &str) -> String {
    raw.split_whitespace()
        .next()
        .unwrap_or("")
        .trim()
        .to_uppercase()
}

/// Parse an `arkfunds.io` holdings payload into rows.
pub fn parse_holdings(payload: &Value) -> Vec<HoldingRow> {
    payload
        .get("holdings")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    let ticker = norm_ticker(r.get("ticker").and_then(Value::as_str)?);
                    if ticker.is_empty() {
                        return None;
                    }
                    Some(HoldingRow {
                        fund: r.get("fund").and_then(Value::as_str)?.to_string(),
                        date: r.get("date").and_then(Value::as_str)?.to_string(),
                        ticker,
                        shares: f(r, "shares"),
                        weight: f(r, "weight"),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Which date a position must be present on to count as existing at the
/// start of the window (otherwise its baseline is zero: an initiation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Baseline {
    /// The earliest date reported by ANY fund (`ark/1`).
    AnyFundFirstDate,
    /// The earliest date reported by the position's own fund (`ark/2`).
    OwnFundFirstDate,
}

/// The session calendar behind persistence and recency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Calendar {
    /// Every date reported by any fund (`ark/1`).
    AllFunds,
    /// The dates reported by the funds that hold the ticker (`ark/2`).
    HoldingFunds,
}

/// When the 10-session recency tier (5 points) is granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecencyTier {
    /// Net buying over the last 10 sessions (`ark/1`).
    NetTen,
    /// …and the last 5 sessions not net selling (`ark/2`).
    NetTenUnlessLastFiveSelling,
}

/// The measurement rules of one published `ark/N`. The code reads the rule,
/// never the version number, so `ark/3` is one new arm in [`ArkVersion::of`]
/// and [`ArkVersion::rules`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArkRules {
    pub baseline: Baseline,
    pub calendar: Calendar,
    pub recency_tier: RecencyTier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArkVersion {
    V1,
    V2,
}

impl ArkVersion {
    /// A registered ticker metric with no arm here is a usage error — it must
    /// never fall through and compute an older definition under a new label.
    pub fn of(name: &str) -> Result<Self, CliError> {
        match name {
            "ark/1" => Ok(Self::V1),
            "ark/2" => Ok(Self::V2),
            other => Err(CliError::Usage(format!(
                "ticker metric {other} is registered but has no ark implementation"
            ))),
        }
    }

    pub fn rules(self) -> ArkRules {
        match self {
            Self::V1 => ArkRules {
                baseline: Baseline::AnyFundFirstDate,
                calendar: Calendar::AllFunds,
                recency_tier: RecencyTier::NetTen,
            },
            Self::V2 => ArkRules {
                baseline: Baseline::OwnFundFirstDate,
                calendar: Calendar::HoldingFunds,
                recency_tier: RecencyTier::NetTenUnlessLastFiveSelling,
            },
        }
    }
}

/// Aggregate holdings rows (any funds, any dates) into per-ticker activity.
pub fn activity(rows: &[HoldingRow], rules: &ArkRules) -> BTreeMap<String, Activity> {
    // (fund, ticker) -> date -> (shares, weight)
    let mut series: BTreeMap<(String, String), BTreeMap<String, (f64, f64)>> = BTreeMap::new();
    for r in rows {
        series
            .entry((r.fund.clone(), r.ticker.clone()))
            .or_default()
            .insert(r.date.clone(), (r.shares, r.weight));
    }
    // Each FUND's own first reported date. "Did this position exist at the
    // start?" must be asked of the fund's own file, not the global calendar:
    // comparing against the global first date meant one late fund file, or one
    // malformed row, made every position in that fund look like an initiation
    // — a flat 500-share holding scored accumulation 35/35 and sorted first.
    let mut all_dates: BTreeSet<&str> = BTreeSet::new();
    let mut fund_first: BTreeMap<String, String> = BTreeMap::new();
    for r in rows {
        let e = fund_first
            .entry(r.fund.clone())
            .or_insert_with(|| r.date.clone());
        if r.date < *e {
            *e = r.date.clone();
        }
    }
    // ONE calendar per ticker: the sessions reported by the funds that hold
    // it. Persistence (buy days / sessions) and the recency cutoffs read this,
    // and the baseline above reads the same funds' first dates. On the global
    // calendar a ticker held only by a late-starting fund had its buy days
    // divided by sessions that fund never reported.
    let mut fund_dates: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for r in rows {
        fund_dates.entry(&r.fund).or_default().insert(&r.date);
        all_dates.insert(&r.date);
    }
    let mut ticker_dates: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (fund, ticker) in series.keys() {
        if let Some(d) = fund_dates.get(fund.as_str()) {
            ticker_dates
                .entry(ticker)
                .or_default()
                .extend(d.iter().copied());
        }
    }
    let mut out: BTreeMap<String, Activity> = BTreeMap::new();
    for ((fund, ticker), by_date) in &series {
        let a = out.entry(ticker.clone()).or_insert_with(|| {
            let calendar = match rules.calendar {
                Calendar::HoldingFunds => ticker_dates.get(ticker.as_str()),
                Calendar::AllFunds => Some(&all_dates),
            };
            let dates: Vec<String> = calendar
                .map(|d| d.iter().map(|x| x.to_string()).collect())
                .unwrap_or_default();
            Activity {
                ticker: ticker.clone(),
                sessions: dates.len(),
                dates,
                ..Default::default()
            }
        });
        let dates: Vec<&String> = by_date.keys().collect();
        if let (Some(first), Some(last)) = (dates.first(), dates.last()) {
            let (s0, _) = by_date[*first];
            let (s1, w1) = by_date[*last];
            // A position whose FIRST row is later than the window's own start
            // did not exist at the start: its baseline is ZERO, not its first
            // observed holding. Measured before this fix: ARK initiating 500k
            // shares mid-window scored 10.0/100 — the strongest possible
            // signal reading as none — while an ARKK +100k / ARKW -100k
            // rebalance that moved no net shares scored 36.0.
            let window_start = match rules.baseline {
                Baseline::OwnFundFirstDate => fund_first.get(fund).map(String::as_str),
                Baseline::AnyFundFirstDate => all_dates.first().copied(),
            };
            let existed_at_start = window_start.map(|d| d == first.as_str()).unwrap_or(false);
            let base = if existed_at_start { s0 } else { 0.0 };
            a.net_shares += s1 - base;
            a.held += s1;
            a.max_weight = a.max_weight.max(w1);
        }
        for w in dates.windows(2) {
            let d = by_date[w[1]].0 - by_date[w[0]].0;
            if d > 0.0 {
                a.buys.push((w[1].clone(), d));
            } else if d < 0.0 {
                a.sells.push((w[1].clone(), -d));
            }
        }
    }
    for a in out.values_mut() {
        let mut days: Vec<&String> = a.buys.iter().map(|(d, _)| d).collect();
        days.sort();
        days.dedup();
        a.buy_days = days.len();
    }
    out
}

/// ARK's buy VWAP for a name from (date, shares) buys and daily closes.
pub fn buy_vwap(buys: &[(String, f64)], closes: &BTreeMap<String, f64>) -> Option<f64> {
    let (mut num, mut den) = (0.0, 0.0);
    for (d, sh) in buys {
        if let Some(c) = closes.get(d) {
            num += c * sh;
            den += sh;
        }
    }
    (den > 0.0).then_some(num / den)
}

/// A scored factor.
#[derive(Clone, Debug, PartialEq)]
pub struct Factor {
    pub name: &'static str,
    pub points: f64,
    pub max: f64,
    pub detail: String,
}

/// `ark/1` for one ticker.
#[cfg(test)]
pub fn ark_1(a: &Activity, price: Option<f64>, vwap: Option<f64>) -> (f64, Vec<Factor>) {
    ark_score_of(a, price, vwap, &ArkVersion::V1.rules())
}

/// `ark/2`: see the module table. `a` must come from `activity` under the
/// same rules.
#[cfg(test)]
pub fn ark_2(a: &Activity, price: Option<f64>, vwap: Option<f64>) -> (f64, Vec<Factor>) {
    ark_score_of(a, price, vwap, &ArkVersion::V2.rules())
}

fn ark_score_of(
    a: &Activity,
    price: Option<f64>,
    vwap: Option<f64>,
    rules: &ArkRules,
) -> (f64, Vec<Factor>) {
    let mut fs = Vec::with_capacity(5);
    // accumulation
    let acc_pct = if a.held > 0.0 {
        a.net_shares / a.held * 100.0
    } else {
        0.0
    };
    let pts = if acc_pct <= 0.0 {
        0.0
    } else if acc_pct < 1.0 {
        8.0 * acc_pct
    } else if acc_pct < 5.0 {
        8.0 + 7.0 * (acc_pct - 1.0) / 4.0
    } else if acc_pct < 10.0 {
        15.0 + 10.0 * (acc_pct - 5.0) / 5.0
    } else if acc_pct < 20.0 {
        25.0 + 10.0 * (acc_pct - 10.0) / 10.0
    } else {
        35.0
    };
    fs.push(Factor {
        name: "accumulation",
        points: pts,
        max: 35.0,
        detail: format!(
            "net {:+.0} shares = {:+.1}% of {:.0} held",
            a.net_shares, acc_pct, a.held
        ),
    });
    // persistence
    let ratio = if a.sessions > 0 {
        a.buy_days as f64 / a.sessions as f64
    } else {
        0.0
    };
    let pts = if ratio >= 0.5 {
        20.0
    } else if ratio >= 0.3 {
        12.0
    } else if ratio >= 0.15 {
        6.0
    } else {
        0.0
    };
    fs.push(Factor {
        name: "persistence",
        points: pts,
        max: 20.0,
        detail: format!("{} buy-days of {} sessions", a.buy_days, a.sessions),
    });
    // recency
    let n = a.dates.len();
    // NET buying in the window, as documented — not "any buy day in it".
    // The `any` form scored 10/10 for ARK dumping 300k shares and buying 1k
    // back on the final day. And a window SHORTER than k made the cutoff fall
    // to dates[0] via saturating_sub, so on a short window every buy counted
    // as "within the last 5" — which the existing 4-date unit test baked in.
    let net_recent = |k: usize| -> Option<f64> {
        if n < 2 {
            return None;
        }
        let idx = n.checked_sub(k)?; // None when the window is shorter than k
        let cutoff = a.dates.get(idx)?.clone();
        Some(
            a.buys
                .iter()
                .filter(|(d, _)| *d >= cutoff)
                .map(|(_, v)| *v)
                .sum::<f64>()
                - a.sells
                    .iter()
                    .filter(|(d, _)| *d >= cutoff)
                    .map(|(_, v)| *v)
                    .sum::<f64>(),
        )
    };
    let (pts, detail) = match (net_recent(5), net_recent(10)) {
        (Some(v5), _) if v5 > 0.0 => (10.0, format!("net +{v5:.0} shares in the last 5 sessions")),
        // The 10-session tier only applies when the most recent 5 are not net
        // SELLING. A -49,999 last-5 against a +50,001 last-10 used to score 5
        // and print "net +50001" — crediting buying that has since reversed.
        (Some(v5), Some(v10))
            if v10 > 0.0 && (v5 >= 0.0 || rules.recency_tier == RecencyTier::NetTen) =>
        {
            (5.0, format!("net +{v10:.0} shares in the last 10 sessions"))
        }
        (Some(v5), _) => (0.0, format!("net {v5:+.0} shares in the last 5 sessions")),
        _ => (0.0, "window too short to judge recency".to_string()),
    };
    fs.push(Factor {
        name: "recency",
        points: pts,
        max: 10.0,
        detail,
    });
    // price proximity
    let (pts, detail) = match (price, vwap) {
        (Some(p), Some(v)) if v > 0.0 => {
            let r = p / v;
            let pts = if r <= 1.0 {
                25.0
            } else if r <= 1.05 {
                18.0
            } else if r <= 1.10 {
                10.0
            } else if r <= 1.20 {
                4.0
            } else {
                0.0
            };
            (
                pts,
                format!(
                    "price {p:.2} vs ARK buy VWAP {v:.2} ({:+.1}%)",
                    (r - 1.0) * 100.0
                ),
            )
        }
        _ => (
            0.0,
            "no buys priced (no buy days or no price history)".to_string(),
        ),
    };
    fs.push(Factor {
        name: "price_proximity",
        points: pts,
        max: 25.0,
        detail,
    });
    // conviction
    let w = a.max_weight;
    let pts = if w >= 3.0 {
        10.0
    } else if w >= 1.5 {
        6.0
    } else if w >= 0.5 {
        3.0
    } else {
        0.0
    };
    fs.push(Factor {
        name: "conviction",
        points: pts,
        max: 10.0,
        detail: format!("max fund weight {w:.2}%"),
    });
    (fs.iter().map(|x| x.points).sum(), fs)
}

fn two_x(ticker: &str) -> Option<&'static str> {
    TWO_X.iter().find(|(t, _)| *t == ticker).map(|(_, v)| *v)
}

fn fetch_json(http: &reqwest::blocking::Client, url: &str) -> Result<Value, CliError> {
    let resp = http
        .get(url)
        .header(reqwest::header::USER_AGENT, "rhap (Mozilla/5.0 compatible)")
        .send()
        .map_err(|e| CliError::Upstream(format!("GET {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(CliError::Upstream(format!(
            "GET {url}: HTTP {}",
            resp.status()
        )));
    }
    resp.json::<Value>()
        .map_err(|e| CliError::Upstream(format!("GET {url}: bad JSON: {e}")))
}

/// Validated inputs for `ark score`.
pub struct ArkQuery<'a> {
    pub metric: &'static MetricDef,
    pub window: usize,
    pub funds: Vec<String>,
    pub tickers: Option<Vec<String>>,
    pub top: usize,
    pub collateral: Option<f64>,
    pub base_url: &'a str,
}

/// `ark score [--window N --funds A,B --tickers T,U --top N --collateral $]`.
/// External read (arkfunds.io) + MCP reads (quotes, historicals). No account.
pub fn ark_score(ctx: &Ctx, q: &ArkQuery) -> Result<(), CliError> {
    // Before any network: a registered metric with no implementation exits 2
    // without issuing six holdings requests first.
    let rules = ArkVersion::of(q.metric.name)?.rules();
    let http = pk_cli_http::client("rhap", env!("CARGO_PKG_VERSION"))?;
    let from = rfc3339_days_ago((q.window as f64 * 1.5) as i64 + 3)[..10].to_string();
    let to = rfc3339_days_ago(0)[..10].to_string();
    let funds: Vec<String> = if q.funds.is_empty() {
        FUNDS.iter().map(|s| s.to_string()).collect()
    } else {
        q.funds.clone()
    };
    let mut rows: Vec<HoldingRow> = Vec::new();
    for fund in &funds {
        let url = format!(
            "{}/api/v2/etf/holdings?symbol={fund}&date_from={from}&date_to={to}",
            q.base_url.trim_end_matches('/')
        );
        rows.extend(parse_holdings(&fetch_json(&http, &url)?));
    }
    if rows.is_empty() {
        return Err(CliError::Upstream("no ARK holdings rows returned".into()));
    }
    let mut acts: Vec<Activity> = activity(&rows, &rules).into_values().collect();
    if let Some(want) = &q.tickers {
        acts.retain(|a| want.iter().any(|t| t.eq_ignore_ascii_case(&a.ticker)));
    } else {
        // Price only names ARK is net-accumulating (the ones that can score).
        acts.retain(|a| a.net_shares > 0.0);
    }
    acts.sort_by(|x, y| {
        let ax = if x.held > 0.0 {
            x.net_shares / x.held
        } else {
            0.0
        };
        let ay = if y.held > 0.0 {
            y.net_shares / y.held
        } else {
            0.0
        };
        ay.partial_cmp(&ax).unwrap_or(std::cmp::Ordering::Equal)
    });
    // S8 (audit 2026-09-18): this truncated on ONE factor — accumulation, 35 of
    // 100 points — before prices were fetched and ark_1 was ever called. At the
    // default --top 15 only 30 names were ever scored, so a name weak on
    // accumulation but perfect on persistence, recency, proximity and
    // conviction (the other 65 points) was discarded unscored while the output
    // claimed to rank by ark/1. Widen the pricing set enough that the score,
    // not its largest single input, decides.
    let priced_cap = (q.top.max(1) * 6).max(60);
    let dropped = acts.len().saturating_sub(priced_cap);
    acts.truncate(priced_cap);

    // Prices: quotes (10 per call) + daily closes over the window.
    if dropped > 0 {
        eprintln!(
            "ark: {dropped} names beyond the {priced_cap}-name pricing cap were not scored \
             (ranked out on accumulation alone, which is 35 of 100 points)"
        );
    }
    let mut client = ctx.client()?;
    let symbols: Vec<String> = acts.iter().map(|a| a.ticker.clone()).collect();
    let mut price: BTreeMap<String, f64> = BTreeMap::new();
    let mut closes: BTreeMap<String, BTreeMap<String, f64>> = BTreeMap::new();
    for batch in symbols.chunks(10) {
        if let Ok(qv) = client.read("get_equity_quotes", json!({ "symbols": batch })) {
            for r in results_of(&qv) {
                let qq = r.get("quote").unwrap_or(&r);
                if let Some(sym) = qq.get("symbol").and_then(Value::as_str) {
                    let p = f(qq, "last_trade_price");
                    if p > 0.0 {
                        price.insert(sym.to_string(), p);
                    }
                }
            }
        }
        if let Ok(h) = client.read(
            "get_equity_historicals",
            json!({ "symbols": batch, "start_time": format!("{from}T00:00:00Z"), "interval": "day", "bounds": "regular" }),
        ) {
            for r in results_of(&h) {
                let Some(sym) = r.get("symbol").and_then(Value::as_str) else {
                    continue;
                };
                let m = closes.entry(sym.to_string()).or_default();
                for b in r.get("bars").and_then(Value::as_array).cloned().unwrap_or_default() {
                    if let Some(d) = b.get("begins_at").and_then(Value::as_str) {
                        let c = f(&b, "close_price");
                        if c > 0.0 {
                            m.insert(d[..10].to_string(), c);
                        }
                    }
                }
            }
        }
    }

    let mut scored: Vec<(f64, Value)> = acts
        .iter()
        .map(|a| {
            let p = price.get(&a.ticker).copied();
            let v = closes.get(&a.ticker).and_then(|c| buy_vwap(&a.buys, c));
            let (total, fs) = ark_score_of(a, p, v, &rules);
            let feasible = match (p, q.collateral) {
                (Some(p), Some(c)) => Some(100.0 * 0.9 * p <= c),
                _ => None,
            };
            let row = json!({
                "ticker": a.ticker,
                "price": p,
                "ark_buy_vwap": v.map(|x| (x * 100.0).round() / 100.0),
                "net_shares": a.net_shares,
                "held": a.held,
                "buy_days": a.buy_days,
                "sessions": a.sessions,
                "max_fund_weight_pct": a.max_weight,
                "two_x_vehicle": two_x(&a.ticker),
                "wheel_feasible": feasible,
                "csp_collateral": p.map(|p| (p * 100.0 * 0.9).round()),
                "metrics": { q.metric.name: {
                    "score": (total * 10.0).round() / 10.0,
                    "trade": total >= q.metric.trade_threshold,
                    "factors": fs.iter().map(|x| json!({"name": x.name, "points": x.points, "max": x.max, "detail": x.detail})).collect::<Vec<_>>(),
                }},
            });
            (total, row)
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let kept: Vec<Value> = scored.into_iter().take(q.top).map(|(_, r)| r).collect();
    let dto = json!({
        "metric": q.metric.name,
        "funds": funds,
        "window_sessions": q.window,
        "date_from": from,
        "date_to": to,
        "names_accumulating": acts.len(),
        "collateral": q.collateral,
        "names": kept,
    });
    output::emit(ctx.common.json, "ark-score", dto, output::render);
    Ok(())
}

/// Resolve the metric for `ark score` (a `ticker-score` metric).
pub fn metric(name: &str) -> Result<&'static MetricDef, CliError> {
    metric_of_kind(name, "ticker-score")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<HoldingRow> {
        let mk = |fund: &str, date: &str, t: &str, sh: f64, w: f64| HoldingRow {
            fund: fund.into(),
            date: date.into(),
            ticker: t.into(),
            shares: sh,
            weight: w,
        };
        vec![
            mk("ARKK", "2026-09-10", "AAA", 1000.0, 2.0),
            mk("ARKK", "2026-09-11", "AAA", 1100.0, 2.1),
            mk("ARKK", "2026-09-12", "AAA", 1100.0, 2.1),
            mk("ARKK", "2026-09-15", "AAA", 1300.0, 3.2),
            mk("ARKW", "2026-09-10", "AAA", 500.0, 1.0),
            mk("ARKW", "2026-09-15", "AAA", 400.0, 0.8),
            mk("ARKK", "2026-09-10", "BBB", 900.0, 1.0),
            mk("ARKK", "2026-09-15", "BBB", 700.0, 0.7),
        ]
    }

    #[test]
    fn holdings_diffs_become_buys_and_net_accumulation() {
        let acts = activity(&rows(), &ArkVersion::V1.rules());
        let a = &acts["AAA"];
        assert_eq!(a.net_shares, 200.0); // ARKK +300, ARKW −100
        assert_eq!(a.held, 1700.0);
        assert_eq!(a.buys.len(), 2); // 09-11 +100, 09-15 +200
        assert_eq!(a.buy_days, 2);
        assert_eq!(a.sessions, 4);
        assert_eq!(a.max_weight, 3.2);
        let b = &acts["BBB"];
        assert_eq!(b.net_shares, -200.0);
        assert!(b.buys.is_empty());
    }

    #[test]
    fn buy_vwap_weights_closes_by_shares_bought() {
        let buys = vec![
            ("2026-09-11".to_string(), 100.0),
            ("2026-09-15".to_string(), 200.0),
        ];
        let mut closes = BTreeMap::new();
        closes.insert("2026-09-11".to_string(), 10.0);
        closes.insert("2026-09-15".to_string(), 13.0);
        let v = buy_vwap(&buys, &closes).unwrap();
        assert!((v - 12.0).abs() < 1e-9);
        assert!(buy_vwap(&[], &closes).is_none());
    }

    /// One fund's file arriving a day late must not turn every flat position
    /// in it into an "initiation". The baseline question is asked of the
    /// FUND's first reported date, not the global calendar's.
    #[test]
    fn a_late_fund_file_does_not_fake_an_initiation() {
        let mk = |fund: &str, date: &str, sh: f64| HoldingRow {
            fund: fund.into(),
            date: date.into(),
            ticker: "FLAT".into(),
            shares: sh,
            weight: 1.0,
        };
        // ARKK reports from the 10th; ARKW's file only starts on the 11th and
        // its position never changes.
        let rows = vec![
            mk("ARKK", "2026-09-10", 100.0),
            mk("ARKK", "2026-09-11", 100.0),
            mk("ARKW", "2026-09-11", 500.0),
            mk("ARKW", "2026-09-12", 500.0),
        ];
        let a = &activity(&rows, &ArkVersion::V2.rules())["FLAT"];
        assert_eq!(
            a.net_shares, 0.0,
            "a flat holding in a late file is not accumulation"
        );
        assert_eq!(
            activity(&rows, &ArkVersion::V1.rules())["FLAT"].net_shares,
            500.0,
            "ark/1 is immutable: it still reads the late file as an initiation"
        );
    }

    /// A genuine initiation — absent on the fund's first date, present later —
    /// still gets a zero baseline.
    #[test]
    fn a_real_initiation_still_counts_from_zero() {
        let mk = |date: &str, t: &str, sh: f64| HoldingRow {
            fund: "ARKK".into(),
            date: date.into(),
            ticker: t.into(),
            shares: sh,
            weight: 1.0,
        };
        let rows = vec![
            mk("2026-09-10", "OLD", 100.0),
            mk("2026-09-11", "OLD", 100.0),
            mk("2026-09-11", "NEW", 500.0),
        ];
        assert_eq!(
            activity(&rows, &ArkVersion::V2.rules())["NEW"].net_shares,
            500.0
        );
        assert_eq!(
            activity(&rows, &ArkVersion::V1.rules())["NEW"].net_shares,
            500.0
        );
    }

    /// The 10-session tier must not credit buying that the last 5 reversed.
    #[test]
    fn recent_net_selling_forfeits_the_ten_session_tier() {
        let dates: Vec<String> = (1..=12).map(|d| format!("2026-09-{d:02}")).collect();
        let a = Activity {
            ticker: "AAA".into(),
            net_shares: 2.0,
            held: 1000.0,
            buys: vec![("2026-09-04".into(), 50_001.0)],
            sells: vec![("2026-09-11".into(), 49_999.0)],
            sessions: 12,
            buy_days: 1,
            max_weight: 3.5,
            dates,
        };
        // ark/1 keeps its published 10-session tier; ark/2 withdraws it.
        let (_, fs1) = ark_1(&a, Some(9.5), Some(10.0));
        assert_eq!(
            fs1.iter().find(|f| f.name == "recency").unwrap().points,
            5.0
        );
        let (_, fs) = ark_2(&a, Some(9.5), Some(10.0));
        let r = fs.iter().find(|f| f.name == "recency").unwrap();
        assert_eq!(r.points, 0.0, "{r:?}");
    }

    /// Documented as "NET buying in the last 5 sessions"; implemented as "ANY
    /// buy day in the last 5". ARK dumping 300k shares and buying 1k back on
    /// the final day scored the full 10/10.
    #[test]
    fn recency_is_net_not_any_buy_day() {
        let dumped = Activity {
            ticker: "AAA".into(),
            net_shares: -299_000.0,
            held: 1000.0,
            buys: vec![("2026-09-15".into(), 1_000.0)],
            sells: vec![("2026-09-14".into(), 300_000.0)],
            sessions: 6,
            buy_days: 1,
            max_weight: 3.5,
            dates: (8..=15)
                .filter(|d| *d != 12 && *d != 13)
                .map(|d| format!("2026-09-{d:02}"))
                .collect(),
        };
        let (_, fs) = ark_1(&dumped, Some(9.5), Some(10.0));
        let r = fs.iter().find(|f| f.name == "recency").unwrap();
        assert_eq!(
            r.points, 0.0,
            "a 300k sell with a 1k buy back is not accumulation: {r:?}"
        );
    }

    /// A window shorter than the lookback cannot answer the question.
    #[test]
    fn a_short_window_scores_no_recency_rather_than_a_confident_ten() {
        let short = Activity {
            ticker: "AAA".into(),
            net_shares: 100.0,
            held: 1000.0,
            buys: vec![("2026-09-11".into(), 100.0)],
            sells: vec![],
            sessions: 3,
            buy_days: 1,
            max_weight: 3.5,
            dates: vec![
                "2026-09-09".into(),
                "2026-09-10".into(),
                "2026-09-11".into(),
            ],
        };
        let (_, fs) = ark_1(&short, Some(9.5), Some(10.0));
        let r = fs.iter().find(|f| f.name == "recency").unwrap();
        assert_eq!(r.points, 0.0);
        assert!(r.detail.contains("too short"), "{r:?}");
    }

    #[test]
    fn ark_1_scores_a_textbook_accumulation_at_the_top() {
        // Six sessions, not four: a four-session window cannot judge "the last
        // five sessions", and the previous fixture asserted a recency score on
        // one — baking in a saturating_sub underflow that made every buy count
        // as recent on any short window.
        let a = Activity {
            ticker: "AAA".into(),
            net_shares: 300.0,
            held: 1000.0,
            buys: vec![
                ("2026-09-11".into(), 50.0),
                ("2026-09-14".into(), 100.0),
                ("2026-09-15".into(), 150.0),
            ],
            sells: vec![],
            sessions: 6,
            buy_days: 3,
            max_weight: 3.5,
            dates: vec![
                "2026-09-08".into(),
                "2026-09-09".into(),
                "2026-09-10".into(),
                "2026-09-11".into(),
                "2026-09-14".into(),
                "2026-09-15".into(),
            ],
        };
        let (total, fs) = ark_1(&a, Some(9.5), Some(10.0));
        assert_eq!(total, 100.0, "{fs:?}");
        let (t2, _) = ark_1(&a, Some(11.5), Some(10.0)); // +15% above her price
        assert_eq!(t2, 79.0);
        let (t3, _) = ark_1(&a, None, None);
        assert_eq!(t3, 75.0);
    }

    #[test]
    fn accumulation_is_piecewise_linear_and_zero_for_net_sellers() {
        let mk = |net: f64| Activity {
            ticker: "X".into(),
            net_shares: net,
            held: 1000.0,
            ..Default::default()
        };
        let pts = |net: f64| ark_1(&mk(net), None, None).1[0].points;
        assert_eq!(pts(-50.0), 0.0);
        assert!((pts(5.0) - 4.0).abs() < 1e-9); // 0.5% → 4
        assert!((pts(10.0) - 8.0).abs() < 1e-9); // 1% → 8
        assert!((pts(50.0) - 15.0).abs() < 1e-9); // 5% → 15
        assert!((pts(100.0) - 25.0).abs() < 1e-9); // 10% → 25
        assert!((pts(300.0) - 35.0).abs() < 1e-9); // 30% → cap
    }

    #[test]
    fn tickers_are_normalized_and_payloads_parse() {
        assert_eq!(norm_ticker("RKLB UQ"), "RKLB");
        assert_eq!(norm_ticker(" tsla "), "TSLA");
        let v = json!({"holdings": [
            {"fund": "ARKK", "date": "2026-09-16", "ticker": "TSLA", "shares": 1712551, "weight": 9.51},
            {"fund": "ARKK", "date": "2026-09-16", "ticker": "", "shares": 1, "weight": 0.0},
            {"fund": "ARKK", "date": "2026-09-16", "shares": 1, "weight": 0.0}
        ]});
        let rows = parse_holdings(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ticker, "TSLA");
        assert_eq!(rows[0].shares, 1712551.0);
        assert_eq!(two_x("TSLA"), Some("TSLL"));
        assert_eq!(two_x("ZZZZ"), None);
    }

    #[test]
    fn ark_metric_is_a_ticker_score_not_a_spread_score() {
        assert!(metric("ark/1").is_ok());
        assert!(metric("ark/2").is_ok());
        assert_eq!(metric("harvest/1").unwrap_err().exit_code(), 2);
        assert_eq!(
            super::super::score::metric_of_kind("ark/1", "score")
                .unwrap_err()
                .exit_code(),
            2
        );
    }
    #[test]
    fn persistence_uses_the_calendar_of_the_funds_that_hold_the_ticker() {
        // ARKK reports 6 sessions; ARKX starts on the 4th and holds LATE alone.
        let mut rows = Vec::new();
        for (k, d) in ["01", "02", "03", "04", "05", "06"].iter().enumerate() {
            rows.push(HoldingRow {
                fund: "ARKK".into(),
                date: format!("2026-09-{d}"),
                ticker: "OLD".into(),
                shares: 1000.0,
                weight: 1.0,
            });
            if k >= 3 {
                rows.push(HoldingRow {
                    fund: "ARKX".into(),
                    date: format!("2026-09-{d}"),
                    ticker: "LATE".into(),
                    shares: 1000.0 + 100.0 * (k as f64 - 3.0),
                    weight: 1.0,
                });
            }
        }
        let act = activity(&rows, &ArkVersion::V2.rules());
        assert_eq!(
            activity(&rows, &ArkVersion::V1.rules())["LATE"].sessions,
            6,
            "ark/1 is unchanged"
        );
        assert_eq!(act["OLD"].sessions, 6);
        assert_eq!(
            act["LATE"].sessions, 3,
            "ARKX never reported the first three"
        );
        assert_eq!(act["LATE"].dates.first().unwrap(), "2026-09-04");
    }
}
