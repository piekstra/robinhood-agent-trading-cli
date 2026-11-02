//! `metrics` and `option score` — named, versioned contract metrics computed
//! locally from MCP reads: the operator's own "custom data points" attached
//! to a contract or spread.
//!
//! The MCP returns quotes and greeks; it has no notion of a *good* contract.
//! Each metric here is a codename with a version (`harvest/1`) so that
//! revisions coexist and results stay comparable: a published definition never
//! changes — a change is a new version (`harvest/2`), and other families can
//! sit alongside. `rhap metrics list` is the registry; `rhap option score
//! --metric <name>` pulls the chain and emits every candidate spread with the
//! metric's score and per-factor breakdown under `metrics.<name>`.
//!
//! Everything numeric is pure and unit-tested offline; only [`option_score`]
//! touches the network, and it only reads.

use std::collections::BTreeMap;

use pk_cli_core::{output, CliError};
use serde_json::{json, Map, Value};

use super::reads::quote_args;
use super::Ctx;

// --------------------------------------------------------------- registry

/// A published metric: its codename, what it applies to, and its factor
/// weights (which sum to the maximum score).
#[derive(Debug)]
pub struct MetricDef {
    pub name: &'static str,
    /// `score` (0–100 over weighted factors) or `state` (a labelled regime).
    pub kind: &'static str,
    pub summary: &'static str,
    pub applies_to: &'static str,
    pub trade_threshold: f64,
    pub factors: &'static [(&'static str, f64)],
}

/// Every metric rhap knows. Append only — never edit a published entry.
pub const METRICS: &[MetricDef] = &[
    MetricDef {
        name: "harvest/1",
        kind: "score",
        summary: "Premium-harvest quality of a vertical credit spread: credit/width, \
              short-leg liquidity, delta placement, theta/vega, probability of \
              profit, technical cushion (0–100; trade at ≥ 65)",
        applies_to: "vertical put or call credit spreads (two legs, same expiry)",
        trade_threshold: 65.0,
        factors: &[
            ("credit_over_width", 30.0),
            ("liquidity", 20.0),
            ("delta_placement", 15.0),
            ("theta_over_vega", 15.0),
            ("probability", 10.0),
            ("technical_cushion", 10.0),
        ],
    },
    MetricDef {
        name: "regime/1",
        kind: "state",
        summary: "S&P 500 market mode for premium selling: trend (close vs 20/50-day \
                  means), realized vol 10/20d vs VIX (vol risk premium), drawdown from \
                  the 20-day high, VIX vs its 5-day high → calm-uptrend | calm-range | \
                  stressed | panic, with a size multiplier and delta ceiling",
        applies_to: "the S&P 500 (SPX bars + VIX); queried via `rhap market regime`",
        trade_threshold: 0.0,
        factors: &[],
    },
    MetricDef {
        name: "ark/1",
        kind: "ticker-score",
        summary: "ARK Invest activity for a ticker: accumulation (net shares bought over the \
                  window as % of holding), persistence (buy-days), recency, price vs ARK's \
                  buy VWAP, conviction (fund weight) → 0–100; trade at ≥ 60. Data: daily \
                  holdings of ARKK/ARKW/ARKQ/ARKG/ARKF/ARKX (arkfunds.io mirror of ARK's CSVs)",
        applies_to: "equity tickers held by ARK ETFs; `rhap ark score`",
        trade_threshold: 60.0,
        factors: &[
            ("accumulation", 35.0),
            ("persistence", 20.0),
            ("recency", 10.0),
            ("price_proximity", 25.0),
            ("conviction", 10.0),
        ],
    },
    MetricDef {
        name: "ark/2",
        kind: "ticker-score",
        summary: "ark/1 with three measurement repairs (initiation baseline and calendar \
                  taken from the funds that hold the ticker; the 10-session recency tier \
                  withdrawn when the last 5 are net selling). ARK Invest activity for a ticker: accumulation (net shares bought over the \
                  window as % of holding), persistence (buy-days), recency, price vs ARK's \
                  buy VWAP, conviction (fund weight) → 0–100; trade at ≥ 60. Data: daily \
                  holdings of ARKK/ARKW/ARKQ/ARKG/ARKF/ARKX (arkfunds.io mirror of ARK's CSVs)",
        applies_to: "equity tickers held by ARK ETFs; `rhap ark score`",
        trade_threshold: 60.0,
        factors: &[
            ("accumulation", 35.0),
            ("persistence", 20.0),
            ("recency", 10.0),
            ("price_proximity", 25.0),
            ("conviction", 10.0),
        ],
    },
    MetricDef {
        name: "regime/2",
        kind: "state",
        summary: "regime/1 plus trend quality (SMA20/SMA50 spread expanding vs contracting → \
                  a `rolling-over` state), a single-day −3% trigger, and asymmetric dwell \
                  (escalate at once; de-escalate after 3 consecutive calmer sessions)",
        applies_to: "the S&P 500 (SPX bars + VIX); `rhap market regime --metric regime/2 [--history N]`",
        trade_threshold: 0.0,
        factors: &[],
    },
    MetricDef {
        name: "regime/3",
        kind: "state",
        summary: "regime/2 overlaid with a structural SMA50/SMA200 phase (spread + expansion, \
                  golden/death cross, days below SMA50, velocity override 8%/10d or −3% day \
                  with 10-session cooldown); handling = the stricter of the two; sets the \
                  preferred side (put spreads in bull phases, none/calls in bear phases)",
        applies_to: "the S&P 500 (needs ≥ 205 sessions); `rhap market regime --metric regime/3 [--history N]`",
        trade_threshold: 0.0,
        factors: &[],
    },
    MetricDef {
        name: "regime/4",
        kind: "state",
        summary: "regime/3 that fails closed: with fewer than 205 sessions the structural \
                  phase is `unknown` and opening is BLOCKED (regime/3 only restricts it); the \
                  reason line states the opening decision. Otherwise identical: regime/2 overlaid with a structural SMA50/SMA200 phase (spread + expansion, \
                  golden/death cross, days below SMA50, velocity override 8%/10d or −3% day \
                  with 10-session cooldown); handling = the stricter of the two; sets the \
                  preferred side (put spreads in bull phases, none/calls in bear phases)",
        applies_to: "the S&P 500 (needs ≥ 205 sessions); `rhap market regime --metric regime/4 [--history N]`",
        trade_threshold: 0.0,
        factors: &[],
    },
];

