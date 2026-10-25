//! Credible strategy backtests versus buy-and-hold.
//!
//! Conceptual port of Whats-News `backtester.py`. Implemented in Rust against verified
//! daily bars from the local archive / Whats-News bridge. Python is not vendored.
//!
//! Methodological rules (WN-style pitfalls this module refuses to repeat):
//! - **No winner selection on the evaluation window.** Every requested strategy is reported.
//!   Train metrics are labelled in-sample and are not used to pick a champion.
//! - **Trading costs** are applied on every fill (commission + slippage, per side).
//! - **Execution is explicit:** signal after the session close; fill at the **next open**.
//! - **Chronological holdout:** last fraction of the series is unseen evaluate; earlier
//!   bars are train / indicator warm-up only.
//! - **Completed-trade** win rate is winners / closed trades. Positive holding days are
//!   never called a win rate.
//! - **Reproducible settings** (seed, params, costs, fill, date range, split) travel with
//!   the report.
//! - **Sample-data isolation is absolute:** sample / demo / unverified bars never compute.

use serde::{Deserialize, Serialize};
use tr_core::{
    Bar, Provenance,
    indicators::{AdaptiveTrendParams, adaptive_trend_research, rsi, sma},
};

use crate::stats::{bar_date, require_research_bars};

pub const TRADING_DAYS: f64 = 252.0;
pub const DEFAULT_HOLDOUT_FRAC: f64 = 0.20;
pub const DEFAULT_COMMISSION_BPS: f64 = 5.0;
pub const DEFAULT_SLIPPAGE_BPS: f64 = 5.0;
pub const DEFAULT_SEED: u64 = 1;
pub const MIN_EVALUATE_BARS: usize = 20;
pub const MIN_TRAIN_BARS: usize = 30;

const OPERATION: &str = "Backtest";

/// Shared gate. Sample / demo / unverified / empty series never enter a backtest.
pub fn require_backtest_bars(bars: &[Bar], operation: &str) -> Result<(), String> {
    require_research_bars(bars, operation)?;
    Bar::provenance(bars).require_verified(operation)?;
    Ok(())
}

/// Commission + slippage charged on each fill (entry and exit).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CostModel {
    pub commission_bps: f64,
    pub slippage_bps: f64,
}

impl Default for CostModel {
    fn default() -> Self {
        Self { commission_bps: DEFAULT_COMMISSION_BPS, slippage_bps: DEFAULT_SLIPPAGE_BPS }
    }
}

impl CostModel {
    pub fn per_side_frac(self) -> f64 {
        ((self.commission_bps + self.slippage_bps) / 10_000.0).max(0.0)
    }

    pub fn label(self) -> String {
        format!(
            "{:.1} bps commission + {:.1} bps slippage per side (round-trip {:.1} bps)",
            self.commission_bps,
            self.slippage_bps,
            2.0 * (self.commission_bps + self.slippage_bps)
        )
    }
}

/// Fill assumption. Always stated with results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FillRule {
    /// Decision after bar *i* close; order fills at bar *i+1* open.
    NextOpen,
}

impl FillRule {
    pub fn label(self) -> &'static str {
        match self {
            Self::NextOpen => {
                "next session open (signal after the prior close; no same-bar fill, no close-to-close lookahead)"
            }
        }
    }
}

impl Default for FillRule {
    fn default() -> Self {
        Self::NextOpen
    }
}

/// Declared strategy. Parameters are fixed before the evaluate window is scored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StrategySpec {
    BuyAndHold,
    AdaptiveTrend { long_only: bool, atr_n: usize, confirm: f64, method: f64 },
    SmaCross { fast: usize, slow: usize },
    RsiReversion { period: usize, oversold: f64, overbought: f64 },
}

impl StrategySpec {
    pub fn id(&self) -> &'static str {
        match self {
            Self::BuyAndHold => "buy_hold",
            Self::AdaptiveTrend { .. } => "adtrend",
            Self::SmaCross { .. } => "sma_cross",
            Self::RsiReversion { .. } => "rsi_reversion",
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::BuyAndHold => "Buy & hold".into(),
            Self::AdaptiveTrend { long_only, .. } => {
                if *long_only {
                    "Adaptive Trend (long/flat)".into()
                } else {
                    "Adaptive Trend (long/short)".into()
                }
            }
            Self::SmaCross { fast, slow } => format!("SMA {fast}/{slow} cross"),
            Self::RsiReversion { period, oversold, overbought } => {
                format!("RSI({period}) {oversold:.0}/{overbought:.0} reversion")
            }
        }
    }

    pub fn params_line(&self) -> String {
        match self {
            Self::BuyAndHold => "always long from the first evaluate open; hold through the last close".into(),
            Self::AdaptiveTrend { long_only, atr_n, confirm, method } => {
                let kind = if *method >= 0.5 { "ADMA" } else { "KAMA" };
                let side =
                    if *long_only { "long when medium regime is +1, else flat" } else { "medium regime +1/−1" };
                format!("ATR {atr_n}, confirm {confirm}, {kind}; {side}")
            }
            Self::SmaCross { fast, slow } => {
                format!("long when SMA({fast}) > SMA({slow}) on the signal close, else flat (or short if shorts on)")
            }
            Self::RsiReversion { period, oversold, overbought } => {
                format!("enter long when RSI({period}) < {oversold}; exit when RSI > {overbought}")
            }
        }
    }

    pub fn is_benchmark(&self) -> bool {
        matches!(self, Self::BuyAndHold)
    }
}

