//! Paper backtest of intraday VWAP and opening-range setups versus being long the cash session.
//!
//! This is not the daily archive backtest. It reads verified regular-session bars
//! (1m, 5m, 15m, 30m, or 1h) and never places an order.
//!
//! Rules:
//! - Sample, missing, and unverified bars are refused. A missing bucket is skipped, not filled.
//! - Premarket and after-hours prints are ignored.
//! - A setup signal is known after the bar closes and fills at the **next regular open of the same session**.
//! - Nothing is carried overnight. An open trade is flattened at that session's last regular close.
//! - Session hold is the benchmark. It is long from each session's first regular open, because that
//!   position is declared before the open, and it is flat again at that session's last close.
//! - The last fraction of **sessions** is the unseen evaluate window. Train is in-sample.
//!   Every requested setup is reported. Nothing is selected as a winner.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use tr_core::{AssetClass, Bar, Provenance, Timeframe};

use crate::backtest::{CostModel, DEFAULT_SEED};
use crate::history::calendar::is_session;
use crate::intraday::{self, OPENING_MINUTES};

pub const MIN_EVALUATE_SESSIONS: usize = 4;
pub const MIN_TRAIN_SESSIONS: usize = 6;
pub const SESSION_YEAR: f64 = 252.0;

const OPERATION: &str = "Intraday backtest";

/// How many bars to ask a provider for. Yahoo's intraday window is shorter than this;
/// the provider clips. The engine does not invent the bars that were not sent.
pub fn fetch_limit(tf: Timeframe) -> usize {
    match tf {
        Timeframe::M1 => 12_000,
        Timeframe::M5 => 8_000,
        Timeframe::M15 => 4_000,
        Timeframe::M30 => 2_500,
        Timeframe::H1 => 1_500,
        _ => 0,
    }
}

pub fn supported_timeframe(tf: Timeframe) -> bool {
    fetch_limit(tf) > 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Setup {
    SessionHold,
    VwapCross,
    OpeningRange,
}

impl Setup {
    pub fn id(self) -> &'static str {
        match self {
            Self::SessionHold => "session_hold",
            Self::VwapCross => "vwap_cross",
            Self::OpeningRange => "opening_range",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::SessionHold => "Session hold",
            Self::VwapCross => "VWAP cross",
            Self::OpeningRange => "Opening range",
        }
    }

    pub fn is_benchmark(self) -> bool {
        matches!(self, Self::SessionHold)
    }

    fn order(self) -> u8 {
        match self {
            Self::SessionHold => 0,
            Self::VwapCross => 1,
            Self::OpeningRange => 2,
        }
    }

    pub fn params_line(self, allow_short: bool) -> String {
        let short = if allow_short { "short" } else { "flat" };
        match self {
            Self::SessionHold => {
                "long from each session's first regular open; flatten at that session's last regular close".into()
            }
            Self::VwapCross => format!(
                "after the close, long when close > session VWAP (hlc3×volume); {short} when close < VWAP; \
                 fill the next regular open of the same session; flatten at the session's last close"
            ),
            Self::OpeningRange => format!(
                "first 30 minutes set the range, known only after that window closes; \
                 later close above the high targets long; later close below the low targets {short}; \
                 inside the range the prior target stays; fill the next regular open; no overnight"
            ),
        }
    }
}

/// `ALL`, `VWAP`, `OR`, and an optional `5m` / `15m` / `1h` token.
/// A timeframe typed in `spec` wins over `tf_field`. Empty means every setup at 5 minutes.
pub fn parse_request(spec: &str, tf_field: &str) -> Result<(Vec<Setup>, Timeframe), String> {
    let field = if tf_field.trim().is_empty() { None } else { Some(parse_tf(tf_field.trim())?) };
    let mut from_spec: Option<Timeframe> = None;
    let mut setups = Vec::new();
    let mut all = false;
    let raw = spec.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("all") || raw.eq_ignore_ascii_case("default") {
        all = true;
    } else {
        for chunk in raw.split(|c: char| c == ',' || c == ';' || c == '|') {
            for word in chunk.split_whitespace() {
                match word.to_ascii_uppercase().as_str() {
                    "ALL" | "DEFAULT" => all = true,
                    "HOLD" | "BH" | "B&H" | "SESSION" | "BUYHOLD" => setups.push(Setup::SessionHold),
                    "VWAP" => setups.push(Setup::VwapCross),
                    "OR" | "ORB" | "OPEN" | "OPENING" | "RANGE" => setups.push(Setup::OpeningRange),
                    _ => {
                        let parsed = parse_tf(word)?;
                        if let Some(have) = from_spec
                            && have != parsed
                        {
                            return Err(format!(
                                "{OPERATION} takes one timeframe (saw {} and {})",
                                have.label(),
                                parsed.label()
                            ));
                        }
                        from_spec = Some(parsed);
                    }
                }
            }
        }
    }
    if all || setups.is_empty() {
        setups = vec![Setup::SessionHold, Setup::VwapCross, Setup::OpeningRange];
    }
    Ok((ensure_benchmark(setups), from_spec.or(field).unwrap_or(Timeframe::M5)))
}

fn parse_tf(raw: &str) -> Result<Timeframe, String> {
    let tf: Timeframe = raw.parse().map_err(|_| {
        format!("{OPERATION} needs a setup (ALL, HOLD, VWAP, OR) or a timeframe (1m, 5m, 15m, 30m, 1h), not {raw:?}")
    })?;
    if supported_timeframe(tf) {
        Ok(tf)
    } else {
        Err(format!(
            "{OPERATION} uses 1m, 5m, 15m, 30m, or 1h bars ({} has no cash-session grid for these setups)",
            tf.label()
        ))
    }
}