/// Look a metric up by codename and kind (`score` for `option score`, `state`
/// for `market regime`).
pub fn metric_of_kind(name: &str, kind: &str) -> Result<&'static MetricDef, CliError> {
    let m = metric(name)?;
    if m.kind != kind {
        return Err(CliError::Usage(format!(
            "metric `{}` is a {} metric, not a {kind} metric",
            m.name, m.kind
        )));
    }
    Ok(m)
}

/// Look a metric up by codename (`family/version`).
pub fn metric(name: &str) -> Result<&'static MetricDef, CliError> {
    let wanted = name.trim();
    METRICS.iter().find(|m| m.name == wanted).ok_or_else(|| {
        CliError::Usage(format!(
            "unknown metric `{wanted}` (known: {})",
            METRICS
                .iter()
                .map(|m| m.name)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })
}

/// The `metrics/v1` DTO.
pub fn metrics_dto() -> Value {
    let list: Vec<Value> = METRICS
        .iter()
        .map(|m| {
            let mut f = Map::new();
            for (k, w) in m.factors {
                f.insert((*k).into(), json!(w));
            }
            json!({
                "name": m.name,
                "kind": m.kind,
                "summary": m.summary,
                "applies_to": m.applies_to,
                "trade_threshold": m.trade_threshold,
                "factors": Value::Object(f),
            })
        })
        .collect();
    json!({ "metrics": list })
}

/// `metrics list` — offline; no session, no network.
pub fn metrics_list(json_mode: bool) -> Result<(), CliError> {
    output::emit(json_mode, "metrics", metrics_dto(), output::render);
    Ok(())
}

// ------------------------------------------------------------ pure inputs

/// The underlying's class, which sets the liquidity and theta/vega bars.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Index,
    Sector,
    Leveraged,
}

impl Class {
    pub fn parse(raw: &str) -> Result<Self, CliError> {
        match raw.trim().to_lowercase().as_str() {
            "index" => Ok(Self::Index),
            "sector" => Ok(Self::Sector),
            "leveraged" => Ok(Self::Leveraged),
            other => Err(CliError::Usage(format!(
                "--class must be index, sector, or leveraged (got `{other}`)"
            ))),
        }
    }

    fn wire(self) -> &'static str {
        match self {
            Self::Index => "index",
            Self::Sector => "sector",
            Self::Leveraged => "leveraged",
        }
    }

    /// Minimum short-leg |theta|/vega for full marks.
    fn theta_vega_bar(self) -> f64 {
        match self {
            Self::Index => 0.25,
            Self::Sector => 0.50,
            Self::Leveraged => 1.50,
        }
    }
}

/// One quoted leg, parsed from `get_option_quotes` + its instrument.
#[derive(Clone, Debug, PartialEq)]
pub struct Leg {
    pub id: String,
    pub strike: f64,
    pub bid: f64,
    pub ask: f64,
    pub mark: f64,
    pub delta: f64,
    pub theta: f64,
    pub vega: f64,
    pub iv: f64,
    pub oi: u64,
    pub pop_short: f64,
}

impl Leg {
    /// Midpoint when both sides are quoted, else the mark.
    pub fn mid(&self) -> f64 {
        if self.bid > 0.0 && self.ask > 0.0 {
            (self.bid + self.ask) / 2.0
        } else {
            self.mark
        }
    }

    /// Bid–ask width as a fraction of the mid (1.0 when unquotable).
    pub fn spread_pct(&self) -> f64 {
        let mid = self.mid();
        if mid <= 0.0 || self.bid <= 0.0 || self.ask <= 0.0 {
            1.0
        } else {
            (self.ask - self.bid) / mid
        }
    }

    fn dto(&self) -> Value {
        json!({
            "id": self.id,
            "strike": self.strike,
            "bid": self.bid,
            "ask": self.ask,
            "mid": round(self.mid(), 4),
            "delta": self.delta,
            "theta": self.theta,
            "vega": self.vega,
            "iv": self.iv,
            "open_interest": self.oi,
            "chance_of_profit_short": self.pop_short,
        })
    }
}

/// Realized-price context for the technical-cushion factor.
#[derive(Clone, Debug, PartialEq)]
pub struct Cushion {
    pub spot: f64,
    pub atr14: Option<f64>,
    pub low20: Option<f64>,
}

/// One scored factor.
#[derive(Clone, Debug, PartialEq)]
pub struct Factor {
    pub name: &'static str,
    pub points: f64,
    pub max: f64,
    pub detail: String,
}

/// A metric's result for one spread.
#[derive(Clone, Debug, PartialEq)]
pub struct Scored {
    pub total: f64,
    pub factors: Vec<Factor>,
}

impl Scored {
    fn dto(&self, threshold: f64) -> Value {
        json!({
            "score": round(self.total, 1),
            "trade": self.total >= threshold,
            "factors": self.factors.iter().map(|f| json!({
                "name": f.name, "points": round(f.points, 1), "max": f.max, "detail": f.detail,
            })).collect::<Vec<_>>(),
        })
    }
}

fn round(x: f64, places: i32) -> f64 {
    let p = 10f64.powi(places);
    (x * p).round() / p
}

// --------------------------------------------------------------- harvest/1

