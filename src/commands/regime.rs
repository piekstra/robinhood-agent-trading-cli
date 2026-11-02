//! `market regime` — state metrics for the S&P 500's market mode, derived
//! from SPX daily bars and the VIX (the two index series the MCP exposes).
//! Pure classification is unit-tested offline; [`market_regime`] is the one
//! networked entry point. `--history N` replays a metric over the last N
//! sessions so a definition can be checked against the tape before it is
//! trusted.
//!
//! Four published definitions (immutable — a change is a new version):
//!
//! **`regime/1`** — trend (close vs SMA20/SMA50), realized vol (RV10/RV20)
//! vs VIX (VRP), drawdown from the 20-day high, VIX vs its 5-day high →
//! `panic | stressed | calm-uptrend | calm-range`, plus book handling
//! (size multiplier, allowed classes, delta ceiling, open/no-open).
//!
//! **`regime/2`** — `regime/1` plus three refinements recorded in the
//! venture's REGIME.md study: (a) trend quality = the SMA20/SMA50 spread and
//! whether it is expanding (calm-uptrend requires expansion; a contracting
//! spread with the close under SMA20 is a "rolling-over" range that sizes at
//! 0.5); (b) a single-day drop trigger (SPX ≤ −3% on the day ⇒ at least
//! stressed; panic if VIX ≥ 25 as well); (c) asymmetric dwell — escalation is
//! immediate, de-escalation needs the calmer state on 3 consecutive sessions.
//!
//! **`regime/3`** — composite: the `regime/2` state overlaid with a
//! *structural* phase computed the way the operator's own long-horizon
//! system frames it (SMA50/SMA200 spread and its expansion, golden/death
//! cross, consecutive closes below SMA50, and a velocity override: ≥ 8% below
//! the 10-day high or a −3% day, with a 10-session cooldown). The composite
//! handling is the stricter of the two, and the structural phase sets the
//! preferred side (put spreads in bull phases, none/calls in bear phases).
//! Constants are as published in that study; they are not tuned here.
//! With fewer than 205 sessions the phase is `unknown`: restricted like a
//! bear phase, but opening is still allowed.
//!
//! **`regime/4`** — `regime/3` that fails closed. An `unknown` phase BLOCKS
//! opening outright, and the reason line states the opening decision for
//! `unknown` and `velocity-exit`. Identical to `regime/3` wherever the
//! structure is measurable.

use pk_cli_core::{output, CliError};
use serde_json::{json, Value};

use super::score::{f, metric_of_kind, results_of, rfc3339_days_ago, MetricDef};
use super::Ctx;

// ------------------------------------------------------------- inputs

/// Everything the three metrics classify, for one session. Percentages are
/// in percent units. Structural fields are `None` when the history is short.
#[derive(Clone, Debug, PartialEq)]
pub struct RegimeInputs {
    pub close: f64,
    pub sma20: f64,
    pub sma50: f64,
    pub sma20_slope_pct: f64,
    pub rv10: f64,
    pub rv20: f64,
    pub drawdown20_pct: f64,
    pub vix: f64,
    pub vix_high5: f64,
    pub vix_mean20: f64,
    // regime/2 additions
    pub spread20_50_pct: f64,
    pub spread20_50_expanding: bool,
    pub ret1_pct: f64,
    // regime/3 structural (SMA200-based) additions
    pub sma200: Option<f64>,
    pub spread50_200_pct: Option<f64>,
    pub spread50_200_expanding: bool,
    pub days_below_sma50: u32,
    pub drop_from_high10_pct: f64,
}

/// A classification plus the book handling it implies.
#[derive(Clone, Debug, PartialEq)]
pub struct Regime {
    pub state: &'static str,
    pub vol_easing: bool,
    pub vrp: f64,
    pub size_multiplier: f64,
    pub allowed_classes: &'static [&'static str],
    pub max_short_delta: f64,
    pub open_allowed: bool,
    pub reasons: Vec<String>,
    /// Structural definitions (regime/3, regime/4) only: the phase and the
    /// preferred side.
    pub structural_phase: Option<StructuralPhase>,
    pub preferred_side: Option<&'static str>,
}

fn handling(state: &str, vol_easing: bool) -> (f64, &'static [&'static str], f64, bool) {
    match state {
        "calm-uptrend" => (1.0, &["index", "sector", "leveraged"], 0.35, true),
        "calm-range" => (0.75, &["index", "sector", "leveraged"], 0.30, true),
        "rolling-over" => (0.5, &["index", "sector"], 0.25, true),
        "stressed" => (0.5, &["index"], 0.25, vol_easing),
        _ => (0.5, &["index"], 0.20, vol_easing),
    }
}

fn build(state: &'static str, vol_easing: bool, vrp: f64, reasons: Vec<String>) -> Regime {
    let (size_multiplier, allowed_classes, max_short_delta, open_allowed) =
        handling(state, vol_easing);
    Regime {
        state,
        vol_easing,
        vrp,
        size_multiplier,
        allowed_classes,
        max_short_delta,
        open_allowed,
        reasons,
        structural_phase: None,
        preferred_side: None,
    }
}

// ------------------------------------------------------------ regime/1

/// `regime/1` — see the module docs for the published definition.
pub fn regime_1(i: &RegimeInputs) -> Regime {
    let vrp = i.vix - i.rv20;
    let vol_easing = i.vix_high5 > 0.0 && i.vix <= 0.90 * i.vix_high5;
    let mut reasons = Vec::new();
    let state = if i.vix >= 30.0 || i.drawdown20_pct <= -7.0 {
        reasons.push(format!(
            "panic: VIX {:.1} ≥ 30 or drawdown {:.1}% ≤ −7%",
            i.vix, i.drawdown20_pct
        ));
        "panic"
    } else if i.vix >= 22.0
        || i.drawdown20_pct <= -4.0
        || (i.close < i.sma50 && i.rv10 > 1.3 * i.rv20)
    {
        reasons.push(format!(
            "stressed: VIX {:.1} (≥22?), drawdown {:.1}% (≤−4?), close {} SMA50 with RV10 {:.1} vs 1.3×RV20 {:.1}",
            i.vix,
            i.drawdown20_pct,
            if i.close < i.sma50 { "<" } else { "≥" },
            i.rv10,
            1.3 * i.rv20
        ));
        "stressed"
    } else if i.close > i.sma20 && i.sma20 > i.sma50 && vrp > 0.0 {
        reasons.push(format!(
            "calm-uptrend: close {:.0} > SMA20 {:.0} > SMA50 {:.0}; VRP {:+.1} pts",
            i.close, i.sma20, i.sma50, vrp
        ));
        "calm-uptrend"
    } else {
        reasons.push(format!(
            "calm-range: close {:.0} vs SMA20 {:.0} / SMA50 {:.0}; VRP {:+.1} pts; slope {:+.2}%",
            i.close, i.sma20, i.sma50, vrp, i.sma20_slope_pct
        ));
        "calm-range"
    };
    if vol_easing {
        reasons.push(format!(
            "vol easing: VIX {:.1} ≤ 90% of 5-day high {:.1}",
            i.vix, i.vix_high5
        ));
    }
    build(state, vol_easing, vrp, reasons)
}