/// User-declared run. Changing these after seeing evaluate numbers is in-sample.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSettings {
    pub holdout_frac: f64,
    pub costs: CostModel,
    pub fill: FillRule,
    pub seed: u64,
    pub allow_short: bool,
    pub strategies: Vec<StrategySpec>,
}

impl Default for RunSettings {
    fn default() -> Self {
        Self {
            holdout_frac: DEFAULT_HOLDOUT_FRAC,
            costs: CostModel::default(),
            fill: FillRule::NextOpen,
            seed: DEFAULT_SEED,
            allow_short: false,
            strategies: default_strategies(false),
        }
    }
}

impl RunSettings {
    pub fn with_strategies(mut self, strategies: Vec<StrategySpec>) -> Self {
        self.strategies = ensure_benchmark(strategies);
        self
    }

    pub fn settings_line(&self) -> String {
        format!(
            "seed={} (deterministic) · fill={} · costs={} · holdout=last {:.0}% chronological · shorts={}",
            self.seed,
            match self.fill {
                FillRule::NextOpen => "next-open",
            },
            self.costs.label(),
            self.holdout_frac * 100.0,
            if self.allow_short { "on" } else { "off (long/flat)" }
        )
    }
}

pub fn default_strategies(allow_short: bool) -> Vec<StrategySpec> {
    vec![
        StrategySpec::BuyAndHold,
        StrategySpec::AdaptiveTrend { long_only: !allow_short, atr_n: 20, confirm: 0.25, method: 0.0 },
        StrategySpec::SmaCross { fast: 20, slow: 50 },
        StrategySpec::RsiReversion { period: 14, oversold: 30.0, overbought: 70.0 },
    ]
}

/// Always keep buy-and-hold first so the evaluate table is a comparison, not a contest.
pub fn ensure_benchmark(mut specs: Vec<StrategySpec>) -> Vec<StrategySpec> {
    if !specs.iter().any(StrategySpec::is_benchmark) {
        specs.insert(0, StrategySpec::BuyAndHold);
    } else if let Some(i) = specs.iter().position(StrategySpec::is_benchmark)
        && i != 0
    {
        let bh = specs.remove(i);
        specs.insert(0, bh);
    }
    specs
}

/// Parse `ADTREND,SMA` / `ALL` / `SMA 10 40`. Empty means the default comparison set.
pub fn parse_strategies(line: &str, allow_short: bool) -> Result<Vec<StrategySpec>, String> {
    let raw = line.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("all") || raw.eq_ignore_ascii_case("default") {
        return Ok(default_strategies(allow_short));
    }
    let mut out = Vec::new();
    for chunk in raw.split(|c: char| c == ',' || c == ';' || c == '|') {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        out.push(parse_one_strategy(chunk, allow_short)?);
    }
    if out.is_empty() {
        return Err("Backtest needs at least one named strategy (ALL, ADTREND, SMA, RSI, HOLD)".into());
    }
    Ok(ensure_benchmark(out))
}