/// `harvest/1`: the premium-harvest rubric for a vertical credit spread.
///
/// Weights: credit/width 30 · liquidity 20 · delta placement 15 · theta/vega
/// 15 · probability 10 · technical cushion 10. Thresholds are the published
/// definition — do not tune them here; publish `harvest/2` instead.
pub fn harvest_1(short: &Leg, long: &Leg, class: Class, cushion: Option<&Cushion>) -> Scored {
    let width = (short.strike - long.strike).abs();
    let credit = short.mid() - long.mid();
    let cw = if width > 0.0 { credit / width } else { 0.0 };
    let mut factors = Vec::with_capacity(6);

    // 1. Credit / width: < 20% → 0; 20% → half; ≥ 30% → full (linear between).
    let pts = if cw < 0.20 {
        0.0
    } else {
        (15.0 + 15.0 * (cw - 0.20) / 0.10).min(30.0)
    };
    factors.push(Factor {
        name: "credit_over_width",
        points: pts,
        max: 30.0,
        detail: format!(
            "credit {:.2} / width {:.2} = {:.0}%",
            credit,
            width,
            cw * 100.0
        ),
    });

    // 2. Liquidity: short-leg market ≤ 2% → 16, ≤ 5% → 8, ≤ 10% → 4, else 0;
    //    open-interest bonus ≥ 5,000 → +4, ≥ 1,000 → +2.
    let sp = short.spread_pct();
    let mut pts = if sp <= 0.02 {
        16.0
    } else if sp <= 0.05 {
        8.0
    } else if sp <= 0.10 {
        4.0
    } else {
        0.0
    };
    let bonus = if short.oi >= 5_000 {
        4.0
    } else if short.oi >= 1_000 {
        2.0
    } else {
        0.0
    };
    pts += bonus;
    factors.push(Factor {
        name: "liquidity",
        points: pts,
        max: 20.0,
        detail: format!(
            "short-leg market {:.1}% of mid, OI {} (+{} bonus)",
            sp * 100.0,
            short.oi,
            bonus
        ),
    });

    // 3. Delta placement: |Δ| 0.22–0.32 full; 0.18–0.22 / 0.32–0.38 half; else 0.
    let d = short.delta.abs();
    let pts = if (0.22..=0.32).contains(&d) {
        15.0
    } else if (0.18..0.22).contains(&d) || (0.32..=0.38).contains(&d) {
        7.5
    } else {
        0.0
    };
    factors.push(Factor {
        name: "delta_placement",
        points: pts,
        max: 15.0,
        detail: format!("short |delta| {d:.3}"),
    });

    // 4. Theta / vega on the short leg vs the class bar (full ≥ bar, half ≥ bar/2).
    let bar = class.theta_vega_bar();
    let ratio = if short.vega > 0.0 {
        short.theta.abs() / short.vega
    } else {
        0.0
    };
    let pts = if ratio >= bar {
        15.0
    } else if ratio >= bar / 2.0 {
        7.5
    } else {
        0.0
    };
    factors.push(Factor {
        name: "theta_over_vega",
        points: pts,
        max: 15.0,
        detail: format!("|theta|/vega {ratio:.2} vs {} bar {bar:.2}", class.wire()),
    });

    // 5. Probability: chance_of_profit_short ≥ 0.72 full; ≥ 0.68 half.
    let p = short.pop_short;
    let pts = if p >= 0.72 {
        10.0
    } else if p >= 0.68 {
        5.0
    } else {
        0.0
    };
    factors.push(Factor {
        name: "probability",
        points: pts,
        max: 10.0,
        detail: format!("chance_of_profit_short {p:.3}"),
    });

    // 6. Technical cushion: short strike ≥ 1 ATR(14) beyond spot (+5) and beyond
    //    the 20-day extreme (+5). Puts measure below; calls above.
    let is_put = short.strike >= long.strike || short.delta < 0.0;
    let (pts, detail) = match cushion {
        Some(c) => {
            let mut pts = 0.0;
            let mut notes = Vec::new();
            match c.atr14 {
                Some(atr) if atr > 0.0 => {
                    let dist = if is_put {
                        c.spot - short.strike
                    } else {
                        short.strike - c.spot
                    };
                    if dist >= atr {
                        pts += 5.0;
                    }
                    notes.push(format!("{:.2} ATR from spot", dist / atr));
                }
                _ => notes.push("ATR unavailable".into()),
            }
            match c.low20 {
                Some(ext) if is_put => {
                    if short.strike < ext {
                        pts += 5.0;
                    }
                    notes.push(format!("20d low {ext:.2}"));
                }
                Some(_) => notes.push("call-side extreme not evaluated in harvest/1".into()),
                None => notes.push("20d extreme unavailable".into()),
            }
            (pts, notes.join("; "))
        }
        None => (0.0, "not evaluated (no price history)".into()),
    };
    factors.push(Factor {
        name: "technical_cushion",
        points: pts,
        max: 10.0,
        detail,
    });

    let total = factors.iter().map(|f| f.points).sum();
    Scored { total, factors }
}

/// Dispatch a metric by codename.
pub fn score(
    def: &MetricDef,
    short: &Leg,
    long: &Leg,
    class: Class,
    cushion: Option<&Cushion>,
) -> Scored {
    match def.name {
        "harvest/1" => harvest_1(short, long, class, cushion),
        "regime/1" | "regime/2" | "regime/3" | "regime/4" => {
            unreachable!("{} is a state metric — see `market regime`", def.name)
        }
        "ark/1" | "ark/2" => {
            unreachable!("{} is a ticker metric — see `ark score`", def.name)
        }
        // Kept in lockstep with the registry by `registry_dispatches…` (score
        // metrics) and `every_registered_ticker_and_state_metric_is_routed…`.
        _ => unreachable!("metric {} is registered but has no scorer", def.name),
    }
}

// ------------------------------------------------------------- pure helpers