// ------------------------------------------------------------ regime/2

/// Severity order used by the dwell rule (higher = more restrictive).
fn severity(state: &str) -> u8 {
    match state {
        "calm-uptrend" => 0,
        "calm-range" => 1,
        "rolling-over" => 2,
        "stressed" => 3,
        _ => 4,
    }
}

/// The raw `regime/2` state for one session (no dwell).
pub fn regime_2_raw(i: &RegimeInputs) -> Regime {
    let vrp = i.vix - i.rv20;
    let vol_easing = i.vix_high5 > 0.0 && i.vix <= 0.90 * i.vix_high5;
    let mut reasons = Vec::new();
    let state = if i.vix >= 30.0
        || i.drawdown20_pct <= -7.0
        || (i.ret1_pct <= -3.0 && i.vix >= 25.0)
    {
        reasons.push(format!(
            "panic: VIX {:.1}, drawdown {:.1}%, 1-day {:+.2}%",
            i.vix, i.drawdown20_pct, i.ret1_pct
        ));
        "panic"
    } else if i.vix >= 22.0
        || i.drawdown20_pct <= -4.0
        || i.ret1_pct <= -3.0
        || (i.close < i.sma50 && i.rv10 > 1.3 * i.rv20)
    {
        reasons.push(format!(
            "stressed: VIX {:.1}, drawdown {:.1}%, 1-day {:+.2}%, close {} SMA50, RV10 {:.1} vs 1.3×RV20 {:.1}",
            i.vix,
            i.drawdown20_pct,
            i.ret1_pct,
            if i.close < i.sma50 { "<" } else { "≥" },
            i.rv10,
            1.3 * i.rv20
        ));
        "stressed"
    } else if i.close > i.sma20 && i.sma20 > i.sma50 && i.spread20_50_expanding && vrp > 0.0 {
        reasons.push(format!(
            "calm-uptrend: close {:.0} > SMA20 {:.0} > SMA50 {:.0}, spread {:+.2}% expanding; VRP {:+.1}",
            i.close, i.sma20, i.sma50, i.spread20_50_pct, vrp
        ));
        "calm-uptrend"
    } else if i.close < i.sma20 && !i.spread20_50_expanding {
        reasons.push(format!(
            "rolling-over: close {:.0} < SMA20 {:.0} with the 20/50 spread {:+.2}% contracting",
            i.close, i.sma20, i.spread20_50_pct
        ));
        "rolling-over"
    } else {
        reasons.push(format!(
            "calm-range: close {:.0} vs SMA20 {:.0} / SMA50 {:.0}; spread {:+.2}% {}; VRP {:+.1}",
            i.close,
            i.sma20,
            i.sma50,
            i.spread20_50_pct,
            if i.spread20_50_expanding {
                "expanding"
            } else {
                "contracting"
            },
            vrp
        ));
        "calm-range"
    };
    if vol_easing {
        reasons.push(format!(
            "vol easing: VIX {:.1} ≤ 90% of 5-day high {:.1}",
            i.vix, i.vix_high5
        ));
    }
    build(state, vol_easing, vrp, reasons)
}

/// Apply the asymmetric dwell: `raw` is the sequence of raw states, oldest
/// first, ending with today. Escalation is immediate; a calmer state is
/// reported only after it has held for `dwell` consecutive sessions.
pub fn apply_dwell(raw: &[&'static str], dwell: usize) -> &'static str {
    let mut held = "unknown";
    for (k, &today) in raw.iter().enumerate() {
        if k == 0 || severity(today) >= severity(held) {
            held = today;
            continue;
        }
        let start = (k + 1).saturating_sub(dwell);
        let tail = &raw[start..=k];
        if tail.len() >= dwell && tail.iter().all(|s| severity(s) <= severity(today)) {
            held = today;
        }
    }
    held
}

/// `regime/2` for the session whose inputs are `i`, given the raw states of
/// the preceding sessions (oldest first).
pub fn regime_2(i: &RegimeInputs, raw_history: &[&'static str]) -> Regime {
    let mut r = regime_2_raw(i);
    let mut seq: Vec<&'static str> = raw_history.to_vec();
    seq.push(r.state);
    let held = apply_dwell(&seq, 3);
    if held != r.state {
        r.reasons.push(format!(
            "dwell: raw {} but {} held — a calmer state needs 3 consecutive sessions",
            r.state, held
        ));
        let (sm, ac, md, oa) = handling(held, r.vol_easing);
        r.state = held;
        r.size_multiplier = sm;
        r.allowed_classes = ac;
        r.max_short_delta = md;
        r.open_allowed = oa;
    }
    r
}

// ------------------------------------------------------------ regime/3

/// A published regime definition, resolved ONCE from its registry entry.
/// Every site that used to branch on the codename string (`replay`, the
/// history depth, the overlay) reads this instead, so publishing `regime/5`
/// edits `of` and nothing falls through to an older definition under a new
/// label: a registered name with no arm here is a usage error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegimeVersion {
    V1,
    V2,
    V3,
    V4,
}

/// What differs between the structural (50/200) definitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuralRules {
    /// What an unmeasurable structure (< 205 sessions) does to opening.
    pub unknown_opening: Opening,
    /// Whether the structural reason line states the opening decision.
    pub reason_states_opening: bool,
}

impl RegimeVersion {
    pub fn of(name: &str) -> Result<Self, CliError> {
        match name {
            "regime/1" => Ok(Self::V1),
            "regime/2" => Ok(Self::V2),
            "regime/3" => Ok(Self::V3),
            "regime/4" => Ok(Self::V4),
            other => Err(CliError::Usage(format!(
                "state metric {other} is registered but has no regime implementation"
            ))),
        }
    }

    /// `None` for the definitions with no structural overlay.
    pub fn structural(self) -> Option<StructuralRules> {
        match self {
            Self::V1 | Self::V2 => None,
            Self::V3 => Some(StructuralRules {
                unknown_opening: Opening::Open,
                reason_states_opening: false,
            }),
            Self::V4 => Some(StructuralRules {
                unknown_opening: Opening::Blocked,
                reason_states_opening: true,
            }),
        }
    }

    /// Calendar days of history to request: the 50/200 overlay needs ≥ 205
    /// sessions.
    pub fn base_days(self) -> i64 {
        if self.structural().is_some() {
            320
        } else {
            120
        }
    }

    /// Calendar days of history `market regime` requests for `history`
    /// replayed sessions (≈ 1.45 calendar days per session on top of the
    /// base window).
    pub fn lookback_days(self, history: usize) -> i64 {
        self.base_days() + (history as f64 * 1.45) as i64
    }
}

/// The structural phase as a closed type. It was a bare `&'static str` whose
/// policy lived in three separate conditionals, and `unknown` was a value
/// nobody had added to the bearish set — regime/3 failed OPEN and the
/// compiler could not say so. A new variant now fails to compile until
/// `policy` answers every dimension for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralPhase {
    FullBull,
    GoldenCrossConfirmed,
    EarlyBull,
    BullWarning,
    ConvergenceExit,
    CorrectionExit,
    BearHold,
    BearDeep,
    VelocityExit,
    Unknown,
}