fn parse_one_strategy(chunk: &str, allow_short: bool) -> Result<StrategySpec, String> {
    let parts: Vec<&str> = chunk.split_whitespace().collect();
    let head = parts.first().copied().unwrap_or("").to_ascii_uppercase();
    let num = |i: usize| parts.get(i).and_then(|s| s.parse::<f64>().ok());
    match head.as_str() {
        "HOLD" | "BH" | "B&H" | "BUYHOLD" | "BUYANDHOLD" | "PASSIVE" => Ok(StrategySpec::BuyAndHold),
        "ADTREND" | "ATREND" | "ADAPTIVE" | "AT" => Ok(StrategySpec::AdaptiveTrend {
            long_only: !allow_short,
            atr_n: num(1).map(|v| v as usize).filter(|v| *v > 0).unwrap_or(20),
            confirm: num(2).unwrap_or(0.25),
            method: num(3).unwrap_or(0.0),
        }),
        "SMA" | "SMACROSS" | "MA" | "CROSS" => {
            let fast = num(1).map(|v| v as usize).filter(|v| *v > 0).unwrap_or(20);
            let slow = num(2).map(|v| v as usize).filter(|v| *v > fast).unwrap_or(50.max(fast + 1));
            Ok(StrategySpec::SmaCross { fast, slow })
        }
        "RSI" | "RSIREV" | "REVERSION" => Ok(StrategySpec::RsiReversion {
            period: num(1).map(|v| v as usize).filter(|v| *v > 1).unwrap_or(14),
            oversold: num(2).unwrap_or(30.0),
            overbought: num(3).unwrap_or(70.0),
        }),
        _ => Err(format!("Unknown strategy {chunk:?} (use HOLD, ADTREND, SMA, RSI or ALL)")),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletedTrade {
    pub entry_date: String,
    pub exit_date: String,
    pub side: i8,
    pub entry_px: f64,
    pub exit_px: f64,
    pub ret: f64,
    pub bars_held: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradeStats {
    pub completed: usize,
    pub winners: usize,
    /// Winning **closed** trades / closed trades. Never the share of up-days in the position.
    pub completed_win_rate: Option<f64>,
    pub avg_win: Option<f64>,
    pub avg_loss: Option<f64>,
    pub profit_factor: Option<f64>,
    pub avg_bars_held: Option<f64>,
    pub expectancy: Option<f64>,
    pub open_trades: usize,
    pub open_unrealized: Option<f64>,
    /// Share of in-market days with a positive close-to-close. Shown only to contrast win rate.
    pub holding_day_positive_rate: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowMetrics {
    pub label: String,
    pub start: String,
    pub end: String,
    pub bars: usize,
    pub start_equity: f64,
    pub end_equity: f64,
    pub total_return: Option<f64>,
    pub cagr: Option<f64>,
    pub max_drawdown: Option<f64>,
    pub exposure: Option<f64>,
    pub trades: TradeStats,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrategyResult {
    pub spec: StrategySpec,
    pub label: String,
    pub params: String,
    pub benchmark: bool,
    pub evaluate: WindowMetrics,
    pub train: WindowMetrics,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BacktestReport {
    pub symbol: String,
    pub ticker: String,
    pub provenance: Provenance,
    pub source: String,
    pub as_of: String,
    pub series_start: String,
    pub series_end: String,
    pub bars: usize,
    pub settings: RunSettings,
    pub settings_line: String,
    pub holdout_note: String,
    pub execution_note: String,
    pub metrics_note: String,
    pub selection_note: String,
    pub paper_note: String,
    pub train_start: String,
    pub train_end: String,
    pub evaluate_start: String,
    pub evaluate_end: String,
    pub evaluate_bars: usize,
    pub train_bars: usize,
    pub results: Vec<StrategyResult>,
}

#[derive(Clone)]
struct OpenTrade {
    side: i8,
    entry_date: String,
    entry_px: f64,
    entry_i: usize,
}

struct Sim {
    cash: f64,
    qty: f64,
    completed: Vec<CompletedTrade>,
    open: Option<OpenTrade>,
    equity: Vec<Option<f64>>,
    in_market: Vec<bool>,
    up_days: usize,
    market_days: usize,
}

impl Sim {
    fn new(n: usize) -> Self {
        Self {
            cash: 1.0,
            qty: 0.0,
            completed: Vec::new(),
            open: None,
            equity: vec![None; n],
            in_market: vec![false; n],
            up_days: 0,
            market_days: 0,
        }
    }

    fn side(&self) -> i8 {
        if self.qty > 1e-12 {
            1
        } else if self.qty < -1e-12 {
            -1
        } else {
            0
        }
    }

    fn equity_at(&self, px: f64) -> f64 {
        self.cash + self.qty * px
    }

    fn flatten(&mut self, bars: &[Bar], i: usize, fill: f64, date: &str) {
        let Some(open) = self.open.take() else {
            self.qty = 0.0;
            return;
        };
        let proceeds = self.qty * fill;
        self.cash += proceeds;
        self.qty = 0.0;
        let ret = trade_return(open.side, open.entry_px, fill);
        self.completed.push(CompletedTrade {
            entry_date: open.entry_date,
            exit_date: date.to_string(),
            side: open.side,
            entry_px: open.entry_px,
            exit_px: fill,
            ret,
            bars_held: i.saturating_sub(open.entry_i).max(1),
        });
        let _ = bars;
    }

    fn open_side(&mut self, side: i8, fill: f64, date: &str, i: usize) {
        if side == 0 {
            return;
        }
        let equity = self.equity_at(fill);
        if equity <= 0.0 || fill <= 0.0 {
            return;
        }
        let notional = equity;
        self.qty = side as f64 * (notional / fill);
        self.cash = equity - self.qty * fill;
        self.open = Some(OpenTrade { side, entry_date: date.to_string(), entry_px: fill, entry_i: i });
    }
}

fn trade_return(side: i8, entry: f64, exit: f64) -> f64 {
    if entry <= 0.0 || exit <= 0.0 {
        return 0.0;
    }
    if side >= 0 { exit / entry - 1.0 } else { entry / exit - 1.0 }
}

fn buy_fill(raw: f64, cost: f64) -> f64 {
    raw * (1.0 + cost)
}

fn sell_fill(raw: f64, cost: f64) -> f64 {
    raw * (1.0 - cost).max(1e-9)
}

fn fill_for_action(raw: f64, from: i8, to: i8, cost: f64) -> f64 {
    // Buying (flat→long or short→flat/long) pays up; selling pays down.
    if to > from { buy_fill(raw, cost) } else { sell_fill(raw, cost) }
}

/// `targets[i]` is the desired signed position after seeing close `i`.
fn simulate(bars: &[Bar], targets: &[i8], costs: CostModel, start: usize, end: usize) -> Sim {
    let n = bars.len();
    let mut sim = Sim::new(n);
    if n == 0 || start > end || end >= n {
        return sim;
    }
    let cost = costs.per_side_frac();
    let mut prev_eq: Option<f64> = None;

    for i in start..=end {
        if i > 0 {
            let want = targets[i - 1];
            let have = sim.side();
            if want != have {
                let raw = bars[i].open;
                if raw.is_finite() && raw > 0.0 {
                    let date = bar_date(bars[i].ts);
                    if have != 0 {
                        let fill = fill_for_action(raw, have, 0, cost);
                        sim.flatten(bars, i, fill, &date);
                    }
                    if want != 0 {
                        let after = sim.side();
                        let fill = fill_for_action(raw, after, want, cost);
                        sim.open_side(want, fill, &date, i);
                    }
                }
            }
        }
        let close = bars[i].close;
        if close.is_finite() && close > 0.0 {
            let eq = sim.equity_at(close);
            sim.equity[i] = Some(eq);
            let in_mkt = sim.side() != 0;
            sim.in_market[i] = in_mkt;
            if in_mkt {
                sim.market_days += 1;
                if let Some(prev) = prev_eq
                    && eq > prev
                {
                    sim.up_days += 1;
                }
            }
            prev_eq = Some(eq);
        }
    }
    sim
}

fn cagr(start: f64, end: f64, bars: usize) -> Option<f64> {
    if start <= 0.0 || end <= 0.0 || bars == 0 {
        return None;
    }
    let years = bars as f64 / TRADING_DAYS;
    if years < 1.0 / TRADING_DAYS {
        return None;
    }
    Some((end / start).powf(1.0 / years) - 1.0)
}

fn max_drawdown(equity: &[Option<f64>], start: usize, end: usize) -> Option<f64> {
    let mut peak = f64::NEG_INFINITY;
    let mut dd = 0.0_f64;
    let mut any = false;
    for eq in equity.iter().take(end + 1).skip(start).flatten() {
        any = true;
        peak = peak.max(*eq);
        if peak > 0.0 {
            dd = dd.min(*eq / peak - 1.0);
        }
    }
    any.then_some(dd)
}

fn mean(xs: &[f64]) -> Option<f64> {
    if xs.is_empty() { None } else { Some(xs.iter().sum::<f64>() / xs.len() as f64) }
}

fn trade_stats(sim: &Sim, last_close: Option<f64>) -> TradeStats {
    let wins: Vec<f64> = sim.completed.iter().filter(|t| t.ret > 0.0).map(|t| t.ret).collect();
    let losses: Vec<f64> = sim.completed.iter().filter(|t| t.ret <= 0.0).map(|t| t.ret).collect();
    let n = sim.completed.len();
    let winners = wins.len();
    let gross_win: f64 = wins.iter().sum();
    let gross_loss: f64 = losses.iter().map(|v| v.abs()).sum();
    let open_unrealized = match (&sim.open, last_close) {
        (Some(o), Some(px)) if px > 0.0 => Some(trade_return(o.side, o.entry_px, px)),
        _ => None,
    };
    TradeStats {
        completed: n,
        winners,
        completed_win_rate: (n > 0).then_some(winners as f64 / n as f64),
        avg_win: mean(&wins),
        avg_loss: mean(&losses),
        profit_factor: (gross_loss > 0.0).then_some(gross_win / gross_loss),
        avg_bars_held: (n > 0).then_some(sim.completed.iter().map(|t| t.bars_held as f64).sum::<f64>() / n as f64),
        expectancy: mean(&sim.completed.iter().map(|t| t.ret).collect::<Vec<_>>()),
        open_trades: usize::from(sim.open.is_some()),
        open_unrealized,
        holding_day_positive_rate: (sim.market_days > 0).then_some(sim.up_days as f64 / sim.market_days as f64),
    }
}

fn window_metrics(label: &str, bars: &[Bar], start: usize, end: usize, sim: &Sim) -> WindowMetrics {
    // Start from unit capital so entry costs are not cancelled out of the return ratio.
    let first_eq = 1.0;
    let last_eq = sim.equity.iter().take(end + 1).skip(start).flatten().copied().next_back().unwrap_or(first_eq);
    let n = end.saturating_sub(start) + 1;
    let exposed = sim.in_market.iter().take(end + 1).skip(start).filter(|v| **v).count();
    let last_close = bars.get(end).map(|b| b.close);
    WindowMetrics {
        label: label.into(),
        start: bars.get(start).map(|b| bar_date(b.ts)).unwrap_or_default(),
        end: bars.get(end).map(|b| bar_date(b.ts)).unwrap_or_default(),
        bars: n,
        start_equity: first_eq,
        end_equity: last_eq,
        total_return: (first_eq > 0.0).then_some(last_eq / first_eq - 1.0),
        cagr: cagr(first_eq, last_eq, n),
        max_drawdown: max_drawdown(&sim.equity, start, end),
        exposure: (n > 0).then_some(exposed as f64 / n as f64),
        trades: trade_stats(sim, last_close),
    }
}

fn targets_for(spec: &StrategySpec, bars: &[Bar], allow_short: bool) -> Result<Vec<i8>, String> {
    let n = bars.len();
    match spec {
        StrategySpec::BuyAndHold => Ok(vec![1; n]),
        StrategySpec::AdaptiveTrend { long_only, atr_n, confirm, method } => {
            let mut p = AdaptiveTrendParams::default();
            p.atr_n = (*atr_n).max(1);
            p.confirm_mult = *confirm;
            p.method = tr_core::indicators::AdaptiveMethod::from_param(*method);
            let trend = adaptive_trend_research(bars, p)?;
            Ok(trend
                .medium_state
                .into_iter()
                .map(|s| {
                    if s > 0 {
                        1
                    } else if s < 0 && !*long_only && allow_short {
                        -1
                    } else {
                        0
                    }
                })
                .collect())
        }
        StrategySpec::SmaCross { fast, slow } => {
            let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
            let f = sma(&closes, (*fast).max(1));
            let s = sma(&closes, (*slow).max(fast + 1));
            Ok(f.iter()
                .zip(&s)
                .map(|(a, b)| {
                    if a.is_finite() && b.is_finite() && *a > *b {
                        1
                    } else if a.is_finite() && b.is_finite() && *a < *b && allow_short {
                        -1
                    } else {
                        0
                    }
                })
                .collect())
        }
        StrategySpec::RsiReversion { period, oversold, overbought } => {
            let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
            let r = rsi(&closes, (*period).max(2));
            let mut pos = vec![0i8; n];
            let mut side = 0i8;
            for i in 0..n {
                if !r[i].is_finite() {
                    pos[i] = side;
                    continue;
                }
                if r[i] < *oversold {
                    side = 1;
                } else if r[i] > *overbought {
                    side = if allow_short { -1 } else { 0 };
                }
                pos[i] = side;
            }
            Ok(pos)
        }
    }
}

/// Last `holdout_frac` of bars are evaluate; earlier bars are train / warm-up.
pub fn holdout_split(n: usize, frac: f64) -> Result<usize, String> {
    if n < MIN_EVALUATE_BARS + MIN_TRAIN_BARS {
        return Err(format!(
            "{OPERATION} refused: need at least {} daily bars for a chronological train/evaluate split (have {n})",
            MIN_EVALUATE_BARS + MIN_TRAIN_BARS
        ));
    }
    let frac = if frac.is_finite() { frac.clamp(0.05, 0.80) } else { DEFAULT_HOLDOUT_FRAC };
    let mut eval_n = ((n as f64) * frac).round() as usize;
    eval_n = eval_n.max(MIN_EVALUATE_BARS);
    if n - eval_n < MIN_TRAIN_BARS {
        eval_n = n - MIN_TRAIN_BARS;
    }
    if eval_n < MIN_EVALUATE_BARS || eval_n >= n {
        return Err(format!("{OPERATION} refused: chronological holdout does not leave enough evaluate bars"));
    }
    Ok(n - eval_n)
}

fn parse_holdout_frac(raw: &str) -> f64 {
    let t = raw.trim().trim_end_matches('%');
    if t.is_empty() {
        return DEFAULT_HOLDOUT_FRAC;
    }
    match t.parse::<f64>() {
        Ok(v) if v > 1.0 => (v / 100.0).clamp(0.05, 0.80),
        Ok(v) if v > 0.0 => v.clamp(0.05, 0.80),
        _ => DEFAULT_HOLDOUT_FRAC,
    }
}

/// `20` / `0.2` / `20%` → fraction of the series reserved for evaluate.
pub fn parse_holdout(raw: &str) -> f64 {
    parse_holdout_frac(raw)
}

pub fn parse_bps(raw: &str, default: f64) -> f64 {
    raw.trim()
        .trim_end_matches("bps")
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(default)
}

pub fn compute_report(
    symbol: &str,
    ticker: &str,
    bars: &[Bar],
    settings: &RunSettings,
    source: &str,
) -> Result<BacktestReport, String> {
    require_backtest_bars(bars, OPERATION)?;
    let provenance = Bar::provenance(bars);
    provenance.require_verified(OPERATION)?;
    let n = bars.len();
    let eval_start = holdout_split(n, settings.holdout_frac)?;
    let train_end = eval_start.saturating_sub(1);
    if eval_start == 0 {
        return Err(format!(
            "{OPERATION} refused: evaluate window would include the first bar (no prior close for a next-open fill)"
        ));
    }
    let strategies = ensure_benchmark(settings.strategies.clone());
    let mut results = Vec::with_capacity(strategies.len());
    for spec in &strategies {
        let targets = targets_for(spec, bars, settings.allow_short)?;
        if targets.len() != n {
            return Err(format!("{OPERATION} refused: strategy {} produced a misaligned signal", spec.label()));
        }
        let eval_sim = simulate(bars, &targets, settings.costs, eval_start, n - 1);
        let train_sim = if train_end > 0 {
            simulate(bars, &targets, settings.costs, 1.min(train_end), train_end)
        } else {
            simulate(bars, &targets, settings.costs, 0, 0)
        };
        results.push(StrategyResult {
            spec: spec.clone(),
            label: spec.label(),
            params: spec.params_line(),
            benchmark: spec.is_benchmark(),
            evaluate: window_metrics("evaluate (unseen holdout)", bars, eval_start, n - 1, &eval_sim),
            train: window_metrics(
                "train (in-sample / warm-up — not for selection)",
                bars,
                1.min(train_end),
                train_end,
                &train_sim,
            ),
        });
    }
    let train_start = bars.first().map(|b| bar_date(b.ts)).unwrap_or_default();
    let train_end_d = bars.get(train_end).map(|b| bar_date(b.ts)).unwrap_or_default();
    let evaluate_start = bars.get(eval_start).map(|b| bar_date(b.ts)).unwrap_or_default();
    let evaluate_end = bars.last().map(|b| bar_date(b.ts)).unwrap_or_default();
    Ok(BacktestReport {
        symbol: symbol.into(),
        ticker: ticker.into(),
        provenance,
        source: provenance.source_label(source).into(),
        as_of: evaluate_end.clone(),
        series_start: train_start.clone(),
        series_end: evaluate_end.clone(),
        bars: n,
        settings: RunSettings { strategies: strategies.clone(), ..settings.clone() },
        settings_line: settings.settings_line(),
        holdout_note: format!(
            "Chronological holdout: train {train_start} … {train_end_d} ({eval_start} bars, in-sample / indicator warm-up only). \
             Evaluate {evaluate_start} … {evaluate_end} ({} bars, unseen). \
             Parameters were declared before this split; do not pick a winner here and treat it as a new out-of-sample test.",
            n - eval_start
        ),
        execution_note: format!(
            "Execution: {}. Position is all-in long, flat, or short. Paper / research only — no live orders.",
            settings.fill.label()
        ),
        metrics_note: "CAGR uses marked evaluate equity (252-day year). Completed-trade win rate = winning closed trades / closed trades. \
             An open trade at the end is marked to the last close for CAGR and is not a completed win. \
             The share of positive holding days is listed separately and is not a win rate."
            .into(),
        selection_note: "Every listed strategy is reported. Buy-and-hold is the benchmark, not a selected winner. \
             Train figures are in-sample. Re-tuning parameters after seeing evaluate numbers invalidates the holdout."
            .into(),
        paper_note: "Paper / research comparison only. This path never routes a brokerage order.".into(),
        train_start,
        train_end: train_end_d,
        evaluate_start,
        evaluate_end,
        evaluate_bars: n - eval_start,
        train_bars: eval_start,
        results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tr_core::Provenance;

    fn ts(day: i64) -> i64 {
        1_700_000_000 + day * 86_400
    }

    fn real(day: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
        Bar::new(ts(day), o, h, l, c, 1_000.0).with_provenance(Provenance::Real)
    }

    fn mono(n: usize, start: f64, step: f64) -> Vec<Bar> {
        (0..n as i64)
            .map(|i| {
                let c = start + i as f64 * step;
                real(i, c, c + 0.4, c - 0.4, c)
            })
            .collect()
    }

    fn settings_hold() -> RunSettings {
        RunSettings::default().with_strategies(vec![StrategySpec::BuyAndHold])
    }

    #[test]
    fn sample_and_unverified_bars_are_refused() {
        let sample: Vec<Bar> = (0..80)
            .map(|i| Bar::new(ts(i as i64), 10.0, 10.0, 10.0, 10.0, 1.0).with_provenance(Provenance::Sample))
            .collect();
        let err = compute_report("X", "X", &sample, &settings_hold(), "unit").unwrap_err();
        assert!(err.contains("SAMPLE"), "{err}");
        let unverified: Vec<Bar> = (0..80).map(|i| Bar::new(ts(i as i64), 10.0, 10.0, 10.0, 10.0, 1.0)).collect();
        let err = compute_report("X", "X", &unverified, &settings_hold(), "unit").unwrap_err();
        assert!(err.contains("UNVERIFIED"), "{err}");
        let err = compute_report("X", "X", &[], &settings_hold(), "unit").unwrap_err();
        assert!(err.contains("refused"), "{err}");
        assert!(require_backtest_bars(&sample, "Backtest").unwrap_err().contains("SAMPLE"));
    }

    #[test]
    fn mixed_sample_bar_taints_the_series() {
        let mut bars = mono(80, 10.0, 0.1);
        bars[10].provenance = Provenance::Sample;
        let err = compute_report("X", "X", &bars, &settings_hold(), "unit").unwrap_err();
        assert!(err.contains("SAMPLE"));
    }

    #[test]
    fn holdout_is_chronological_and_last_fraction() {
        assert_eq!(holdout_split(100, 0.20).unwrap(), 80);
        assert_eq!(holdout_split(100, parse_holdout("20%")).unwrap(), 80);
        assert_eq!(holdout_split(100, parse_holdout("20")).unwrap(), 80);
        assert!(holdout_split(10, 0.2).is_err());
        let bars = mono(100, 10.0, 0.05);
        let report = compute_report("X US Equity", "X", &bars, &settings_hold(), "unit").unwrap();
        assert_eq!(report.train_bars, 80);
        assert_eq!(report.evaluate_bars, 20);
        assert!(report.evaluate_start > report.train_end);
        assert!(report.holdout_note.contains("unseen"));
        assert!(report.selection_note.contains("Buy-and-hold"));
    }

    #[test]
    fn buy_and_hold_beats_zero_on_a_rise_and_pays_costs() {
        let bars = mono(80, 10.0, 0.2);
        let free = RunSettings { costs: CostModel { commission_bps: 0.0, slippage_bps: 0.0 }, ..settings_hold() };
        let paid = RunSettings { costs: CostModel { commission_bps: 50.0, slippage_bps: 50.0 }, ..settings_hold() };
        let a = compute_report("X", "X", &bars, &free, "unit").unwrap();
        let b = compute_report("X", "X", &bars, &paid, "unit").unwrap();
        let ra = a.results[0].evaluate.total_return.unwrap();
        let rb = b.results[0].evaluate.total_return.unwrap();
        assert!(ra > 0.0, "{ra}");
        assert!(rb < ra, "costs should reduce return: free={ra} paid={rb}");
        assert!(a.results[0].evaluate.cagr.unwrap() > 0.0);
        assert_eq!(a.results[0].evaluate.trades.completed, 0);
        assert_eq!(a.results[0].evaluate.trades.open_trades, 1);
        assert!(a.results[0].evaluate.trades.completed_win_rate.is_none());
    }

    #[test]
    fn completed_win_rate_is_not_holding_day_rate() {
        // Always-long on a rising series: one open trade, many up-days.
        let bars = mono(80, 10.0, 0.15);
        let report = compute_report("X", "X", &bars, &settings_hold(), "unit").unwrap();
        let ev = &report.results[0].evaluate.trades;
        assert_eq!(ev.completed, 0);
        assert!(ev.completed_win_rate.is_none());
        assert!(ev.holding_day_positive_rate.unwrap() > 0.8);
        assert!(report.metrics_note.to_ascii_lowercase().contains("not a win rate"));
    }

    #[test]
    fn next_open_fill_does_not_use_signal_bar_close() {
        // Gap up after a flat series: B&H must pay the next open, not yesterday's close.
        let mut bars = mono(80, 10.0, 0.0);
        let eval = holdout_split(80, 0.20).unwrap();
        bars[eval].open = 12.0;
        bars[eval].high = 12.2;
        bars[eval].low = 11.8;
        bars[eval].close = 12.0;
        for b in bars.iter_mut().skip(eval + 1) {
            b.open = 12.0;
            b.high = 12.2;
            b.low = 11.8;
            b.close = 12.0;
        }
        let settings = RunSettings { costs: CostModel { commission_bps: 0.0, slippage_bps: 0.0 }, ..settings_hold() };
        let report = compute_report("X", "X", &bars, &settings, "unit").unwrap();
        let ev = &report.results[0].evaluate;
        // Entered at 12, marked at 12 → ~0 return, not a fictional 20% from the 10 close.
        assert!(ev.total_return.unwrap().abs() < 0.02, "{}", ev.total_return.unwrap());
        assert!(report.execution_note.contains("next session open"));
    }

    #[test]
    fn sma_cross_emits_completed_trades_on_an_oscillator() {
        let mut bars = Vec::new();
        for i in 0..120 {
            let phase = ((i as f64) / 12.0).sin();
            let c = 20.0 + 6.0 * phase;
            bars.push(real(i, c, c + 0.3, c - 0.3, c));
        }
        let settings = RunSettings {
            costs: CostModel { commission_bps: 0.0, slippage_bps: 0.0 },
            holdout_frac: 0.35,
            strategies: vec![StrategySpec::BuyAndHold, StrategySpec::SmaCross { fast: 3, slow: 8 }],
            ..RunSettings::default()
        };
        let report = compute_report("X", "X", &bars, &settings, "unit").unwrap();
        assert_eq!(report.results.len(), 2);
        let sma = report.results.iter().find(|r| r.spec.id() == "sma_cross").unwrap();
        assert!(sma.evaluate.trades.completed >= 1, "expected at least one completed SMA trade");
        if let Some(wr) = sma.evaluate.trades.completed_win_rate {
            assert!((0.0..=1.0).contains(&wr));
        }
        assert!(sma.evaluate.trades.holding_day_positive_rate.is_some());
        assert_ne!(sma.evaluate.trades.completed_win_rate, sma.evaluate.trades.holding_day_positive_rate);
    }

    #[test]
    fn all_declared_strategies_are_reported_no_winner_pick() {
        let bars = mono(160, 15.0, 0.05);
        let settings = RunSettings::default();
        let report = compute_report("AAPL US Equity", "AAPL", &bars, &settings, "archive").unwrap();
        let ids: Vec<_> = report.results.iter().map(|r| r.spec.id()).collect();
        assert_eq!(ids[0], "buy_hold");
        assert!(ids.contains(&"adtrend"));
        assert!(ids.contains(&"sma_cross"));
        assert!(ids.contains(&"rsi_reversion"));
        assert!(report.results[0].benchmark);
        assert!(report.results.iter().skip(1).all(|r| !r.benchmark));
        assert!(report.settings_line.contains("seed=1"));
        assert_eq!(report.provenance, Provenance::Real);
        assert_eq!(report.source, "archive");
    }

    #[test]
    fn parse_strategies_always_keeps_buy_and_hold() {
        let only = parse_strategies("SMA 10 30", false).unwrap();
        assert!(only[0].is_benchmark());
        assert!(matches!(&only[1], StrategySpec::SmaCross { fast: 10, slow: 30 }));
        let all = parse_strategies("ALL", true).unwrap();
        assert!(matches!(&all[1], StrategySpec::AdaptiveTrend { long_only: false, .. }));
        assert!(parse_strategies("NOPE", false).is_err());
    }

    #[test]
    fn rsi_reversion_stays_flat_without_crosses() {
        let bars = mono(80, 50.0, 0.01);
        let settings = RunSettings {
            strategies: vec![
                StrategySpec::BuyAndHold,
                StrategySpec::RsiReversion { period: 14, oversold: 5.0, overbought: 95.0 },
            ],
            ..RunSettings::default()
        };
        let report = compute_report("X", "X", &bars, &settings, "unit").unwrap();
        let rsi = &report.results[1];
        assert_eq!(rsi.evaluate.trades.completed, 0);
        assert!(rsi.evaluate.exposure.unwrap() < 0.15);
    }

    #[test]
    fn reproducible_settings_travel_with_the_report() {
        let bars = mono(90, 8.0, 0.02);
        let settings = RunSettings {
            seed: 7,
            holdout_frac: 0.25,
            costs: CostModel { commission_bps: 1.0, slippage_bps: 2.0 },
            ..settings_hold()
        };
        let report = compute_report("MSFT US Equity", "MSFT", &bars, &settings, "Local Yahoo").unwrap();
        assert!(report.settings_line.contains("seed=7"));
        assert!(report.settings_line.contains("1.0 bps"));
        assert_eq!(report.settings.seed, 7);
        assert_eq!(report.ticker, "MSFT");
        assert!(report.paper_note.contains("Paper"));
        assert!(report.execution_note.contains("Paper"));
    }

    #[test]
    fn cached_verified_bars_are_accepted() {
        let mut bars = mono(80, 10.0, 0.1);
        for b in &mut bars {
            b.provenance = Provenance::Cached;
        }
        assert!(compute_report("X", "X", &bars, &settings_hold(), "cache").is_ok());
    }
}