/// Pair every short leg with the long leg one `width` further out-of-the-money
/// (puts: lower strike; calls: higher). Legs without a partner are skipped.
pub fn pair_spreads(legs: &[Leg], width: f64, is_put: bool) -> Vec<(Leg, Leg)> {
    let mut out = Vec::new();
    for short in legs {
        let target = if is_put {
            short.strike - width
        } else {
            short.strike + width
        };
        if let Some(long) = legs.iter().find(|l| (l.strike - target).abs() < 1e-6) {
            out.push((short.clone(), long.clone()));
        }
    }
    out
}

/// The chain's native strike step near a price: the smallest gap between
/// adjacent strikes within ±10% of `around`.
pub fn strike_step(strikes: &[f64], around: f64) -> Option<f64> {
    let mut near: Vec<f64> = strikes
        .iter()
        .copied()
        .filter(|s| (s - around).abs() <= around * 0.10)
        .collect();
    near.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    near.dedup();
    near.windows(2)
        .map(|w| w[1] - w[0])
        .filter(|g| *g > 1e-6)
        .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
}

/// Average true range over the last `n` daily bars and the lowest low over the
/// last 20, from `(high, low, close)` tuples in chronological order.
pub fn atr_and_low(bars: &[(f64, f64, f64)], n: usize) -> (Option<f64>, Option<f64>) {
    if bars.len() < 2 {
        return (None, None);
    }
    let trs: Vec<f64> = bars
        .windows(2)
        .map(|w| {
            let (h, l, _) = w[1];
            let pc = w[0].2;
            (h - l).max((h - pc).abs()).max((l - pc).abs())
        })
        .collect();
    let atr = if trs.len() >= n {
        Some(trs[trs.len() - n..].iter().sum::<f64>() / n as f64)
    } else {
        None
    };
    let tail = &bars[bars.len().saturating_sub(20)..];
    let low = tail.iter().map(|b| b.1).fold(f64::INFINITY, f64::min);
    (atr, (tail.len() >= 20 && low.is_finite()).then_some(low))
}

/// Pull the `cursor` query parameter out of a `next` URL (percent-decoded).
pub fn cursor_from_next(next: &str) -> Option<String> {
    let q = next.split_once('?')?.1;
    q.split('&')
        .find_map(|kv| kv.strip_prefix("cursor="))
        .map(|c| c.replace("%3D", "=").replace("%3d", "="))
        .filter(|c| !c.is_empty())
}

/// Civil date from days since the Unix epoch (Howard Hinnant's algorithm).
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `YYYY-MM-DDT00:00:00Z` for `days_ago` days before today (UTC).
pub(crate) fn rfc3339_days_ago(days_ago: i64) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, m, d) = civil_from_days(secs / 86_400 - days_ago);
    format!("{y:04}-{m:02}-{d:02}T00:00:00Z")
}

pub(crate) fn f(v: &Value, key: &str) -> f64 {
    match v.get(key) {
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Parse one `get_option_quotes` row (the `quote` object) into a [`Leg`],
/// given the instrument's strike.
pub fn leg_from_quote(q: &Value, strike: f64) -> Option<Leg> {
    let id = q.get("instrument_id")?.as_str()?.to_string();
    Some(Leg {
        id,
        strike,
        bid: f(q, "bid_price"),
        ask: f(q, "ask_price"),
        mark: f(q, "mark_price"),
        delta: f(q, "delta"),
        theta: f(q, "theta"),
        vega: f(q, "vega"),
        iv: f(q, "implied_volatility"),
        oi: q.get("open_interest").and_then(Value::as_u64).unwrap_or(0),
        pop_short: f(q, "chance_of_profit_short"),
    })
}

/// The result rows of a tool payload. The tools name their arrays differently
/// (`results`, `instruments`, `chains`, `bars`; usually under `data`), so this
/// takes the first known key, else the only array in the object, else a bare
/// array — a shape it doesn't recognize yields no rows rather than a panic.
pub(crate) fn results_of(payload: &Value) -> Vec<Value> {
    let inner = payload.get("data").unwrap_or(payload);
    for key in ["results", "instruments", "chains", "quotes", "bars"] {
        if let Some(arr) = inner.get(key).and_then(Value::as_array) {
            return arr.clone();
        }
    }
    if let Some(obj) = inner.as_object() {
        let arrays: Vec<&Vec<Value>> = obj.values().filter_map(Value::as_array).collect();
        if arrays.len() == 1 {
            return arrays[0].clone();
        }
    }
    inner.as_array().cloned().unwrap_or_default()
}

fn next_of(payload: &Value) -> Option<String> {
    let inner = payload.get("data").unwrap_or(payload);
    inner
        .get("next")
        .and_then(Value::as_str)
        .map(str::to_string)
}

// ------------------------------------------------------ underlying lookups

/// Daily `(high, low, close)` bars, oldest first.
pub type Bars = Vec<(f64, f64, f64)>;

/// Where the underlying's level and bars come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnderlyingType {
    Equity,
    Index,
}

impl UnderlyingType {
    fn wire(self) -> &'static str {
        match self {
            Self::Equity => "equity",
            Self::Index => "index",
        }
    }
}

/// Daily `(high, low, close)` tuples from a historicals row's `bars`, accepting
/// the equity (`*_price`) and index (`*_value`) spellings.
pub fn bars_of(row: &Value) -> Bars {
    row.get("bars")
        .and_then(Value::as_array)
        .map(|bars| {
            bars.iter()
                .filter(|b| {
                    !b.get("interpolated")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                })
                .map(|b| {
                    let pick = |a: &str, c: &str| {
                        let x = f(b, a);
                        if x > 0.0 {
                            x
                        } else {
                            f(b, c)
                        }
                    };
                    (
                        pick("high_price", "high_value"),
                        pick("low_price", "low_value"),
                        pick("close_price", "close_value"),
                    )
                })
                .filter(|(h, l, c)| *h > 0.0 && *l > 0.0 && *c > 0.0)
                .collect()
        })
        .unwrap_or_default()
}