/// What a phase does to opening a new position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opening {
    Open,
    RequiresVolEasing,
    Blocked,
}

pub struct PhasePolicy {
    /// Caps size at 0.5, short delta at 0.25 and classes at index-only.
    pub restricted: bool,
    pub side: &'static str,
    pub opening: Opening,
}

impl StructuralPhase {
    /// The wire value in the `structural_phase` DTO field. Immutable.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FullBull => "full-bull",
            Self::GoldenCrossConfirmed => "golden-cross-confirmed",
            Self::EarlyBull => "early-bull",
            Self::BullWarning => "bull-warning",
            Self::ConvergenceExit => "convergence-exit",
            Self::CorrectionExit => "correction-exit",
            Self::BearHold => "bear-hold",
            Self::BearDeep => "bear-deep",
            Self::VelocityExit => "velocity-exit",
            Self::Unknown => "unknown",
        }
    }

    /// A published definition never changes: `regime/3` keeps what it
    /// shipped with — an unmeasurable structure is restricted but may still
    /// open — and `regime/4` refuses to open on it. The rule, not the version
    /// number, is what arrives here.
    pub fn policy(self, rules: &StructuralRules) -> PhasePolicy {
        let (restricted, opening) = match self {
            Self::FullBull | Self::GoldenCrossConfirmed | Self::EarlyBull | Self::BullWarning => {
                (false, Opening::Open)
            }
            Self::ConvergenceExit | Self::CorrectionExit | Self::BearHold | Self::BearDeep => {
                (true, Opening::Open)
            }
            Self::VelocityExit => (true, Opening::RequiresVolEasing),
            // Fewer than 205 sessions: the 50/200 spread cannot be computed.
            // An unmeasurable structure is not a benign one. The live margin
            // is 15 sessions (220 available), so a short feed response
            // reaches this.
            Self::Unknown => (true, rules.unknown_opening),
        };
        PhasePolicy {
            restricted,
            side: if restricted {
                "none-or-call-spreads"
            } else {
                "put-spreads"
            },
            opening,
        }
    }
}

/// The structural phase (SMA50/SMA200 framing) with the velocity override.
/// `velocity_active` carries the 10-session cooldown from the caller.
pub fn structural_phase(i: &RegimeInputs, velocity_active: bool) -> StructuralPhase {
    if velocity_active {
        return StructuralPhase::VelocityExit;
    }
    let Some(spread) = i.spread50_200_pct else {
        return StructuralPhase::Unknown;
    };
    let golden = spread > 0.0;
    let expanding = i.spread50_200_expanding;
    if golden {
        if spread > 3.0 && expanding {
            StructuralPhase::FullBull
        } else if spread <= 1.0 && !expanding {
            StructuralPhase::ConvergenceExit
        } else if i.days_below_sma50 >= 10 {
            StructuralPhase::CorrectionExit
        } else if !expanding {
            StructuralPhase::BullWarning
        } else {
            StructuralPhase::GoldenCrossConfirmed
        }
    } else if (spread >= -3.0 && expanding && i.close > i.sma50) || (spread >= -1.0 && expanding) {
        StructuralPhase::EarlyBull
    } else if spread < -3.0 {
        StructuralPhase::BearDeep
    } else {
        StructuralPhase::BearHold
    }
}

/// Velocity trigger for one session (no cooldown).
pub fn velocity_trigger(i: &RegimeInputs) -> bool {
    i.drop_from_high10_pct >= 8.0 || i.ret1_pct <= -3.0
}

/// `regime/3`: the `regime/2` state overlaid with the structural phase.
#[cfg(test)]
pub fn regime_3(i: &RegimeInputs, raw_history: &[&'static str], velocity_active: bool) -> Regime {
    let rules = RegimeVersion::V3
        .structural()
        .expect("regime/3 is structural");
    structural_overlay(i, raw_history, velocity_active, &rules)
}

/// `regime/4`: `regime/3` that FAILS CLOSED. With fewer than 205 sessions the
/// 50/200 spread cannot be computed; `regime/3` caps size and class on that
/// but still authorises opening. `regime/4` blocks opening outright and says
/// so in `reasons`, for the unknown phase and for a velocity exit alike.
#[cfg(test)]
pub fn regime_4(i: &RegimeInputs, raw_history: &[&'static str], velocity_active: bool) -> Regime {
    let rules = RegimeVersion::V4
        .structural()
        .expect("regime/4 is structural");
    structural_overlay(i, raw_history, velocity_active, &rules)
}

fn structural_overlay(
    i: &RegimeInputs,
    raw_history: &[&'static str],
    velocity_active: bool,
    rules: &StructuralRules,
) -> Regime {
    let mut r = regime_2(i, raw_history);
    let phase = structural_phase(i, velocity_active);
    let policy = phase.policy(rules);
    let (name, side) = (phase.as_str(), policy.side);
    if policy.restricted {
        r.size_multiplier = r.size_multiplier.min(0.5);
        r.max_short_delta = r.max_short_delta.min(0.25);
        if r.allowed_classes.len() > 1 {
            r.allowed_classes = &["index"];
        }
        // The EFFECT first, on its own, for every definition.
        match policy.opening {
            Opening::Open => {}
            Opening::RequiresVolEasing => r.open_allowed = r.open_allowed && r.vol_easing,
            Opening::Blocked => r.open_allowed = false,
        }
        // Then the TEXT, which is pure. The reason line is the only narrative
        // surface: without the opening decision a reader of `reasons`
        // concludes opening is permitted at half size. regime/3's published
        // line carries no such clause and keeps none.
        let opening = if rules.reason_states_opening {
            opening_clause(policy.opening, r.open_allowed)
        } else {
            ""
        };
        r.reasons.push(format!(
            "structural {name}:{opening} size capped 0.5, index only, delta ≤ 0.25; preferred side {side}"
        ));
    } else {
        r.reasons.push(format!(
            "structural {name}: no additional restriction; side {side}"
        ));
    }
    r.structural_phase = Some(phase);
    r.preferred_side = Some(side);
    r
}

fn opening_clause(opening: Opening, open_allowed: bool) -> &'static str {
    match (opening, open_allowed) {
        (Opening::Open, _) => "",
        (Opening::RequiresVolEasing, true) => " opening allowed only because vol is easing;",
        (Opening::RequiresVolEasing, false) => " opening blocked until vol eases;",
        (Opening::Blocked, _) => {
            " fewer than 205 sessions, structure unmeasurable — opening blocked;"
        }
    }
}

// ------------------------------------------------------------ pure inputs

fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        0.0
    } else {
        xs.iter().sum::<f64>() / xs.len() as f64
    }
}