pub fn ensure_benchmark(mut setups: Vec<Setup>) -> Vec<Setup> {
    setups.sort_by_key(|s| s.order());
    setups.dedup();
    if !setups.iter().any(|s| s.is_benchmark()) {
        setups.insert(0, Setup::SessionHold);
    }
    setups
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSettings {
    pub holdout_frac: f64,
    pub costs: CostModel,
    pub seed: u64,
    pub allow_short: bool,
    pub timeframe: Timeframe,
    pub setups: Vec<Setup>,
}

impl Default for RunSettings {
    fn default() -> Self {
        Self {
            holdout_frac: crate::backtest::DEFAULT_HOLDOUT_FRAC,
            costs: CostModel::default(),
            seed: DEFAULT_SEED,
            allow_short: false,
            timeframe: Timeframe::M5,
            setups: vec![Setup::SessionHold, Setup::VwapCross, Setup::OpeningRange],
        }
    }
}

impl RunSettings {
    pub fn settings_line(&self) -> String {
        format!(
            "seed={} (deterministic) · {} regular session · fill=next regular open, flat at session close · costs={} · holdout=last {:.0}% of sessions · shorts={}",
            self.seed,
            self.timeframe.label(),
            self.costs.label(),
            self.holdout_frac * 100.0,
            if self.allow_short { "on (VWAP and opening range)" } else { "off (long/flat)" }
        )
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
    /// Winning closed round-trips / closed round-trips. Never the share of up bars.
    pub completed_win_rate: Option<f64>,
    pub avg_win: Option<f64>,
    pub avg_loss: Option<f64>,
    pub profit_factor: Option<f64>,
    pub expectancy: Option<f64>,
    pub open_trades: usize,
    pub rows: Vec<CompletedTrade>,
    /// Share of in-market bars whose marked equity rose. Not a win rate.
    pub holding_bar_positive_rate: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub label: String,
    pub start: String,
    pub end: String,
    pub sessions: usize,
    pub bars: usize,
    pub total_return: Option<f64>,
    pub session_cagr: Option<f64>,
    pub max_drawdown: Option<f64>,
    pub exposure: Option<f64>,
    pub trades: TradeStats,
    pub opening_skipped: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetupResult {
    pub spec: Setup,
    pub label: String,
    pub params: String,
    pub benchmark: bool,
    pub evaluate: Window,
    pub train: Window,
    /// Evaluate total return minus session-hold total return. Absent on the benchmark.
    pub vs_hold: Option<f64>,
}

/// Sessions that were present as prints but not traded, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Skips {
    pub holes: usize,
    pub missing_open: usize,
    pub ambiguous: usize,
    pub bad_price: usize,
}

impl Skips {
    fn line(self) -> String {
        let mut parts = Vec::new();
        if self.holes > 0 {
            parts.push(format!("{} with a missing bucket", self.holes));
        }
        if self.missing_open > 0 {
            parts.push(format!("{} with no opening print", self.missing_open));
        }
        if self.ambiguous > 0 {
            parts.push(format!("{} with two prints in one bucket", self.ambiguous));
        }
        if self.bad_price > 0 {
            parts.push(format!("{} with a non-positive or crossed price", self.bad_price));
        }
        if parts.is_empty() {
            "No session was skipped for a hole or a bad print.".into()
        } else {
            format!("Skipped {}. A later complete day does not stand in. Nothing was filled in.", parts.join("; "))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntradayReport {
    pub symbol: String,
    pub ticker: String,
    pub timeframe: Timeframe,
    pub provenance: Provenance,
    pub source: String,
    pub as_of: String,
    pub series_start: String,
    pub series_end: String,
    pub sessions: usize,
    pub partial_sessions: usize,
    pub bars: usize,
    pub skips: Skips,
    pub settings: RunSettings,
    pub settings_line: String,
    pub holdout_note: String,
    pub execution_note: String,
    pub metrics_note: String,
    pub selection_note: String,
    pub skip_note: String,
    pub opening_note: String,
    pub paper_note: String,
    pub train_start: String,
    pub train_end: String,
    pub evaluate_start: String,
    pub evaluate_end: String,
    pub evaluate_sessions: usize,
    pub train_sessions: usize,
    pub results: Vec<SetupResult>,
}

/// First evaluate session. Earlier sessions are train.
pub fn holdout_split(sessions: usize, frac: f64) -> Result<usize, String> {
    let need = MIN_EVALUATE_SESSIONS + MIN_TRAIN_SESSIONS;
    if sessions < need {
        return Err(format!(
            "{OPERATION} refused: need at least {need} complete regular sessions for a chronological split (have {sessions})"
        ));
    }
    let frac = if frac.is_finite() { frac.clamp(0.05, 0.80) } else { crate::backtest::DEFAULT_HOLDOUT_FRAC };
    let mut eval_n = ((sessions as f64) * frac).round() as usize;
    eval_n = eval_n.max(MIN_EVALUATE_SESSIONS);
    if sessions - eval_n < MIN_TRAIN_SESSIONS {
        eval_n = sessions - MIN_TRAIN_SESSIONS;
    }
    if eval_n < MIN_EVALUATE_SESSIONS || eval_n >= sessions {
        return Err(format!("{OPERATION} refused: chronological holdout does not leave enough sessions"));
    }
    Ok(sessions - eval_n)
}

#[derive(Clone)]
struct Session {
    date: NaiveDate,
    bars: Vec<Bar>,
    partial: bool,
}

struct Built {
    sessions: Vec<Session>,
    skips: Skips,
}

fn cash(class: AssetClass) -> bool {
    matches!(class, AssetClass::Equity | AssetClass::Etf | AssetClass::Index)
}

fn step_minutes(tf: Timeframe) -> Option<u32> {
    match tf {
        Timeframe::M1 => Some(1),
        Timeframe::M5 => Some(5),
        Timeframe::M15 => Some(15),
        Timeframe::M30 => Some(30),
        Timeframe::H1 => Some(60),
        _ => None,
    }
}

fn opening_need(tf: Timeframe) -> Option<usize> {
    let step = step_minutes(tf)?;
    if step == 0 || OPENING_MINUTES % step != 0 {
        return None;
    }
    Some((OPENING_MINUTES / step) as usize)
}

fn price_ok(b: &Bar) -> bool {
    b.open.is_finite()
        && b.open > 0.0
        && b.close.is_finite()
        && b.close > 0.0
        && b.high.is_finite()
        && b.low.is_finite()
        && b.high >= b.low
}

fn ny_day(ts: i64) -> NaiveDate {
    tr_core::ny::civil(ts).date()
}

fn day_text(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

fn sessionize(tf: Timeframe, bars: &[Bar]) -> Result<Built, String> {
    if !supported_timeframe(tf) {
        return Err(format!(
            "{OPERATION} refused: {} is not a cash-session timeframe (use 1m, 5m, 15m, 30m, or 1h)",
            tf.label()
        ));
    }
    let mut by_day: std::collections::BTreeMap<NaiveDate, std::collections::BTreeMap<i64, Bar>> = Default::default();
    let mut ambiguous_days = std::collections::BTreeSet::new();
    for b in bars {
        if !intraday::in_regular_session(b.ts) {
            continue;
        }
        let Some(bucket) = intraday::regular_bucket(tf, b.ts) else { continue };
        let day = by_day.entry(ny_day(b.ts)).or_default();
        if day.insert(bucket, *b).is_some() {
            ambiguous_days.insert(ny_day(b.ts));
        }
    }
    let mut skips = Skips::default();
    let mut sessions = Vec::new();
    for (date, buckets) in by_day {
        if !is_session(date) {
            continue;
        }
        if ambiguous_days.contains(&date) {
            skips.ambiguous += 1;
            continue;
        }
        let opens = intraday::regular_opens(date, tf);
        let Some(&open_bucket) = opens.first() else { continue };
        let Some((&first, _)) = buckets.iter().next() else { continue };
        let Some(&last) = buckets.keys().next_back() else { continue };
        if first != open_bucket {
            skips.missing_open += 1;
            continue;
        }
        let holed = opens.iter().any(|t| *t >= first && *t <= last && !buckets.contains_key(t));
        if holed {
            skips.holes += 1;
            continue;
        }
        let mut seq = Vec::new();
        let mut bad = false;
        for t in opens.iter().copied().take_while(|t| *t <= last) {
            let Some(bar) = buckets.get(&t) else {
                bad = true;
                break;
            };
            if !price_ok(bar) {
                bad = true;
                break;
            }
            seq.push(*bar);
        }
        if bad || seq.is_empty() {
            skips.bad_price += 1;
            continue;
        }
        let partial = opens.last().copied() != Some(last);
        sessions.push(Session { date, bars: seq, partial });
    }
    Ok(Built { sessions, skips })
}

struct OpenTrade {
    side: i8,
    entry_date: String,
    entry_px: f64,
    entry_i: usize,
}

struct Ledger {
    cash: f64,
    qty: f64,
    open: Option<OpenTrade>,
    completed: Vec<CompletedTrade>,
    equity: Vec<f64>,
    in_market: Vec<bool>,
    up_bars: usize,
    market_bars: usize,
}

impl Ledger {
    fn new() -> Self {
        Self {
            cash: 1.0,
            qty: 0.0,
            open: None,
            completed: Vec::new(),
            equity: Vec::new(),
            in_market: Vec::new(),
            up_bars: 0,
            market_bars: 0,
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

    fn flatten(&mut self, fill: f64, date: &str, i: usize) {
        let Some(open) = self.open.take() else {
            self.qty = 0.0;
            return;
        };
        self.cash += self.qty * fill;
        self.qty = 0.0;
        self.completed.push(CompletedTrade {
            entry_date: open.entry_date,
            exit_date: date.to_string(),
            side: open.side,
            entry_px: open.entry_px,
            exit_px: fill,
            ret: trade_return(open.side, open.entry_px, fill),
            bars_held: i.saturating_sub(open.entry_i).max(1),
        });
    }

    fn open_side(&mut self, side: i8, fill: f64, date: &str, i: usize) {
        if side == 0 || !(fill.is_finite() && fill > 0.0) || self.cash <= 0.0 {
            return;
        }
        let equity = self.cash;
        let qty = side as f64 * (equity / fill);
        self.cash = equity - qty * fill;
        self.qty = qty;
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
    if to > from { buy_fill(raw, cost) } else { sell_fill(raw, cost) }
}

fn exit_fill(side: i8, raw: f64, cost: f64) -> f64 {
    if side > 0 { sell_fill(raw, cost) } else { buy_fill(raw, cost) }
}

fn mark(ledger: &mut Ledger, close: f64, held: bool, prev: &mut f64) {
    let eq = if ledger.side() == 0 { ledger.cash } else { ledger.equity_at(close) };
    ledger.equity.push(eq);
    ledger.in_market.push(held);
    if held {
        ledger.market_bars += 1;
        if eq > *prev {
            ledger.up_bars += 1;
        }
    }
    *prev = eq;
}

fn run_hold(ledger: &mut Ledger, session: &Session, cost: f64, base: usize, prev: &mut f64) {
    let date = day_text(session.date);
    let n = session.bars.len();
    if let Some(first) = session.bars.first() {
        let fill = buy_fill(first.open, cost);
        ledger.open_side(1, fill, &date, base);
    }
    for (i, bar) in session.bars.iter().enumerate() {
        let held = ledger.side() != 0;
        if i + 1 == n && held {
            ledger.flatten(exit_fill(ledger.side(), bar.close, cost), &date, base + i);
        }
        mark(ledger, bar.close, held, prev);
    }
}

fn run_signals(ledger: &mut Ledger, session: &Session, signals: &[i8], cost: f64, base: usize, prev: &mut f64) {
    let date = day_text(session.date);
    let n = session.bars.len();
    for (i, bar) in session.bars.iter().enumerate() {
        if i > 0 {
            let want = signals[i - 1];
            let have = ledger.side();
            if want != have && bar.open.is_finite() && bar.open > 0.0 {
                if have != 0 {
                    ledger.flatten(fill_for_action(bar.open, have, 0, cost), &date, base + i);
                }
                if want != 0 {
                    let fill = fill_for_action(bar.open, 0, want, cost);
                    ledger.open_side(want, fill, &date, base + i);
                }
            }
        }
        let held = ledger.side() != 0;
        if i + 1 == n && held {
            ledger.flatten(exit_fill(ledger.side(), bar.close, cost), &date, base + i);
        }
        mark(ledger, bar.close, held, prev);
    }
}

fn run_flat(ledger: &mut Ledger, session: &Session, prev: &mut f64) {
    for bar in &session.bars {
        mark(ledger, bar.close, false, prev);
    }
}

fn vwap_signals(bars: &[Bar], allow_short: bool) -> Vec<i8> {
    let mut pv = 0.0;
    let mut vol = 0.0;
    let mut side = 0i8;
    let mut out = Vec::with_capacity(bars.len());
    for b in bars {
        let typical = b.hlc3();
        if typical.is_finite() && b.volume.is_finite() && b.volume > 0.0 {
            pv += typical * b.volume;
            vol += b.volume;
        }
        if vol > 0.0 {
            let vwap = pv / vol;
            if b.close > vwap {
                side = 1;
            } else if b.close < vwap {
                side = if allow_short { -1 } else { 0 };
            }
        }
        out.push(side);
    }
    out
}

enum RangePlan {
    Signals(Vec<i8>),
    /// This session's first 30 minutes are not all present.
    Incomplete,
    /// The bar size does not divide 30 minutes. Every session is incomplete for this setup.
    Unsupported,
}

fn opening_plan(tf: Timeframe, session: &Session, allow_short: bool) -> RangePlan {
    let Some(need) = opening_need(tf) else {
        return RangePlan::Unsupported;
    };
    if session.bars.len() < need {
        return RangePlan::Incomplete;
    }
    let opens = intraday::regular_opens(session.date, tf);
    if opens.len() < need {
        return RangePlan::Incomplete;
    }
    let mut high = f64::NEG_INFINITY;
    let mut low = f64::INFINITY;
    for (i, bucket) in opens.iter().take(need).enumerate() {
        let bar = &session.bars[i];
        if intraday::regular_bucket(tf, bar.ts) != Some(*bucket) {
            return RangePlan::Incomplete;
        }
        high = high.max(bar.high);
        low = low.min(bar.low);
    }
    if !(high.is_finite() && low.is_finite() && high >= low) {
        return RangePlan::Incomplete;
    }
    let mut side = 0i8;
    let mut signals = Vec::with_capacity(session.bars.len());
    for (i, bar) in session.bars.iter().enumerate() {
        if i + 1 >= need {
            if bar.close > high {
                side = 1;
            } else if bar.close < low {
                side = if allow_short { -1 } else { 0 };
            }
        }
        signals.push(side);
    }
    RangePlan::Signals(signals)
}

struct RunOut {
    ledger: Ledger,
    opening_skipped: usize,
}

fn run_range(
    setup: Setup,
    tf: Timeframe,
    sessions: &[Session],
    start: usize,
    end: usize,
    cost: f64,
    allow_short: bool,
) -> RunOut {
    let mut ledger = Ledger::new();
    let mut prev = 1.0;
    let mut opening_skipped = 0;
    let mut base = 0;
    for session in sessions.iter().take(end + 1).skip(start) {
        match setup {
            Setup::SessionHold => run_hold(&mut ledger, session, cost, base, &mut prev),
            Setup::VwapCross => {
                let signals = vwap_signals(&session.bars, allow_short);
                run_signals(&mut ledger, session, &signals, cost, base, &mut prev);
            }
            Setup::OpeningRange => match opening_plan(tf, session, allow_short) {
                RangePlan::Signals(signals) => {
                    run_signals(&mut ledger, session, &signals, cost, base, &mut prev);
                }
                RangePlan::Incomplete | RangePlan::Unsupported => {
                    opening_skipped += 1;
                    run_flat(&mut ledger, session, &mut prev);
                }
            },
        }
        base += session.bars.len();
    }
    RunOut { ledger, opening_skipped }
}

fn session_cagr(start: f64, end: f64, sessions: usize) -> Option<f64> {
    if start <= 0.0 || end <= 0.0 || sessions == 0 {
        return None;
    }
    let years = sessions as f64 / SESSION_YEAR;
    if years < 1.0 / SESSION_YEAR {
        return None;
    }
    Some((end / start).powf(1.0 / years) - 1.0)
}

fn max_drawdown(equity: &[f64]) -> Option<f64> {
    let mut peak = f64::NEG_INFINITY;
    let mut dd = 0.0_f64;
    if equity.is_empty() {
        return None;
    }
    for eq in equity {
        peak = peak.max(*eq);
        if peak > 0.0 {
            dd = dd.min(eq / peak - 1.0);
        }
    }
    Some(dd)
}

fn mean(xs: &[f64]) -> Option<f64> {
    if xs.is_empty() { None } else { Some(xs.iter().sum::<f64>() / xs.len() as f64) }
}

fn trade_stats(ledger: &Ledger) -> TradeStats {
    let wins: Vec<f64> = ledger.completed.iter().filter(|t| t.ret > 0.0).map(|t| t.ret).collect();
    let losses: Vec<f64> = ledger.completed.iter().filter(|t| t.ret <= 0.0).map(|t| t.ret).collect();
    let n = ledger.completed.len();
    let gross_win: f64 = wins.iter().sum();
    let gross_loss: f64 = losses.iter().map(|v| v.abs()).sum();
    TradeStats {
        completed: n,
        winners: wins.len(),
        completed_win_rate: (n > 0).then_some(wins.len() as f64 / n as f64),
        avg_win: mean(&wins),
        avg_loss: mean(&losses),
        profit_factor: (gross_loss > 0.0).then_some(gross_win / gross_loss),
        expectancy: mean(&ledger.completed.iter().map(|t| t.ret).collect::<Vec<_>>()),
        open_trades: usize::from(ledger.open.is_some()),
        rows: ledger.completed.clone(),
        holding_bar_positive_rate: (ledger.market_bars > 0)
            .then_some(ledger.up_bars as f64 / ledger.market_bars as f64),
    }
}

fn window_metrics(label: &str, sessions: &[Session], start: usize, end: usize, run: &RunOut) -> Window {
    let slice = &sessions[start..=end];
    let first = slice.first().map(|s| day_text(s.date)).unwrap_or_default();
    let last = slice.last().map(|s| day_text(s.date)).unwrap_or_default();
    let n = slice.len();
    let bars = run.ledger.equity.len();
    let last_eq = run.ledger.equity.last().copied().unwrap_or(1.0);
    let exposed = run.ledger.in_market.iter().filter(|v| **v).count();
    Window {
        label: label.into(),
        start: first,
        end: last,
        sessions: n,
        bars,
        total_return: Some(last_eq - 1.0),
        session_cagr: session_cagr(1.0, last_eq, n),
        max_drawdown: max_drawdown(&run.ledger.equity),
        exposure: (bars > 0).then_some(exposed as f64 / bars as f64),
        trades: trade_stats(&run.ledger),
        opening_skipped: run.opening_skipped,
    }
}

fn opening_note(tf: Timeframe, results: &[SetupResult]) -> String {
    let Some(or) = results.iter().find(|r| r.spec == Setup::OpeningRange) else {
        return "Opening range was not in this run.".into();
    };
    let skipped = or.evaluate.opening_skipped + or.train.opening_skipped;
    if opening_need(tf).is_none() {
        format!(
            "Opening range skipped all {skipped} sessions: {} bars do not divide the 30-minute window. Nothing was filled in.",
            tf.label()
        )
    } else if skipped > 0 {
        format!(
            "Opening range left {skipped} session(s) flat because the first {OPENING_MINUTES} minutes were incomplete. Those sessions were not filled from a later day."
        )
    } else {
        format!(
            "Opening range used the first {OPENING_MINUTES} minutes of each session. The range is known only after that window closes."
        )
    }
}

pub fn compute_report(
    symbol: &str,
    bars: &[Bar],
    settings: &RunSettings,
    source: &str,
) -> Result<IntradayReport, String> {
    if bars.is_empty() {
        return Err(format!("{OPERATION} refused: no bars. Sample and display prices are never used."));
    }
    Bar::require_verified(bars, OPERATION)?;
    let sec = crate::security(symbol);
    if !cash(sec.class) {
        return Err(format!(
            "{OPERATION} refused: {} has no US cash session. Sample prices are never used.",
            sec.ticker
        ));
    }
    if !supported_timeframe(settings.timeframe) {
        return Err(format!("{OPERATION} refused: {} is not a cash-session timeframe", settings.timeframe.label()));
    }
    let built = sessionize(settings.timeframe, bars)?;
    let n = built.sessions.len();
    let eval_start = match holdout_split(n, settings.holdout_frac) {
        Ok(i) => i,
        Err(e) => {
            return Err(format!("{e}. Eligible {n}. {}", built.skips.line()));
        }
    };
    let train_end = eval_start - 1;
    let setups = ensure_benchmark(settings.setups.clone());
    let cost = settings.costs.per_side_frac();
    let mut results = Vec::with_capacity(setups.len());
    for spec in setups {
        let eval_run =
            run_range(spec, settings.timeframe, &built.sessions, eval_start, n - 1, cost, settings.allow_short);
        let train_run = run_range(spec, settings.timeframe, &built.sessions, 0, train_end, cost, settings.allow_short);
        results.push(SetupResult {
            spec,
            label: spec.label().into(),
            params: spec.params_line(settings.allow_short),
            benchmark: spec.is_benchmark(),
            evaluate: window_metrics("evaluate (unseen holdout)", &built.sessions, eval_start, n - 1, &eval_run),
            train: window_metrics("train (in-sample — not for selection)", &built.sessions, 0, train_end, &train_run),
            vs_hold: None,
        });
    }
    let hold_ret = results.iter().find(|r| r.benchmark).and_then(|r| r.evaluate.total_return);
    for row in &mut results {
        row.vs_hold = if row.benchmark {
            None
        } else {
            match (row.evaluate.total_return, hold_ret) {
                (Some(a), Some(b)) => Some(a - b),
                _ => None,
            }
        };
    }
    let train_start = day_text(built.sessions[0].date);
    let train_end_d = day_text(built.sessions[train_end].date);
    let evaluate_start = day_text(built.sessions[eval_start].date);
    let evaluate_end = day_text(built.sessions[n - 1].date);
    let partial = built.sessions.iter().filter(|s| s.partial).count();
    let bar_count: usize = built.sessions.iter().map(|s| s.bars.len()).sum();
    let provenance = Bar::provenance(bars);
    let opening = opening_note(settings.timeframe, &results);
    let stored = RunSettings { setups: results.iter().map(|r| r.spec).collect(), ..settings.clone() };
    Ok(IntradayReport {
        symbol: sec.key(),
        ticker: sec.ticker,
        timeframe: settings.timeframe,
        provenance,
        source: provenance.source_label(source).into(),
        as_of: evaluate_end.clone(),
        series_start: train_start.clone(),
        series_end: evaluate_end.clone(),
        sessions: n,
        partial_sessions: partial,
        bars: bar_count,
        skips: built.skips,
        settings_line: stored.settings_line(),
        settings: stored,
        holdout_note: format!(
            "Chronological holdout by session, not by bar: train {train_start} … {train_end_d} ({eval_start} sessions, in-sample). \
             Evaluate {evaluate_start} … {evaluate_end} ({} sessions, unseen). \
             Setups reset every session, so train is not an indicator warm-up. \
             Do not pick a winner here and treat it as a new out-of-sample test.",
            n - eval_start
        ),
        execution_note: "Execution: signal after the bar close; fill at the next regular open of the same session \
             (no same-bar fill, no close-to-close lookahead). An open trade is flattened at that session's last regular close \
             — nothing is carried overnight. Session hold enters at the first regular open because that position is declared before the open. \
             Paper / research only — no live orders."
            .into(),
        metrics_note: "Total return compounds marked equity from 1 across the window, including costs on each fill and the session-close flatten. \
             CAGR treats each cash session as one day in a 252-session year. \
             Completed-trade win rate = winning closed round-trips / closed round-trips. \
             The share of up bars while in a position is listed separately and is not a win rate. \
             vs Hold is the evaluate total-return gap versus session hold, not a selected winner."
            .into(),
        selection_note: "Every listed setup is reported. Session hold is the benchmark, not a selected winner. \
             Train figures are in-sample. Re-tuning after seeing evaluate numbers invalidates the holdout."
            .into(),
        skip_note: {
            let mut line = built.skips.line();
            if partial > 0 {
                line.push_str(&format!(
                    " {partial} eligible session(s) end before the cash close and are flattened at the last regular bar present."
                ));
            }
            line
        },
        opening_note: opening,
        paper_note: "Paper / research comparison only. This path never routes a brokerage order.".into(),
        train_start,
        train_end: train_end_d,
        evaluate_start,
        evaluate_end,
        evaluate_sessions: n - eval_start,
        train_sessions: eval_start,
        results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, NaiveDate};

    fn dates(n: usize) -> Vec<NaiveDate> {
        let mut d = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let mut out = Vec::new();
        while out.len() < n {
            if is_session(d) {
                out.push(d);
            }
            d += Duration::days(1);
        }
        out
    }

    fn px(ts: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
        Bar::new(ts, o, h, l, c, 100.0).with_provenance(Provenance::Real)
    }

    fn flat_days(n: usize, tf: Timeframe, price: f64) -> Vec<Bar> {
        let mut bars = Vec::new();
        for date in dates(n) {
            for ts in intraday::regular_opens(date, tf) {
                bars.push(px(ts, price, price + 0.05, price - 0.05, price));
            }
        }
        bars
    }

    fn settings(setups: Vec<Setup>, tf: Timeframe) -> RunSettings {
        RunSettings {
            costs: CostModel { commission_bps: 0.0, slippage_bps: 0.0 },
            timeframe: tf,
            setups,
            ..RunSettings::default()
        }
    }

    fn report(bars: &[Bar], setups: Vec<Setup>, tf: Timeframe) -> IntradayReport {
        compute_report("AAPL US Equity", bars, &settings(setups, tf), "unit").unwrap()
    }

    fn setup<'a>(report: &'a IntradayReport, id: &str) -> &'a SetupResult {
        report.results.iter().find(|r| r.spec.id() == id).unwrap()
    }

    #[test]
    fn sample_unverified_and_non_cash_are_refused() {
        let mut bars = flat_days(12, Timeframe::M30, 10.0);
        bars[3].provenance = Provenance::Sample;
        let err =
            compute_report("AAPL", &bars, &settings(vec![Setup::SessionHold], Timeframe::M30), "unit").unwrap_err();
        assert!(err.contains("SAMPLE"), "{err}");

        let unverified: Vec<Bar> = flat_days(12, Timeframe::M30, 10.0)
            .into_iter()
            .map(|mut b| {
                b.provenance = Provenance::Missing;
                b
            })
            .collect();
        let err = compute_report("AAPL", &unverified, &settings(vec![Setup::SessionHold], Timeframe::M30), "unit")
            .unwrap_err();
        assert!(err.contains("UNVERIFIED"), "{err}");

        let real = flat_days(12, Timeframe::M30, 10.0);
        let err = compute_report("BTCUSD Curncy", &real, &settings(vec![Setup::SessionHold], Timeframe::M30), "unit")
            .unwrap_err();
        assert!(err.contains("no US cash session"), "{err}");

        let err =
            compute_report("AAPL", &real, &settings(vec![Setup::SessionHold], Timeframe::D1), "unit").unwrap_err();
        assert!(err.contains("1D") || err.contains("timeframe"), "{err}");
        assert!(compute_report("AAPL", &[], &RunSettings::default(), "unit").unwrap_err().contains("refused"));
    }

    #[test]
    fn cached_bars_run_and_holdout_is_by_session() {
        let mut bars = flat_days(16, Timeframe::M30, 10.0);
        for b in &mut bars {
            b.provenance = Provenance::Cached;
        }
        let report = report(&bars, vec![Setup::SessionHold], Timeframe::M30);
        assert_eq!(report.provenance, Provenance::Cached);
        assert_eq!(report.train_sessions, 12);
        assert_eq!(report.evaluate_sessions, 4);
        assert_eq!(report.evaluate_start, dates(16)[12].format("%Y-%m-%d").to_string());
        assert!(report.evaluate_start > report.train_end);
        assert!(report.holdout_note.contains("by session"));
        assert!(report.selection_note.contains("benchmark"));
        assert!(report.selection_note.to_ascii_lowercase().contains("not a selected winner"));
        assert!(holdout_split(9, 0.2).is_err());
    }

    #[test]
    fn session_hold_pays_costs_and_does_not_call_up_bars_a_win_rate() {
        let bars = flat_days(12, Timeframe::M30, 10.0);
        let free = report(&bars, vec![Setup::SessionHold], Timeframe::M30);
        let mut paid_settings = settings(vec![Setup::SessionHold], Timeframe::M30);
        paid_settings.costs = CostModel { commission_bps: 100.0, slippage_bps: 0.0 };
        let paid = compute_report("AAPL US Equity", &bars, &paid_settings, "unit").unwrap();
        let free_ret = setup(&free, "session_hold").evaluate.total_return.unwrap();
        let paid_ret = setup(&paid, "session_hold").evaluate.total_return.unwrap();
        assert!(free_ret.abs() < 1e-9, "{free_ret}");
        assert!(paid_ret < free_ret && paid_ret < 0.0, "{paid_ret}");
        let factor = 9.9_f64 / 10.1;
        let expect = factor.powi(4) - 1.0;
        assert!((paid_ret - expect).abs() < 1e-9, "paid={paid_ret} expect={expect}");

        // Open 10, dip, finish at 12: the round-trip wins, most in-market bars do not.
        let mut dipped = Vec::new();
        for date in dates(12) {
            let opens = intraday::regular_opens(date, Timeframe::M30);
            for (i, ts) in opens.iter().enumerate() {
                let close = if i + 1 == opens.len() { 12.0 } else { 9.0 };
                let open = if i == 0 { 10.0 } else { 9.0 };
                dipped.push(px(*ts, open, open.max(close) + 0.1, open.min(close) - 0.1, close));
            }
        }
        let dipped = report(&dipped, vec![Setup::SessionHold], Timeframe::M30);
        let hold = setup(&dipped, "session_hold");
        assert_eq!(hold.evaluate.trades.completed_win_rate, Some(1.0));
        let bars_up = hold.evaluate.trades.holding_bar_positive_rate.unwrap();
        assert!(bars_up < 0.5, "{bars_up}");
        assert_ne!(hold.evaluate.trades.completed_win_rate, hold.evaluate.trades.holding_bar_positive_rate);
        assert!(dipped.metrics_note.to_ascii_lowercase().contains("not a win rate"));
        assert_eq!(hold.evaluate.trades.open_trades, 0);
        assert!(hold.evaluate.trades.rows.iter().all(|t| t.entry_date == t.exit_date));
    }

    #[test]
    fn vwap_fills_the_next_open_and_flattens_before_the_next_session() {
        // Later sessions open at 100. A position carried overnight would flatten there
        // instead of at the prior session's last close (20).
        let mut bars = Vec::new();
        let days = dates(12);
        for (d, date) in days.iter().enumerate() {
            let opens = intraday::regular_opens(*date, Timeframe::M30);
            for (i, ts) in opens.iter().enumerate() {
                let first_open = if d == 0 { 10.0 } else { 100.0 };
                let (o, h, l, c) = if i == 0 {
                    (first_open, first_open, first_open, first_open)
                } else if i == 1 {
                    (first_open, first_open + 4.0, first_open, first_open + 4.0)
                } else if i == 2 {
                    (first_open + 5.0, first_open + 5.2, first_open + 4.8, first_open + 5.0)
                } else if i + 1 == opens.len() {
                    (20.0, 20.2, 19.8, 20.0)
                } else {
                    (first_open + 5.0, first_open + 5.2, first_open + 4.8, first_open + 5.0)
                };
                bars.push(px(*ts, o, h, l, c));
            }
        }
        // Day 0: first open 10, signal bar close 14, fill 15, exit 20.
        // Later days: first open 100 equals its own VWAP, signal close 104, fill 105, exit 20.
        // A position carried from the prior close would be flattened at 100.
        let report = report(&bars, vec![Setup::VwapCross], Timeframe::M30);
        let vwap = setup(&report, "vwap_cross");
        assert!(vwap.evaluate.trades.completed >= 1);
        for t in &vwap.evaluate.trades.rows {
            assert_eq!(t.entry_date, t.exit_date, "overnight carry: {t:?}");
            assert!((t.exit_px - 20.0).abs() < 1e-9, "flattened at the next open: {t:?}");
            assert!((t.entry_px - 105.0).abs() < 1e-9 || (t.entry_px - 15.0).abs() < 1e-9, "{t:?}");
            assert!((t.entry_px - 14.0).abs() > 1.0, "signal close was used as the fill: {t:?}");
        }
        let eval_days: Vec<_> = dates(12).into_iter().skip(8).collect();
        assert!(eval_days[0] > dates(12)[0]);
        assert!(vwap.evaluate.trades.rows.iter().all(|t| t.entry_date != dates(12)[0].format("%Y-%m-%d").to_string()));
        assert!(report.execution_note.contains("next regular open"));
        assert!(report.execution_note.contains("overnight"));
        assert!(report.results[0].benchmark);
        assert!(vwap.vs_hold.is_some());
    }

    #[test]
    fn opening_range_uses_the_cash_window_and_the_next_open() {
        let mut bars = Vec::new();
        let mut with_pre = Vec::new();
        for date in dates(12) {
            let opens = intraday::regular_opens(date, Timeframe::M30);
            let pre = tr_core::ny::to_utc(date.and_hms_opt(8, 0, 0).unwrap());
            with_pre.push(Bar::new(pre, 1.0, 100.0, 1.0, 1.0, 1_000_000.0).with_provenance(Provenance::Real));
            for (i, ts) in opens.iter().enumerate() {
                let (o, h, l, c) = if i == 0 {
                    (10.0, 10.5, 9.5, 10.0)
                } else if i == 1 {
                    (10.2, 11.2, 10.1, 11.0)
                } else if i == 2 {
                    (12.0, 12.2, 11.8, 12.0)
                } else if i == 3 {
                    (12.0, 12.1, 10.1, 10.2)
                } else {
                    (13.0, 13.2, 12.8, 13.0)
                };
                let bar = px(*ts, o, h, l, c);
                bars.push(bar);
                with_pre.push(bar);
            }
        }
        let plain = report(&bars, vec![Setup::OpeningRange], Timeframe::M30);
        let pre = report(&with_pre, vec![Setup::OpeningRange], Timeframe::M30);
        let or = setup(&plain, "opening_range");
        let or_pre = setup(&pre, "opening_range");
        assert_eq!(or.evaluate.trades.completed, or.evaluate.sessions);
        assert_eq!(or.evaluate.opening_skipped, 0);
        for t in &or.evaluate.trades.rows {
            assert!((t.entry_px - 12.0).abs() < 1e-9, "filled the signal bar: {t:?}");
            assert!((t.exit_px - 13.0).abs() < 1e-9, "left early inside the range: {t:?}");
            assert_eq!(t.entry_date, t.exit_date);
            assert_eq!(t.side, 1);
        }
        assert_eq!(or.evaluate.total_return, or_pre.evaluate.total_return);
        assert_eq!(or.evaluate.trades.completed, or_pre.evaluate.trades.completed);
        assert!(plain.opening_note.contains("30"));
    }

    #[test]
    fn a_hole_is_skipped_and_a_short_opening_window_stays_flat() {
        let days = dates(11);
        let mut bars = Vec::new();
        for (i, date) in days.iter().enumerate() {
            let mut opens = intraday::regular_opens(*date, Timeframe::M30);
            if i == 0 {
                opens.remove(3);
            }
            for ts in opens {
                bars.push(px(ts, 10.0, 10.2, 9.8, 10.0));
            }
        }
        let holed = report(&bars, vec![Setup::SessionHold], Timeframe::M30);
        assert_eq!(holed.skips.holes, 1);
        assert_eq!(holed.sessions, 10);
        assert_eq!(holed.series_start, days[1].format("%Y-%m-%d").to_string());
        assert!(holed.skip_note.contains("missing bucket"));

        let mut short = Vec::new();
        for date in dates(12) {
            for ts in intraday::regular_opens(date, Timeframe::M5).into_iter().take(5) {
                short.push(px(ts, 10.0, 11.0, 9.0, 10.5));
            }
        }
        let short = report(&short, vec![Setup::OpeningRange, Setup::SessionHold], Timeframe::M5);
        let or = setup(&short, "opening_range");
        let hold = setup(&short, "session_hold");
        assert_eq!(or.evaluate.trades.completed, 0);
        assert!(or.evaluate.opening_skipped + or.train.opening_skipped >= 10);
        assert!(hold.evaluate.trades.completed >= 1);
        assert!(short.opening_note.contains("incomplete"));
        assert_eq!(short.partial_sessions, short.sessions);
    }

    #[test]
    fn one_hour_bars_do_not_invent_an_opening_range() {
        let mut bars = Vec::new();
        for date in dates(12) {
            let opens = intraday::regular_opens(date, Timeframe::H1);
            for (i, ts) in opens.iter().enumerate() {
                let close = if i + 1 == opens.len() { 11.0 } else { 10.0 };
                let open = if i == 0 { 10.0 } else { close };
                bars.push(px(*ts, open, close + 0.1, open - 0.1, close));
            }
        }
        let report = report(&bars, vec![Setup::OpeningRange, Setup::SessionHold], Timeframe::H1);
        let or = setup(&report, "opening_range");
        let hold = setup(&report, "session_hold");
        assert_eq!(or.evaluate.trades.completed, 0);
        assert!(or.evaluate.total_return.unwrap().abs() < 1e-12);
        assert!(hold.evaluate.total_return.unwrap() > 0.0);
        assert!(report.opening_note.contains("do not divide"));
        assert!(or.vs_hold.unwrap() < 0.0);
    }

    #[test]
    fn shorts_stay_off_until_asked_and_every_setup_is_listed() {
        let mut bars = Vec::new();
        for date in dates(12) {
            let opens = intraday::regular_opens(date, Timeframe::M30);
            for (i, ts) in opens.iter().enumerate() {
                let (o, c) = if i == 0 {
                    (10.0, 10.0)
                } else if i == 1 {
                    (10.0, 8.0)
                } else if i == 2 {
                    (7.0, 7.0)
                } else {
                    (6.0, 6.0)
                };
                bars.push(px(*ts, o, o.max(c), o.min(c), c));
            }
        }
        let flat = report(&bars, vec![Setup::VwapCross], Timeframe::M30);
        assert_eq!(setup(&flat, "vwap_cross").evaluate.trades.completed, 0);
        let mut short_settings = settings(vec![Setup::VwapCross], Timeframe::M30);
        short_settings.allow_short = true;
        let shorted = compute_report("AAPL US Equity", &bars, &short_settings, "unit").unwrap();
        let vwap = setup(&shorted, "vwap_cross");
        assert!(vwap.evaluate.trades.completed >= 1);
        assert!(vwap.evaluate.trades.rows.iter().all(|t| t.side == -1 && (t.entry_px - 7.0).abs() < 1e-9));

        let all = report(&flat_days(12, Timeframe::M5, 10.0), parse_request("ALL", "5m").unwrap().0, Timeframe::M5);
        let ids: Vec<_> = all.results.iter().map(|r| r.spec.id()).collect();
        assert_eq!(ids, vec!["session_hold", "vwap_cross", "opening_range"]);
        assert!(all.results[0].benchmark);
        assert!(all.paper_note.contains("Paper"));
        let (only, tf) = parse_request("OR 15m", "5m").unwrap();
        assert_eq!(tf, Timeframe::M15);
        assert_eq!(only, vec![Setup::SessionHold, Setup::OpeningRange]);
        assert!(parse_request("NOPE", "").is_err());
        assert!(parse_request("VWAP", "2m").is_err());
        assert_eq!(parse_request("", "").unwrap().1, Timeframe::M5);
    }

    #[test]
    fn premarket_does_not_move_vwap_and_settings_travel_with_the_report() {
        let mut bars = Vec::new();
        let mut clean = Vec::new();
        for date in dates(12) {
            let pre = tr_core::ny::to_utc(date.and_hms_opt(8, 0, 0).unwrap());
            bars.push(Bar::new(pre, 1.0, 1.0, 1.0, 1.0, 5_000_000.0).with_provenance(Provenance::Real));
            for ts in intraday::regular_opens(date, Timeframe::M30) {
                let bar = px(ts, 10.0, 10.4, 9.8, 10.2);
                bars.push(bar);
                clean.push(bar);
            }
        }
        let a = report(&bars, vec![Setup::VwapCross], Timeframe::M30);
        let b = report(&clean, vec![Setup::VwapCross], Timeframe::M30);
        assert_eq!(setup(&a, "vwap_cross").evaluate.total_return, setup(&b, "vwap_cross").evaluate.total_return);

        let mut custom = settings(vec![Setup::SessionHold], Timeframe::M15);
        custom.seed = 7;
        custom.holdout_frac = 0.25;
        custom.costs = CostModel::default();
        let bars = flat_days(16, Timeframe::M15, 8.0);
        let report = compute_report("MSFT US Equity", &bars, &custom, "yahoo 15m").unwrap();
        assert!(report.settings_line.contains("seed=7"));
        assert!(report.settings_line.contains("15m"));
        assert_eq!(report.ticker, "MSFT");
        assert_eq!(report.timeframe, Timeframe::M15);
        assert_eq!(report.source, "yahoo 15m");
        let doc = crate::portable::intraday_backtest_document(&report, "2026-01-28T00:00:00Z").unwrap();
        assert_eq!(doc.kind, crate::portable::Kind::IntradayBacktest);
        assert!(!doc.sample);
        assert_eq!(doc.source, "yahoo 15m");
    }
}