/// The underlying's level, its type, and (optionally) ~45 days of daily bars.
/// Equities answer `get_equity_quotes`; index underlyings (XSP, SPX, XND, …)
/// 404 there, so the fallback resolves the index instrument and uses the
/// index quote/historical tools instead.
fn spot_and_bars(
    client: &mut crate::mcp::McpClient,
    underlying: &str,
    want_bars: bool,
) -> Result<(f64, UnderlyingType, Bars), CliError> {
    let start = rfc3339_days_ago(45);
    if let Ok(eq) = client.read("get_equity_quotes", quote_args(underlying)) {
        let spot = results_of(&eq)
            .first()
            .map(|r| f(r.get("quote").unwrap_or(r), "last_trade_price"))
            .filter(|s| *s > 0.0);
        if let Some(spot) = spot {
            let bars = if want_bars {
                let hist = client.read(
                    "get_equity_historicals",
                    json!({
                        "symbols": [underlying],
                        "start_time": start,
                        "interval": "day",
                        "bounds": "regular",
                    }),
                )?;
                results_of(&hist).first().map(bars_of).unwrap_or_default()
            } else {
                Vec::new()
            };
            return Ok((spot, UnderlyingType::Equity, bars));
        }
    }
    // `get_indexes` takes a comma-separated string, unlike the equity tools.
    let idx = client.read("get_indexes", json!({ "symbols": underlying }))?;
    let inner = idx.get("data").unwrap_or(&idx);
    let id = inner
        .get("indexes")
        .and_then(Value::as_array)
        .and_then(|a| {
            a.iter()
                .find(|i| i.get("symbol").and_then(Value::as_str) == Some(underlying))
        })
        .and_then(|i| i.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .ok_or_else(|| {
            CliError::NotFound(format!(
                "{underlying} is neither a quotable equity nor an index"
            ))
        })?;
    let q = client.read("get_index_quotes", json!({ "instrument_ids": [id] }))?;
    let spot = results_of(&q)
        .first()
        .map(|r| f(r, "value"))
        .filter(|s| *s > 0.0)
        .ok_or_else(|| CliError::Upstream(format!("no index level for {underlying}")))?;
    let bars = if want_bars {
        let hist = client.read(
            "get_index_historicals",
            json!({ "instrument_ids": [id], "start_time": start, "interval": "day" }),
        )?;
        results_of(&hist).first().map(bars_of).unwrap_or_default()
    } else {
        Vec::new()
    };
    Ok((spot, UnderlyingType::Index, bars))
}

// ------------------------------------------------------------- the command

/// Validated inputs for `option score`.
pub struct ScoreQuery<'a> {
    pub underlying: &'a str,
    pub expiration: &'a str,
    pub is_put: bool,
    pub width: Option<f64>,
    pub class: Class,
    pub metric: &'static MetricDef,
    pub min_delta: f64,
    pub max_delta: f64,
    pub cushion: bool,
    pub top: usize,
}