/// Annualized close-to-close realized volatility (%) over the last `n` returns.
pub fn realized_vol(closes: &[f64], n: usize) -> f64 {
    if closes.len() < n + 1 {
        return 0.0;
    }
    let tail = &closes[closes.len() - n - 1..];
    let rets: Vec<f64> = tail.windows(2).map(|w| (w[1] / w[0]).ln()).collect();
    let m = mean(&rets);
    let var =
        rets.iter().map(|r| (r - m).powi(2)).sum::<f64>() / (rets.len() as f64 - 1.0).max(1.0);
    var.sqrt() * (252f64).sqrt() * 100.0
}

/// Consecutive sessions (ending at the last close) with close < SMA50.
fn days_below_sma50(closes: &[f64]) -> u32 {
    let mut days = 0;
    let mut i = closes.len();
    while i >= 50 {
        let sma = mean(&closes[i - 50..i]);
        if closes[i - 1] < sma {
            days += 1;
            i -= 1;
        } else {
            break;
        }
    }
    days
}

/// Build the inputs for the session ending at the last element of
/// `spx_closes` (oldest first, ≥ 61 closes; ≥ 205 for the structural
/// fields), with VIX closes (oldest first, the last being today's level).
pub fn inputs_from(spx_closes: &[f64], vix_closes: &[f64]) -> Result<RegimeInputs, CliError> {
    if spx_closes.len() < 61 {
        return Err(CliError::Upstream(format!(
            "regime metrics need ≥ 61 SPX daily closes (got {})",
            spx_closes.len()
        )));
    }
    if vix_closes.is_empty() {
        return Err(CliError::Upstream("regime metrics need a VIX level".into()));
    }
    let n = spx_closes.len();
    let close = spx_closes[n - 1];
    let sma20 = mean(&spx_closes[n - 20..]);
    let sma50 = mean(&spx_closes[n - 50..]);
    let sma20_prev = mean(&spx_closes[n - 30..n - 10]);
    let sma20_slope_pct = (sma20 / sma20_prev - 1.0) * 100.0;
    let high20 = spx_closes[n - 20..]
        .iter()
        .cloned()
        .fold(f64::MIN, f64::max);
    let drawdown20_pct = (close / high20 - 1.0) * 100.0;
    let spread20_50_pct = (sma20 / sma50 - 1.0) * 100.0;
    let spread20_50_prev =
        (mean(&spx_closes[n - 25..n - 5]) / mean(&spx_closes[n - 55..n - 5]) - 1.0) * 100.0;
    let ret1_pct = (close / spx_closes[n - 2] - 1.0) * 100.0;
    let (sma200, spread50_200_pct, spread50_200_expanding) = if n >= 205 {
        let s200 = mean(&spx_closes[n - 200..]);
        let sp = (sma50 / s200 - 1.0) * 100.0;
        let sp_prev =
            (mean(&spx_closes[n - 55..n - 5]) / mean(&spx_closes[n - 205..n - 5]) - 1.0) * 100.0;
        (Some(s200), Some(sp), sp > sp_prev)
    } else {
        (None, None, false)
    };
    let high10 = spx_closes[n - 10..]
        .iter()
        .cloned()
        .fold(f64::MIN, f64::max);
    let vm = vix_closes.len();
    let vix = vix_closes[vm - 1];
    let vix_high5 = vix_closes[vm.saturating_sub(5)..]
        .iter()
        .cloned()
        .fold(f64::MIN, f64::max);
    let vix_mean20 = mean(&vix_closes[vm.saturating_sub(20)..]);
    Ok(RegimeInputs {
        close,
        sma20,
        sma50,
        sma20_slope_pct,
        rv10: realized_vol(spx_closes, 10),
        rv20: realized_vol(spx_closes, 20),
        drawdown20_pct,
        vix,
        vix_high5,
        vix_mean20,
        spread20_50_pct,
        spread20_50_expanding: spread20_50_pct > spread20_50_prev,
        ret1_pct,
        sma200,
        spread50_200_pct,
        spread50_200_expanding,
        days_below_sma50: days_below_sma50(spx_closes),
        drop_from_high10_pct: (1.0 - close / high10) * 100.0,
    })
}

/// Classify one metric over the last `history` sessions of aligned close
/// series (the VIX series is aligned by trailing index). Returns (inputs,
/// regime) per session, oldest first; dwell and the velocity cooldown are
/// stateful across the replay.
pub fn replay(
    version: RegimeVersion,
    spx: &[f64],
    vix: &[f64],
    history: usize,
) -> Result<Vec<(RegimeInputs, Regime)>, CliError> {
    let n = spx.len();
    let m = vix.len();
    let first = (n.saturating_sub(history) + 1).max(61);
    if first > n {
        return Err(CliError::Upstream(
            "not enough closes for the requested history".into(),
        ));
    }
    let mut out = Vec::new();
    let mut raw: Vec<&'static str> = Vec::new();
    let mut vel_cooldown: u32 = 0;
    for end in first..=n {
        let vend = m.saturating_sub(n - end);
        if vend == 0 {
            continue;
        }
        let i = inputs_from(&spx[..end], &vix[..vend])?;
        let r = match version {
            RegimeVersion::V1 => regime_1(&i),
            RegimeVersion::V2 => {
                let r = regime_2(&i, &raw);
                raw.push(regime_2_raw(&i).state);
                r
            }
            RegimeVersion::V3 | RegimeVersion::V4 => {
                if velocity_trigger(&i) {
                    vel_cooldown = 10;
                } else {
                    vel_cooldown = vel_cooldown.saturating_sub(1);
                }
                let rules = version.structural().expect("V3/V4 are structural");
                let r = structural_overlay(&i, &raw, vel_cooldown > 0, &rules);
                raw.push(regime_2_raw(&i).state);
                r
            }
        };
        out.push((i, r));
    }
    if out.is_empty() {
        return Err(CliError::Upstream("no sessions classified".into()));
    }
    Ok(out)
}

/// Non-interpolated daily `(YYYY-MM-DD, close)` pairs, oldest first, from an
/// index historicals row. The feed fills weekends, holidays and the still-open
/// session with `interpolated: true` copies of the prior close — those are
/// dropped, and the live level stands in for today (see [`closes_with_live`]).
pub fn real_closes(row: &Value) -> Vec<(String, f64)> {
    row.get("bars")
        .and_then(Value::as_array)
        .map(|bars| {
            bars.iter()
                .filter(|b| b.get("interpolated").and_then(Value::as_bool) != Some(true))
                .filter_map(|b| {
                    let c = f(b, "close_value");
                    let c = if c > 0.0 { c } else { f(b, "close_price") };
                    let d = b
                        .get("begins_at")
                        .and_then(Value::as_str)?
                        .get(..10)?
                        .to_string();
                    (c > 0.0).then_some((d, c))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The close series with the live level appended as today's close when the
/// last real bar predates `today`.
pub fn closes_with_live(bars: &[(String, f64)], today: &str, live: f64) -> Vec<f64> {
    let mut closes: Vec<f64> = bars.iter().map(|(_, c)| *c).collect();
    match bars.last() {
        Some((d, _)) if d.as_str() >= today => {}
        // A live quote that EQUALS the last settled close is that same close,
        // not a new session. Outside regular hours the cash index is not
        // computed, so the quote returns the prior settlement — and `today` is
        // the UTC date, which rolls hours before the ET session does. The
        // 08:13 ET pre-market job therefore appended a duplicate bar every
        // weekday: ret1 read +0.00% where the true value was +1.14%, RV10 fell
        // 15%, and a -3.4% day would have armed neither the regime/2 stressed
        // trigger nor the regime/3 velocity override.
        Some((_, c)) if (*c - live).abs() < 1e-9 => {}
        _ if live > 0.0 => closes.push(live),
        _ => {}
    }
    closes
}

#[cfg(test)]
mod phantom_tests {
    use super::closes_with_live;

    /// The pre-market cron runs at 08:13 ET, when the cash index has not been
    /// computed and the quote returns the prior settlement, while `today` is
    /// already the next UTC date. The guard `d >= today` was false, so the
    /// settled close was appended a second time.
    #[test]
    fn a_live_quote_equal_to_the_last_close_is_not_a_new_session() {
        let bars = vec![
            ("2026-09-16".to_string(), 7550.0),
            ("2026-09-17".to_string(), 7637.76),
        ];
        let out = closes_with_live(&bars, "2026-09-18", 7637.76);
        assert_eq!(
            out,
            vec![7550.0, 7637.76],
            "the settled close was duplicated"
        );
    }

    #[test]
    fn a_genuinely_new_live_print_is_still_appended() {
        let bars = vec![("2026-09-17".to_string(), 7637.76)];
        let out = closes_with_live(&bars, "2026-09-18", 7650.0);
        assert_eq!(out, vec![7637.76, 7650.0]);
    }
}

fn r2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn inputs_dto(i: &RegimeInputs) -> Value {
    json!({
        "close": i.close,
        "sma20": r2(i.sma20),
        "sma50": r2(i.sma50),
        "sma200": i.sma200.map(r2),
        "sma20_slope_pct": r2(i.sma20_slope_pct),
        "spread20_50_pct": r2(i.spread20_50_pct),
        "spread20_50_expanding": i.spread20_50_expanding,
        "spread50_200_pct": i.spread50_200_pct.map(r2),
        "spread50_200_expanding": i.spread50_200_expanding,
        "days_below_sma50": i.days_below_sma50,
        "ret1_pct": r2(i.ret1_pct),
        "drop_from_high10_pct": r2(i.drop_from_high10_pct),
        "rv10_pct": r2(i.rv10),
        "rv20_pct": r2(i.rv20),
        "vix": i.vix,
        "vix_high5": i.vix_high5,
        "vix_mean20": r2(i.vix_mean20),
        "vrp_pts": r2(i.vix - i.rv20),
        "drawdown20_pct": r2(i.drawdown20_pct),
    })
}

fn regime_dto(r: &Regime) -> Value {
    json!({
        "state": r.state,
        "structural_phase": r.structural_phase.map(StructuralPhase::as_str),
        "preferred_side": r.preferred_side,
        "vol_easing": r.vol_easing,
        "handling": {
            "size_multiplier": r.size_multiplier,
            "allowed_classes": r.allowed_classes,
            "max_short_delta": r.max_short_delta,
            "open_allowed": r.open_allowed,
        },
        "reasons": r.reasons,
    })
}

/// `market regime [--metric regime/1|2|3 --symbol SPX --vol-symbol VIX
/// --history N]` → `get_indexes` + `get_index_quotes` + `get_index_historicals`
/// (×2). READ.
pub fn market_regime(
    ctx: &Ctx,
    metric_name: &str,
    symbol: &str,
    vol_symbol: &str,
    history: usize,
) -> Result<(), CliError> {
    let def: &MetricDef = metric_of_kind(metric_name, "state")?;
    // Both argument checks BEFORE the keychain and the network: a registered
    // metric with no implementation must exit 2 without prompting for a token.
    let version = RegimeVersion::of(def.name)?;
    let symbol = symbol.trim().to_uppercase();
    let vol_symbol = vol_symbol.trim().to_uppercase();
    let mut client = ctx.client()?;

    let idx = client.read(
        "get_indexes",
        json!({ "symbols": format!("{symbol},{vol_symbol}") }),
    )?;
    let inner = idx.get("data").unwrap_or(&idx);
    let id_of = |sym: &str| -> Result<String, CliError> {
        inner
            .get("indexes")
            .and_then(Value::as_array)
            .and_then(|a| {
                a.iter()
                    .find(|i| i.get("symbol").and_then(Value::as_str) == Some(sym))
            })
            .and_then(|i| i.get("id").and_then(Value::as_str))
            .map(str::to_string)
            .ok_or_else(|| CliError::NotFound(format!("index {sym} not found")))
    };
    let (spx_id, vix_id) = (id_of(&symbol)?, id_of(&vol_symbol)?);

    let quotes = client.read(
        "get_index_quotes",
        json!({ "instrument_ids": [spx_id.clone(), vix_id.clone()] }),
    )?;
    let rows = results_of(&quotes);
    let level_of = |id: &str| -> Option<f64> {
        rows.iter()
            .find(|q| q.get("instrument_id").and_then(Value::as_str) == Some(id))
            .map(|q| f(q, "value"))
            .filter(|v| *v > 0.0)
    };
    let vix_now = level_of(&vix_id)
        .ok_or_else(|| CliError::Upstream(format!("no level for {vol_symbol}")))?;
    let spx_now = level_of(&spx_id).unwrap_or(0.0);
    let today = rfc3339_days_ago(0)[..10].to_string();

    // regime/3 needs SMA200 (+ history).
    let days = version.lookback_days(history);
    let hist = |client: &mut crate::mcp::McpClient,
                id: &str,
                days: i64|
     -> Result<Vec<(String, f64)>, CliError> {
        let h = client.read(
            "get_index_historicals",
            json!({ "instrument_ids": [id], "start_time": rfc3339_days_ago(days), "interval": "day" }),
        )?;
        Ok(results_of(&h).first().map(real_closes).unwrap_or_default())
    };
    let spx_closes = closes_with_live(&hist(&mut client, &spx_id, days)?, &today, spx_now);
    let vix_closes = closes_with_live(&hist(&mut client, &vix_id, days)?, &today, vix_now);

    let run = replay(version, &spx_closes, &vix_closes, history.max(1))?;
    let (i, r) = run.last().expect("replay is non-empty");
    let mut dto = json!({
        "metric": def.name,
        "symbols": { "index": symbol, "vol": vol_symbol },
        "sessions_available": spx_closes.len(),
    });
    for (k, v) in regime_dto(r).as_object().expect("object") {
        dto[k] = v.clone();
    }
    dto["inputs"] = inputs_dto(i);
    if history > 1 {
        let n = run.len();
        dto["history"] = Value::Array(
            run.iter()
                .enumerate()
                .map(|(k, (i, r))| {
                    json!({
                        "sessions_ago": n - 1 - k,
                        "close": i.close,
                        "vix": i.vix,
                        "state": r.state,
                        "structural_phase": r.structural_phase.map(StructuralPhase::as_str),
                        "size": r.size_multiplier,
                        "open_allowed": r.open_allowed,
                    })
                })
                .collect(),
        );
    }
    output::emit(ctx.common.json, "market-regime", dto, output::render);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> RegimeInputs {
        RegimeInputs {
            close: 7585.0,
            sma20: 7520.0,
            sma50: 7400.0,
            sma20_slope_pct: 0.8,
            rv10: 9.0,
            rv20: 10.0,
            drawdown20_pct: -0.5,
            vix: 17.2,
            vix_high5: 18.0,
            vix_mean20: 16.5,
            spread20_50_pct: 1.6,
            spread20_50_expanding: true,
            ret1_pct: 0.2,
            sma200: Some(7100.0),
            spread50_200_pct: Some(4.2),
            spread50_200_expanding: true,
            days_below_sma50: 0,
            drop_from_high10_pct: 0.5,
        }
    }

    #[test]
    fn regime_1_calm_uptrend_is_full_size_all_classes() {
        let r = regime_1(&base());
        assert_eq!(r.state, "calm-uptrend");
        assert_eq!(r.size_multiplier, 1.0);
        assert!(r.open_allowed);
        assert_eq!(r.allowed_classes.len(), 3);
        assert!((r.vrp - 7.2).abs() < 1e-9);
    }

    #[test]
    fn regime_1_negative_vrp_or_broken_trend_is_calm_range() {
        let mut i = base();
        i.rv20 = 18.0;
        assert_eq!(regime_1(&i).state, "calm-range");
        let mut j = base();
        j.close = 7500.0;
        let r = regime_1(&j);
        assert_eq!(r.state, "calm-range");
        assert_eq!(r.size_multiplier, 0.75);
        assert_eq!(r.max_short_delta, 0.30);
    }

    #[test]
    fn regime_1_stressed_opens_only_when_vol_is_easing() {
        let mut i = base();
        i.vix = 24.0;
        i.vix_high5 = 25.0;
        let r = regime_1(&i);
        assert_eq!(r.state, "stressed");
        assert!(!r.open_allowed);
        i.vix_high5 = 30.0;
        let r = regime_1(&i);
        assert!(r.vol_easing && r.open_allowed);
        assert_eq!(r.allowed_classes, &["index"]);
        assert_eq!(r.max_short_delta, 0.25);
    }

    #[test]
    fn regime_1_panic_by_vix_or_drawdown() {
        let mut i = base();
        i.vix = 31.0;
        i.vix_high5 = 31.0;
        assert_eq!(regime_1(&i).state, "panic");
        assert!(!regime_1(&i).open_allowed);
        let mut j = base();
        j.drawdown20_pct = -7.5;
        assert_eq!(regime_1(&j).state, "panic");
    }

    #[test]
    fn regime_2_single_day_drop_and_rolling_over() {
        let mut i = base();
        i.ret1_pct = -3.2;
        assert_eq!(regime_2_raw(&i).state, "stressed");
        i.vix = 26.0;
        assert_eq!(regime_2_raw(&i).state, "panic");
        let mut j = base();
        j.close = 7500.0; // below SMA20
        j.spread20_50_expanding = false;
        let r = regime_2_raw(&j);
        assert_eq!(r.state, "rolling-over");
        assert_eq!(r.size_multiplier, 0.5);
        assert_eq!(r.allowed_classes, &["index", "sector"]);
        let mut k = base();
        k.spread20_50_expanding = false;
        assert_eq!(regime_2_raw(&k).state, "calm-range");
    }

    #[test]
    fn dwell_escalates_immediately_and_de_escalates_after_three() {
        assert_eq!(apply_dwell(&["calm-uptrend", "stressed"], 3), "stressed");
        assert_eq!(apply_dwell(&["stressed", "calm-range"], 3), "stressed");
        assert_eq!(
            apply_dwell(&["stressed", "calm-range", "calm-range"], 3),
            "stressed"
        );
        assert_eq!(
            apply_dwell(&["stressed", "calm-range", "calm-range", "calm-range"], 3),
            "calm-range"
        );
        assert_eq!(
            apply_dwell(
                &[
                    "stressed",
                    "calm-range",
                    "stressed",
                    "calm-range",
                    "calm-range"
                ],
                3
            ),
            "stressed"
        );
        let held = regime_2(&base(), &["stressed", "stressed"]);
        assert_eq!(held.state, "stressed", "{:?}", held.reasons);
        assert_eq!(held.size_multiplier, 0.5);
        let free = regime_2(&base(), &["stressed", "calm-uptrend", "calm-uptrend"]);
        assert_eq!(free.state, "calm-uptrend");
        let mut i = base();
        i.vix = 24.0;
        assert_eq!(regime_2(&i, &["calm-uptrend"]).state, "stressed");
    }

    #[test]
    fn structural_phases_follow_the_published_table() {
        let mut i = base();
        assert_eq!(structural_phase(&i, false).as_str(), "full-bull");
        i.spread50_200_expanding = false;
        assert_eq!(structural_phase(&i, false).as_str(), "bull-warning");
        i.spread50_200_pct = Some(0.8);
        assert_eq!(structural_phase(&i, false).as_str(), "convergence-exit");
        i.spread50_200_pct = Some(2.0);
        i.days_below_sma50 = 12;
        assert_eq!(structural_phase(&i, false).as_str(), "correction-exit");
        i.spread50_200_pct = Some(-2.0);
        i.spread50_200_expanding = true;
        i.close = 7500.0;
        assert_eq!(structural_phase(&i, false).as_str(), "early-bull");
        i.spread50_200_expanding = false;
        assert_eq!(structural_phase(&i, false).as_str(), "bear-hold");
        i.spread50_200_pct = Some(-4.0);
        assert_eq!(structural_phase(&i, false).as_str(), "bear-deep");
        assert_eq!(structural_phase(&i, true).as_str(), "velocity-exit");
        i.spread50_200_pct = None;
        assert_eq!(structural_phase(&i, false).as_str(), "unknown");
    }

    #[test]
    fn regime_3_caps_handling_in_bear_phases_and_sets_the_side() {
        let r = regime_3(&base(), &[], false);
        assert_eq!(r.state, "calm-uptrend");
        assert_eq!(r.structural_phase, Some(StructuralPhase::FullBull));
        assert_eq!(r.preferred_side, Some("put-spreads"));
        assert_eq!(r.size_multiplier, 1.0);
        let mut i = base();
        i.spread50_200_pct = Some(-4.0);
        i.spread50_200_expanding = false;
        let r = regime_3(&i, &[], false);
        assert_eq!(r.structural_phase, Some(StructuralPhase::BearDeep));
        assert_eq!(r.size_multiplier, 0.5);
        assert_eq!(r.allowed_classes, &["index"]);
        assert_eq!(r.max_short_delta, 0.25);
        assert_eq!(r.preferred_side, Some("none-or-call-spreads"));
        let v = regime_3(&base(), &[], true);
        assert_eq!(v.structural_phase, Some(StructuralPhase::VelocityExit));
        assert!(!v.open_allowed);
    }

    /// With under 205 sessions the 50/200 spread cannot be computed. Adding
    /// "unknown" to the bearish set capped size and class but left
    /// open_allowed TRUE — a mutant deleting the fix survived the whole suite,
    /// because no test asserted what regime_3 DOES with "unknown".
    #[test]
    fn an_unmeasurable_structure_blocks_opening_outright() {
        let mut i = base();
        i.spread50_200_pct = None;
        let r = regime_4(&i, &[], false);
        assert_eq!(r.structural_phase, Some(StructuralPhase::Unknown));
        assert!(
            !r.open_allowed,
            "unknown structure must not authorise opening"
        );
        // `reasons` is the only narrative surface; it must say so too.
        assert!(
            r.reasons.iter().any(|x| x.contains("opening blocked")),
            "{:?}",
            r.reasons
        );
        assert_eq!(r.preferred_side, Some("none-or-call-spreads"));
        assert_eq!(r.size_multiplier, 0.5);
    }

    /// A published definition never changes. regime/3 shipped restricting an
    /// unknown structure WITHOUT blocking opens, and with no opening clause
    /// in its reason line; the repair is regime/4, not an edit to regime/3.
    #[test]
    fn regime_3_is_unchanged_by_the_publication_of_regime_4() {
        let mut i = base();
        i.spread50_200_pct = None;
        let r3 = regime_3(&i, &[], false);
        assert!(r3.open_allowed);
        assert_eq!(r3.size_multiplier, 0.5);
        assert!(r3.reasons.iter().any(|x| x
            == "structural unknown: size capped 0.5, index only, delta ≤ 0.25; preferred side none-or-call-spreads"));
        // Everywhere the structure IS measurable the two agree exactly.
        let (a, b) = (regime_3(&base(), &[], false), regime_4(&base(), &[], false));
        assert_eq!(a.open_allowed, b.open_allowed);
        assert_eq!(a.reasons, b.reasons);
        // Velocity: same gate, regime/4 also says it.
        let (v3, v4) = (regime_3(&base(), &[], true), regime_4(&base(), &[], true));
        assert_eq!(v3.open_allowed, v4.open_allowed);
        assert!(v4.reasons.iter().any(|x| x.contains("opening")));
    }

    #[test]
    fn velocity_trigger_is_eight_percent_or_a_three_percent_day() {
        let mut i = base();
        assert!(!velocity_trigger(&i));
        i.drop_from_high10_pct = 8.5;
        assert!(velocity_trigger(&i));
        i.drop_from_high10_pct = 1.0;
        i.ret1_pct = -3.0;
        assert!(velocity_trigger(&i));
    }

    #[test]
    fn realized_vol_of_a_flat_series_is_zero_and_needs_n_plus_one_closes() {
        let flat = vec![100.0; 30];
        assert_eq!(realized_vol(&flat, 20), 0.0);
        assert_eq!(realized_vol(&flat[..10], 20), 0.0);
        let alt: Vec<f64> = (0..41)
            .map(|k| if k % 2 == 0 { 100.0 } else { 101.0 })
            .collect();
        let rv = realized_vol(&alt, 20);
        assert!((rv - 16.2).abs() < 0.5, "{rv}");
    }

    #[test]
    fn inputs_need_sixty_one_closes_and_compute_means_drawdown_and_structure() {
        // Flat for 50 sessions, then +10/day: the 20/50 spread is expanding.
        let closes: Vec<f64> = (0..80)
            .map(|k| {
                if k < 50 {
                    7000.0
                } else {
                    7000.0 + (k - 49) as f64 * 10.0
                }
            })
            .collect();
        let vix: Vec<f64> = vec![16.0; 30];
        let i = inputs_from(&closes, &vix).unwrap();
        assert_eq!(i.close, 7300.0);
        assert!((i.sma20 - 7205.0).abs() < 1e-9);
        assert!((i.sma50 - 7093.0).abs() < 1e-9);
        assert_eq!(i.drawdown20_pct, 0.0);
        assert_eq!(i.vix_high5, 16.0);
        assert!(i.spread20_50_expanding);
        assert_eq!(i.sma200, None);
        assert_eq!(i.days_below_sma50, 0);
        assert!((i.ret1_pct - 10.0 / 7290.0 * 100.0).abs() < 1e-9);
        // A straight-line ramp has a *contracting* ratio spread (same $ gap, higher base).
        let ramp: Vec<f64> = (0..80).map(|k| 7000.0 + k as f64 * 10.0).collect();
        assert!(!inputs_from(&ramp, &vix).unwrap().spread20_50_expanding);
        assert_eq!(inputs_from(&closes[..50], &vix).unwrap_err().exit_code(), 5);
        // Accelerating (quadratic) rise: the 50/200 ratio spread expands.
        let long: Vec<f64> = (0..260).map(|k| 5000.0 + 0.05 * (k * k) as f64).collect();
        let j = inputs_from(&long, &vix).unwrap();
        assert!(j.sma200.is_some() && j.spread50_200_pct.unwrap() > 0.0);
        assert!(j.spread50_200_expanding);
        let mut fall = long.clone();
        let len = fall.len();
        for slot in fall.iter_mut().skip(len - 5) {
            *slot = 4000.0;
        }
        assert_eq!(inputs_from(&fall, &vix).unwrap().days_below_sma50, 5);
    }

    #[test]
    fn replay_walks_the_series_with_stateful_dwell_and_cooldown() {
        // Accelerating rise (expanding 20/50 spread) with a −4% shock 3 sessions from the end.
        let mut spx: Vec<f64> = (0..240).map(|k| 6000.0 + 0.02 * (k * k) as f64).collect();
        let n = spx.len();
        let shock = spx[n - 4] * 0.96;
        spx[n - 3] = shock;
        spx[n - 2] = shock * 1.002;
        spx[n - 1] = shock * 1.004;
        let vix = vec![15.0; 240];
        let run3 = replay(RegimeVersion::V3, &spx, &vix, 6).unwrap();
        assert_eq!(run3.len(), 6);
        let states: Vec<&str> = run3.iter().map(|(_, r)| r.state).collect();
        assert_eq!(states[2], "calm-uptrend");
        assert_eq!(states[3], "stressed", "{states:?}");
        assert_eq!(states[4], "stressed", "dwell holds");
        assert_eq!(states[5], "stressed", "dwell holds");
        let phases: Vec<Option<&str>> = run3
            .iter()
            .map(|(_, r)| r.structural_phase.map(StructuralPhase::as_str))
            .collect();
        assert_eq!(phases[3], Some("velocity-exit"));
        assert_eq!(phases[5], Some("velocity-exit"), "10-session cooldown");
        let run1 = replay(RegimeVersion::V1, &spx, &vix, 6).unwrap();
        assert_eq!(run1[3].1.state, "stressed");
        assert!(run1.iter().all(|(_, r)| r.structural_phase.is_none()));
        assert!(replay(RegimeVersion::V1, &spx[..30], &vix, 3).is_err());
    }

    /// RS8/RS9: dwell only works if `replay` feeds each session the raw
    /// states of the sessions before it. Recompute that history independently
    /// and require the reported state to be the dwell over it, on a series
    /// where dwell actually holds a state the raw classifier has left. A
    /// replay that stopped pushing raw states (or truncated them) reports the
    /// raw state and fails here.
    #[test]
    fn replay_feeds_dwell_the_full_raw_history() {
        let mut spx: Vec<f64> = (0..240).map(|k| 6000.0 + 0.02 * (k * k) as f64).collect();
        let n = spx.len();
        let shock = spx[n - 8] * 0.96;
        for (j, slot) in spx.iter_mut().skip(n - 7).enumerate() {
            *slot = shock * (1.0 + 0.004 * (j + 1) as f64);
        }
        let vix = vec![15.0; 240];
        for version in [RegimeVersion::V2, RegimeVersion::V3, RegimeVersion::V4] {
            let run = replay(version, &spx, &vix, 12).unwrap();
            let raw: Vec<&'static str> = run.iter().map(|(i, _)| regime_2_raw(i).state).collect();
            let mut held_past_raw = 0;
            for (k, (_, r)) in run.iter().enumerate() {
                assert_eq!(
                    r.state,
                    apply_dwell(&raw[..=k], 3),
                    "{version:?} session {k}: raw {raw:?}"
                );
                if r.state != raw[k] {
                    held_past_raw += 1;
                }
            }
            assert!(
                held_past_raw > 0,
                "{version:?}: the fixture must exercise dwell (raw {raw:?})"
            );
        }
    }

    /// RS4: regime/3 and /4 need ≥ 205 sessions for the 50/200 overlay. At
    /// the fewest sessions a calendar span can hold (5 of 7 days trade, less
    /// 10 NYSE holidays a year), 320 days yields ≥ 205; 120 would yield ~80,
    /// and the live regime would read `unknown` every day.
    #[test]
    fn the_structural_lookback_covers_205_sessions() {
        let min_sessions = |days: i64| days * 5 / 7 - (days * 10 + 364) / 365;
        // The oldest replayed session needs the full window behind it.
        let cases = [
            (RegimeVersion::V1, 61),
            (RegimeVersion::V2, 61),
            (RegimeVersion::V3, 205),
            (RegimeVersion::V4, 205),
        ];
        for (v, needed) in cases {
            for history in [1usize, 2, 10, 60, 250, 750] {
                let days = v.lookback_days(history);
                let have = min_sessions(days);
                assert!(
                    have >= needed + history as i64 - 1,
                    "{v:?} --history {history}: {days} calendar days hold only {have} sessions"
                );
            }
        }
        assert!(min_sessions(120) < 205, "the check must be able to fail");
    }

    /// Routing, not arithmetic: on 150 calm sessions the 50/200 spread is
    /// unmeasurable, which is the one input the two definitions disagree on.
    /// A name that fell through to the other definition fails here.
    #[test]
    fn replay_routes_each_structural_metric_to_its_own_definition() {
        let spx: Vec<f64> = (0..150).map(|k| 6000.0 + 0.02 * (k * k) as f64).collect();
        let vix = vec![15.0; 150];
        let last = |name: &str| {
            let v = RegimeVersion::of(name).unwrap();
            replay(v, &spx, &vix, 2).unwrap().pop().unwrap().1
        };
        let (r3, r4) = (last("regime/3"), last("regime/4"));
        assert_eq!(r3.structural_phase, Some(StructuralPhase::Unknown));
        assert_eq!(r4.structural_phase, Some(StructuralPhase::Unknown));
        assert!(r3.open_allowed, "regime/3 as published");
        assert!(!r4.open_allowed, "regime/4 fails closed");
        assert!(last("regime/2").structural_phase.is_none());
        assert_eq!(RegimeVersion::of("regime/9").unwrap_err().exit_code(), 2);
    }

    #[test]
    fn interpolated_bars_are_dropped_and_the_live_level_stands_in_for_today() {
        let row = json!({"bars": [
            {"begins_at": "2026-09-11T00:00:00Z", "close_value": "100", "interpolated": false},
            {"begins_at": "2026-09-12T00:00:00Z", "close_value": "100", "interpolated": true},
            {"begins_at": "2026-09-14T00:00:00Z", "close_value": "102"},
            {"begins_at": "2026-09-15T00:00:00Z", "close_value": "102", "interpolated": true},
            {"begins_at": "2026-09-16T00:00:00Z", "close_value": "0"}
        ]});
        let bars = real_closes(&row);
        assert_eq!(
            bars,
            vec![
                ("2026-09-11".to_string(), 100.0),
                ("2026-09-14".to_string(), 102.0)
            ]
        );
        assert_eq!(
            closes_with_live(&bars, "2026-09-15", 99.5),
            vec![100.0, 102.0, 99.5]
        );
        assert_eq!(
            closes_with_live(&bars, "2026-09-14", 99.5),
            vec![100.0, 102.0]
        );
        assert_eq!(
            closes_with_live(&bars, "2026-09-15", 0.0),
            vec![100.0, 102.0]
        );
    }
}