/// `option score --underlying --expiration [--type --width --class --metric
/// --min-delta --max-delta --no-cushion --top]` → chains + instruments +
/// quotes (+ equity quote + daily bars), scored locally. READ-only.
pub fn option_score(ctx: &Ctx, q: &ScoreQuery) -> Result<(), CliError> {
    let underlying = q.underlying.trim().to_uppercase();
    let kind = if q.is_put { "put" } else { "call" };
    let mut client = ctx.client()?;

    // 1. Chain: confirms the symbol and the expiry exist.
    let chains = client.read(
        "get_option_chains",
        json!({ "underlying_symbol": underlying }),
    )?;
    let chain = results_of(&chains)
        .into_iter()
        .find(|c| c.get("symbol").and_then(Value::as_str) == Some(underlying.as_str()))
        .ok_or_else(|| CliError::NotFound(format!("no option chain for {underlying}")))?;
    if let Some(dates) = chain.get("expiration_dates").and_then(Value::as_array) {
        if !dates.iter().any(|d| d.as_str() == Some(q.expiration)) {
            return Err(CliError::NotFound(format!(
                "{underlying} has no {} expiry; chain lists: {}",
                q.expiration,
                dates
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
    }

    // 2. Spot (+ daily bars for the cushion factor), equity or index.
    let (spot, underlying_type, bars) = spot_and_bars(&mut client, &underlying, q.cushion)?;

    // 3. Instruments for the expiry (paginated), narrowed to the OTM band.
    let (lo, hi) = if q.is_put {
        (spot * 0.70, spot * 1.02)
    } else {
        (spot * 0.98, spot * 1.30)
    };
    let mut strikes: BTreeMap<String, f64> = BTreeMap::new();
    let mut all_strikes: Vec<f64> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut scanned = 0usize;
    for _ in 0..50 {
        let mut args = json!({
            "chain_symbol": underlying,
            "expiration_dates": q.expiration,
            "type": kind,
            "state": "active",
        });
        if let Some(c) = &cursor {
            args["cursor"] = Value::String(c.clone());
        }
        let page = client.read("get_option_instruments", args)?;
        for inst in results_of(&page) {
            scanned += 1;
            let strike = f(&inst, "strike_price");
            all_strikes.push(strike);
            if strike >= lo && strike <= hi {
                if let Some(id) = inst.get("id").and_then(Value::as_str) {
                    strikes.insert(id.to_string(), strike);
                }
            }
        }
        cursor = next_of(&page).and_then(|n| cursor_from_next(&n));
        if cursor.is_none() {
            break;
        }
    }
    if strikes.is_empty() {
        return Err(CliError::NotFound(format!(
            "no {kind} instruments for {underlying} {} within {lo:.2}–{hi:.2}",
            q.expiration
        )));
    }
    let width = match q.width {
        Some(w) => w,
        None => strike_step(&all_strikes, spot).ok_or_else(|| {
            CliError::Upstream("could not infer the strike step; pass --width".into())
        })?,
    };

    // 4. Quotes, 20 ids per call (the tool's documented comfortable batch).
    let ids: Vec<String> = strikes.keys().cloned().collect();
    let mut legs: Vec<Leg> = Vec::new();
    for batch in ids.chunks(20) {
        let quotes = client.read("get_option_quotes", json!({ "instrument_ids": batch }))?;
        for row in results_of(&quotes) {
            let qv = row.get("quote").unwrap_or(&row);
            let Some(id) = qv.get("instrument_id").and_then(Value::as_str) else {
                continue;
            };
            if let Some(strike) = strikes.get(id) {
                if let Some(leg) = leg_from_quote(qv, *strike) {
                    legs.push(leg);
                }
            }
        }
    }
    legs.sort_by(|a, b| {
        b.strike
            .partial_cmp(&a.strike)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // 5. Realized-price context for the cushion factor.
    let cushion = if q.cushion {
        let (atr14, low20) = atr_and_low(&bars, 14);
        Some(Cushion { spot, atr14, low20 })
    } else {
        None
    };

    // 6. Pair, filter by short delta, score, rank.
    let pairs = pair_spreads(&legs, width, q.is_put);
    let mut rows: Vec<(f64, Value)> = pairs
        .iter()
        .filter(|(s, _)| {
            let d = s.delta.abs();
            d >= q.min_delta && d <= q.max_delta
        })
        .map(|(s, l)| {
            let scored = score(q.metric, s, l, q.class, cushion.as_ref());
            let credit = s.mid() - l.mid();
            let mut metrics = Map::new();
            metrics.insert(q.metric.name.into(), scored.dto(q.metric.trade_threshold));
            let row = json!({
                "short": s.dto(),
                "long": l.dto(),
                "width": round(width, 4),
                "credit_mid": round(credit, 4),
                "credit_over_width": round(if width > 0.0 { credit / width } else { 0.0 }, 4),
                "max_loss_per_contract": round((width - credit) * 100.0, 2),
                "metrics": Value::Object(metrics),
            });
            (scored.total, row)
        })
        .collect();
    rows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let total = rows.len();
    let kept: Vec<Value> = rows.into_iter().take(q.top).map(|(_, r)| r).collect();
    if total > kept.len() {
        eprintln!(
            "option score: showing top {} of {total} spreads (raise --top to see more)",
            kept.len()
        );
    }

    let dto = json!({
        "metric": q.metric.name,
        "underlying": underlying,
        "underlying_type": underlying_type.wire(),
        "expiration": q.expiration,
        "type": kind,
        "class": q.class.wire(),
        "width": round(width, 4),
        "spot": spot,
        "delta_band": [q.min_delta, q.max_delta],
        "cushion": cushion.as_ref().map(|c| json!({
            "atr14": c.atr14.map(|x| round(x, 4)),
            "low20": c.low20.map(|x| round(x, 4)),
        })),
        "instruments_scanned": scanned,
        "legs_quoted": legs.len(),
        "spreads_scored": total,
        "spreads": kept,
    });
    output::emit(ctx.common.json, "option-score", dto, output::render);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    fn leg(
        id: &str,
        strike: f64,
        bid: f64,
        ask: f64,
        delta: f64,
        theta: f64,
        vega: f64,
        oi: u64,
        pop: f64,
    ) -> Leg {
        Leg {
            id: id.into(),
            strike,
            bid,
            ask,
            mark: (bid + ask) / 2.0,
            delta,
            theta,
            vega,
            iv: 0.2,
            oi,
            pop_short: pop,
        }
    }

    /// The other two families. Both resolvers REFUSE a registered name they
    /// have no arm for, so a version that is published but never routed fails
    /// here instead of computing an older definition under the new label.
    #[test]
    fn every_registered_ticker_and_state_metric_is_routed_to_its_own_definition() {
        use super::super::ark::ArkVersion;
        use super::super::regime::RegimeVersion;
        let mut ark_rules = Vec::new();
        for m in METRICS.iter().filter(|m| m.kind == "ticker-score") {
            let v = ArkVersion::of(m.name).unwrap_or_else(|_| panic!("{} is unrouted", m.name));
            assert!(
                !ark_rules.contains(&v.rules()),
                "{} resolves to the same rules as an earlier version",
                m.name
            );
            ark_rules.push(v.rules());
        }
        let mut regimes = Vec::new();
        for m in METRICS.iter().filter(|m| m.kind == "state") {
            let v = RegimeVersion::of(m.name).unwrap_or_else(|_| panic!("{} is unrouted", m.name));
            assert!(!regimes.contains(&v), "{} shares a version", m.name);
            regimes.push(v);
        }
        let kinds = ["score", "ticker-score", "state"];
        assert!(
            METRICS.iter().all(|m| kinds.contains(&m.kind)),
            "a new metric kind needs its own routing check here"
        );
        assert_eq!(ArkVersion::of("ark/99").unwrap_err().exit_code(), 2);
        assert_eq!(RegimeVersion::of("regime/99").unwrap_err().exit_code(), 2);
    }

    #[test]
    fn registry_dispatches_every_published_metric() {
        let s = leg("S", 100.0, 1.0, 1.1, -0.28, -0.05, 0.10, 6000, 0.75);
        let l = leg("L", 98.0, 0.5, 0.6, -0.20, -0.03, 0.08, 6000, 0.80);
        for m in METRICS.iter().filter(|m| m.kind == "score") {
            let r = score(m, &s, &l, Class::Index, None);
            assert!(
                r.total >= 0.0 && r.total <= 100.0,
                "{} out of range",
                m.name
            );
            assert_eq!(r.factors.len(), m.factors.len(), "{} factor count", m.name);
            let max: f64 = r.factors.iter().map(|f| f.max).sum();
            assert!(
                (max - 100.0).abs() < 1e-9,
                "{} weights must sum to 100",
                m.name
            );
        }
        assert!(metric("harvest/1").is_ok());
        assert_eq!(metric("nope/9").unwrap_err().exit_code(), 2);
        assert!(metric_of_kind("harvest/1", "score").is_ok());
        assert_eq!(
            metric_of_kind("regime/1", "score").unwrap_err().exit_code(),
            2
        );
        assert!(metric_of_kind("regime/1", "state").is_ok());
    }

    #[test]
    fn harvest_1_scores_a_textbook_spread_at_the_top() {
        // 30% credit/width, 1% market, 6k OI, Δ −0.28, θ/ν 0.5, POP 0.75, 1.5 ATR below spot and under the 20d low.
        let s = leg("S", 100.0, 0.995, 1.005, -0.28, -0.05, 0.10, 6000, 0.75);
        let l = leg("L", 98.0, 0.395, 0.405, -0.20, -0.03, 0.08, 6000, 0.80);
        let c = Cushion {
            spot: 103.0,
            atr14: Some(2.0),
            low20: Some(101.0),
        };
        let r = harvest_1(&s, &l, Class::Index, Some(&c));
        assert!((r.total - 100.0).abs() < 1e-9, "{r:?}");
    }

    /// `pair_spreads` builds call spreads with long.strike = short.strike +
    /// width, so `short.strike <= long.strike` was true for EVERY call spread
    /// and they were all scored as puts. Two effects: the cushion factor could
    /// never be earned (calls capped at 90/100), and the emitted detail
    /// reported a NEGATIVE ATR distance as if measured correctly. No test in
    /// the suite scored a call spread, so it was invisible.
    #[test]
    fn a_call_spread_is_not_scored_as_a_put() {
        // Mirror image of the textbook put: identical geometry, 1.5 ATR of
        // cushion ABOVE spot.
        let s = leg("S", 100.0, 0.995, 1.005, 0.28, -0.05, 0.10, 6000, 0.75);
        let l = leg("L", 102.0, 0.395, 0.405, 0.20, -0.03, 0.08, 6000, 0.80);
        let c = Cushion {
            spot: 97.0,
            atr14: Some(2.0),
            low20: None,
        };
        let r = harvest_1(&s, &l, Class::Index, Some(&c));
        let f = r
            .factors
            .iter()
            .find(|f| f.name == "technical_cushion")
            .expect("cushion factor");
        assert!(
            f.points > 0.0,
            "a call spread 1.5 ATR above spot must earn cushion, got {f:?}"
        );
        assert!(
            !f.detail.contains('-'),
            "distance must be measured upward for a call: {}",
            f.detail
        );
    }

    #[test]
    fn harvest_1_matches_the_hand_scored_spy_740_738() {
        // 09-15 close marks: credit 0.38 on $2 (19% → 0), 1.3% market + 27k OI (20),
        // Δ −0.285 (15), POP ~0.70 (5); θ/ν and cushion unknown here → 0.
        let s = leg("S", 740.0, 6.58, 6.67, -0.285, -0.04, 0.60, 27_400, 0.70);
        let l = leg("L", 738.0, 6.20, 6.29, -0.269, -0.04, 0.58, 20_000, 0.72);
        let r = harvest_1(&s, &l, Class::Index, None);
        let by: std::collections::HashMap<_, _> =
            r.factors.iter().map(|f| (f.name, f.points)).collect();
        assert_eq!(by["credit_over_width"], 0.0);
        assert_eq!(by["liquidity"], 20.0);
        assert_eq!(by["delta_placement"], 15.0);
        assert_eq!(by["probability"], 5.0);
        assert_eq!(by["technical_cushion"], 0.0);
        assert_eq!(r.total, 40.0);
    }

    #[test]
    fn credit_over_width_is_linear_between_20_and_30_percent() {
        let mk = |credit: f64| {
            let s = leg(
                "S",
                100.0,
                credit + 0.5,
                credit + 0.5,
                -0.28,
                -0.05,
                0.1,
                100,
                0.7,
            );
            let l = leg("L", 98.0, 0.5, 0.5, -0.2, -0.03, 0.08, 100, 0.8);
            harvest_1(&s, &l, Class::Index, None).factors[0].points
        };
        assert_eq!(mk(0.38), 0.0);
        assert!((mk(0.40) - 15.0).abs() < 1e-9);
        assert!((mk(0.50) - 22.5).abs() < 1e-9);
        assert!((mk(0.60) - 30.0).abs() < 1e-9);
        assert!((mk(0.80) - 30.0).abs() < 1e-9);
    }

    #[test]
    fn leveraged_class_demands_a_higher_theta_vega_bar() {
        let s = leg("S", 62.0, 2.40, 2.46, -0.277, -0.08, 0.10, 1466, 0.70);
        let l = leg("L", 60.0, 1.98, 2.01, -0.23, -0.07, 0.09, 10_270, 0.75);
        let idx = harvest_1(&s, &l, Class::Index, None);
        let lev = harvest_1(&s, &l, Class::Leveraged, None);
        // ratio 0.8: full marks vs the index bar (0.25), half vs leveraged (1.5/2 = 0.75).
        assert_eq!(idx.factors[3].points, 15.0);
        assert_eq!(lev.factors[3].points, 7.5);
    }

    #[test]
    fn pairs_use_the_next_strike_out_and_skip_orphans() {
        let legs = vec![
            leg("A", 100.0, 1.0, 1.1, -0.3, -0.05, 0.1, 10, 0.7),
            leg("B", 98.0, 0.5, 0.6, -0.2, -0.03, 0.08, 10, 0.8),
            leg("C", 95.0, 0.2, 0.3, -0.1, -0.01, 0.05, 10, 0.9),
        ];
        let puts = pair_spreads(&legs, 2.0, true);
        assert_eq!(puts.len(), 1);
        assert_eq!((puts[0].0.id.as_str(), puts[0].1.id.as_str()), ("A", "B"));
        let calls = pair_spreads(&legs, 2.0, false);
        assert_eq!(calls.len(), 1);
        assert_eq!((calls[0].0.id.as_str(), calls[0].1.id.as_str()), ("B", "A"));
        assert!(pair_spreads(&legs, 3.0, true)
            .iter()
            .any(|(s, l)| s.id == "B" && l.id == "C"));
    }

    #[test]
    fn strike_step_is_the_local_grid() {
        assert_eq!(
            strike_step(&[95.0, 96.0, 97.0, 100.0, 105.0], 96.0),
            Some(1.0)
        );
        assert_eq!(strike_step(&[80.0, 85.0, 90.0, 95.0], 88.0), Some(5.0));
        assert_eq!(strike_step(&[50.0], 50.0), None);
    }

    #[test]
    fn atr_and_low_from_daily_bars() {
        // 25 flat-range bars: TR = 2 every day, low = 99 → ATR 2, low20 99.
        let bars: Vec<(f64, f64, f64)> = (0..25).map(|_| (101.0, 99.0, 100.0)).collect();
        let (atr, low) = atr_and_low(&bars, 14);
        assert_eq!(atr, Some(2.0));
        assert_eq!(low, Some(99.0));
        // Too short for the 20-day low or a 14-bar ATR.
        assert_eq!(atr_and_low(&bars[..10], 14), (None, None));
    }

    #[test]
    fn cursor_is_lifted_and_decoded_from_the_next_url() {
        let next = "http://edge-internal.example/options/instruments/?chain_symbol=XYZ&cursor=cD03MTkuMDAwMA%3D%3D&type=put";
        assert_eq!(cursor_from_next(next).as_deref(), Some("cD03MTkuMDAwMA=="));
        assert_eq!(cursor_from_next("http://x/y?chain_symbol=XYZ"), None);
        assert_eq!(cursor_from_next("no-query"), None);
    }

    #[test]
    fn civil_dates_round_trip_known_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_712), (2026, 9, 16));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
    }

    #[test]
    fn a_quote_row_parses_into_a_leg() {
        let q = json!({
            "instrument_id": "abc", "bid_price": "2.400000", "ask_price": "2.460000",
            "mark_price": "2.430000", "delta": "-0.277", "theta": "-0.08", "vega": "0.10",
            "implied_volatility": "0.65", "open_interest": 1466, "chance_of_profit_short": "0.70"
        });
        let leg = leg_from_quote(&q, 62.0).expect("leg");
        assert_eq!(leg.strike, 62.0);
        assert!((leg.mid() - 2.43).abs() < 1e-9);
        assert_eq!(leg.oi, 1466);
        assert!(leg_from_quote(&json!({"bid_price": "1"}), 1.0).is_none());
    }

    #[test]
    fn results_of_accepts_every_live_shape() {
        // Shapes observed live 2026-09-16: instruments/next, quotes results[].quote,
        // chains, historicals results[].bars, and a bare array.
        assert_eq!(
            results_of(&json!({"data": {"instruments": [1, 2], "next": "u"}})).len(),
            2
        );
        assert_eq!(
            results_of(&json!({"data": {"results": [{"quote": {}}]}})).len(),
            1
        );
        assert_eq!(results_of(&json!({"data": {"chains": [{}]}})).len(), 1);
        assert_eq!(
            results_of(&json!({"data": {"only_array": [1, 2, 3]}})).len(),
            3
        );
        assert_eq!(results_of(&json!([1])).len(), 1);
        assert!(results_of(&json!({"data": {"a": [1], "b": [2]}})).is_empty());
        assert_eq!(
            next_of(&json!({"data": {"instruments": [], "next": "http://x/?cursor=abc"}}))
                .as_deref(),
            Some("http://x/?cursor=abc")
        );
    }

    #[test]
    fn bars_accept_equity_and_index_spellings() {
        let eq = json!({"bars": [{"high_price": "2", "low_price": "1", "close_price": "1.5"}]});
        let ix = json!({"bars": [{"high_value": "2", "low_value": "1", "close_value": "1.5"}]});
        assert_eq!(bars_of(&eq), vec![(2.0, 1.0, 1.5)]);
        assert_eq!(bars_of(&ix), vec![(2.0, 1.0, 1.5)]);
        assert!(bars_of(&json!({"bars": [{"high_price": "0"}]})).is_empty());
        assert!(bars_of(&json!({})).is_empty());
    }

    /// A synthesized bar copies the prior session's OHLC, so keeping it makes
    /// TR re-count that range and shortens every window. Measured on the live
    /// SPX pull for the 45-day window `spot_and_bars` requests: 45 bars, 13
    /// interpolated (29%) — the "14-bar ATR" saw 10 real sessions and came out
    /// 12.6% low, moving the low20 gate in the permissive direction.
    ///
    /// The previous version of `bars_accept_equity_and_index_spellings`
    /// asserted the opposite: it fed an `interpolated: true` bar and required
    /// that it be KEPT, locking the defect in by test.
    #[test]
    fn interpolated_bars_are_dropped_they_are_not_observations() {
        let mixed = json!({"bars": [
            {"high_value": "2", "low_value": "1", "close_value": "1.5"},
            {"high_value": "2", "low_value": "1", "close_value": "1.5", "interpolated": true},
            {"high_value": "3", "low_value": "2", "close_value": "2.5"},
        ]});
        assert_eq!(bars_of(&mixed), vec![(2.0, 1.0, 1.5), (3.0, 2.0, 2.5)]);
        let all_fake = json!({"bars": [
            {"high_value": "2", "low_value": "1", "close_value": "1.5", "interpolated": true},
        ]});
        assert!(bars_of(&all_fake).is_empty());
    }

    #[test]
    fn the_metrics_dto_lists_every_registered_codename() {
        let v = metrics_dto();
        let names: Vec<&str> = v["metrics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, METRICS.iter().map(|m| m.name).collect::<Vec<_>>());
        assert_eq!(v["metrics"][0]["factors"]["credit_over_width"], 30.0);
    }
}
