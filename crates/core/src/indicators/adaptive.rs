//! Whats-News Adaptive Trend: multi-horizon adaptive baselines, sticky regimes,
//! destination bands and ratcheting trailing stops.
//!
//! Conceptual port of Whats-News `adaptive_trend.py`. The functions here are
//! provenance-agnostic. Research overlays that claim live archive analysis must
//! call [`adaptive_trend_research`] so sample, unverified or empty series are
//! refused — never a silent fallback. Python is not vendored into the runtime.

use super::ma::rma;
use super::momentum::efficiency_ratio;
use super::volatility::atr;
use super::{NAN, Ohlcv};
use crate::{Bar, Provenance};

/// Kaufman (squared) or ADMA (linear) efficiency-ratio smoothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AdaptiveMethod {
    Kama,
    Adma,
}

impl AdaptiveMethod {
    pub fn from_param(v: f64) -> Self {
        if v >= 0.5 { Self::Adma } else { Self::Kama }
    }

    pub fn as_param(self) -> f64 {
        match self {
            Self::Kama => 0.0,
            Self::Adma => 1.0,
        }
    }
}

/// Whats-News Adaptive Trend defaults (SB 10/2/30, MB 20/2/60, LB 40/2/120).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AdaptiveTrendParams {
    pub method: AdaptiveMethod,
    pub sb_er: usize,
    pub sb_fast: usize,
    pub sb_slow: usize,
    pub mb_er: usize,
    pub mb_fast: usize,
    pub mb_slow: usize,
    pub lb_er: usize,
    pub lb_fast: usize,
    pub lb_slow: usize,
    pub atr_n: usize,
    pub confirm_mult: f64,
    pub min_hold: usize,
    pub session_atr: bool,
    pub session_reset: bool,
}

impl Default for AdaptiveTrendParams {
    fn default() -> Self {
        Self {
            method: AdaptiveMethod::Kama,
            sb_er: 10,
            sb_fast: 2,
            sb_slow: 30,
            mb_er: 20,
            mb_fast: 2,
            mb_slow: 60,
            lb_er: 40,
            lb_fast: 2,
            lb_slow: 120,
            atr_n: 20,
            confirm_mult: 0.25,
            min_hold: 10,
            session_atr: false,
            session_reset: false,
        }
    }
}

impl AdaptiveTrendParams {
    /// Registry parameters: ATR length, confirm multiple, method (0 = KAMA, 1 = ADMA),
    /// medium-baseline ER length, minimum regime hold. Missing trailing values keep
    /// the defaults, so older 3-parameter specs behave exactly as before.
    pub fn from_registry(p: &[f64]) -> Self {
        let mut s = Self::default();
        let finite = |k: usize| p.get(k).copied().filter(|v| v.is_finite());
        if let Some(n) = finite(0) {
            s.atr_n = (n as usize).max(1);
        }
        if let Some(c) = finite(1) {
            s.confirm_mult = c;
        }
        if let Some(m) = finite(2) {
            s.method = AdaptiveMethod::from_param(m);
        }
        if let Some(er) = finite(3) {
            s.mb_er = (er as usize).max(1);
        }
        if let Some(hold) = finite(4) {
            s.min_hold = hold as usize;
        }
        s
    }

    /// Inverse of [`Self::from_registry`] for the five registry-backed fields.
    pub fn to_registry(&self) -> Vec<f64> {
        vec![self.atr_n as f64, self.confirm_mult, self.method.as_param(), self.mb_er as f64, self.min_hold as f64]
    }
}

/// One Adaptive Trend snapshot, aligned with the input bars (NaN / 0 during warm-up).
#[derive(Debug, Clone)]
pub struct AdaptiveTrend {
    pub sb: Vec<f64>,
    pub mb: Vec<f64>,
    pub lb: Vec<f64>,
    pub sdb: Vec<f64>,
    pub mrt: Vec<f64>,
    pub mdb: Vec<f64>,
    pub lrt: Vec<f64>,
    pub ldb: Vec<f64>,
    /// −1 short, 0 none, +1 long.
    pub short_state: Vec<i8>,
    pub medium_state: Vec<i8>,
    pub long_state: Vec<i8>,
    pub entry_long: Vec<bool>,
    pub entry_short: Vec<bool>,
    pub atr: Vec<f64>,
    pub confidence: Vec<f64>,
    pub sigma: Vec<f64>,
}

impl AdaptiveTrend {
    fn empty(len: usize) -> Self {
        Self {
            sb: vec![NAN; len],
            mb: vec![NAN; len],
            lb: vec![NAN; len],
            sdb: vec![NAN; len],
            mrt: vec![NAN; len],
            mdb: vec![NAN; len],
            lrt: vec![NAN; len],
            ldb: vec![NAN; len],
            short_state: vec![0; len],
            medium_state: vec![0; len],
            long_state: vec![0; len],
            entry_long: vec![false; len],
            entry_short: vec![false; len],
            atr: vec![NAN; len],
            confidence: vec![NAN; len],
            sigma: vec![NAN; len],
        }
    }

    /// Last-bar HUD: short / medium / long regime and trailing-stop guides.
    pub fn hud(&self) -> String {
        let i = self.medium_state.len().saturating_sub(1);
        let side = |s: i8| match s {
            1 => '+',
            -1 => '-',
            _ => '0',
        };
        let (sh, md, lg) = (
            self.short_state.get(i).copied().unwrap_or(0),
            self.medium_state.get(i).copied().unwrap_or(0),
            self.long_state.get(i).copied().unwrap_or(0),
        );
        let mrt = self.mrt.get(i).copied().filter(|v| v.is_finite());
        let lrt = self.lrt.get(i).copied().filter(|v| v.is_finite());
        let stops = match (mrt, lrt) {
            (Some(m), Some(l)) => format!("  · MRT {m:.2}  LRT {l:.2}"),
            (Some(m), None) => format!("  · MRT {m:.2}"),
            (None, Some(l)) => format!("  · LRT {l:.2}"),
            (None, None) => String::new(),
        };
        format!(
            "AT  S{} M{} L{}{stops}  · medium manages the trade; MRT/LRT are ratcheting stops",
            side(sh),
            side(md),
            side(lg)
        )
    }

    /// Close prices of medium-regime flips, for chart dots. Warm-up stays NaN.
    pub fn flip_dots(&self, close: &[f64]) -> (Vec<f64>, Vec<f64>) {
        let mark = |flag: &[bool]| -> Vec<f64> {
            flag.iter().zip(close).map(|(on, c)| if *on && c.is_finite() { *c } else { NAN }).collect()
        };
        (mark(&self.entry_long), mark(&self.entry_short))
    }
}

/// Shared gate for Adaptive Trend research overlays. Sample / unverified / empty
/// series are refused; there is no silent fallback onto demo bars.
pub fn require_adaptive_bars(bars: &[Bar], operation: &str) -> Result<(), String> {
    if bars.is_empty() {
        return Err(format!(
            "{operation} refused: no bars loaded. Sample or display prices are never used as archive analysis."
        ));
    }
    let provenance = Bar::provenance(bars);
    if matches!(provenance, Provenance::Sample) {
        return Err(format!(
            "{operation} refused: {}. Sample / demo bars never drive Adaptive Trend overlays that claim live archive analysis.",
            provenance.label()
        ));
    }
    provenance.require_verified(operation)?;
    Ok(())
}

/// Compute Adaptive Trend only on verified real/cached bars.
pub fn adaptive_trend_research(bars: &[Bar], p: AdaptiveTrendParams) -> Result<AdaptiveTrend, String> {
    require_adaptive_bars(bars, "Adaptive Trend")?;
    Ok(adaptive_trend(&Ohlcv::new(bars), p))
}

/// Adaptive Trend on any OHLCV. Warm-up positions are NaN / flat (state 0).
pub fn adaptive_trend(o: &Ohlcv, p: AdaptiveTrendParams) -> AdaptiveTrend {
    adaptive_trend_with(o, p, None)
}

pub fn adaptive_trend_with(o: &Ohlcv, p: AdaptiveTrendParams, starts: Option<&[bool]>) -> AdaptiveTrend {
    let len = o.len();
    if len == 0 {
        return AdaptiveTrend::empty(0);
    }
    if p.session_reset
        && let Some(starts) = starts
    {
        let mut out = AdaptiveTrend::empty(len);
        let cuts: Vec<_> = std::iter::once(0)
            .chain((1..len).filter(|i| starts.get(*i) == Some(&true)))
            .chain(std::iter::once(len))
            .collect();
        for w in cuts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let slice = Ohlcv {
                ts: o.ts[a..b].to_vec(),
                open: o.open[a..b].to_vec(),
                high: o.high[a..b].to_vec(),
                low: o.low[a..b].to_vec(),
                close: o.close[a..b].to_vec(),
                volume: o.volume[a..b].to_vec(),
            };
            let part = adaptive_trend_with(
                &slice,
                AdaptiveTrendParams { session_reset: false, ..p },
                Some(&vec![false; b - a]),
            );
            out.sb[a..b].copy_from_slice(&part.sb);
            out.mb[a..b].copy_from_slice(&part.mb);
            out.lb[a..b].copy_from_slice(&part.lb);
            out.sdb[a..b].copy_from_slice(&part.sdb);
            out.mrt[a..b].copy_from_slice(&part.mrt);
            out.mdb[a..b].copy_from_slice(&part.mdb);
            out.lrt[a..b].copy_from_slice(&part.lrt);
            out.ldb[a..b].copy_from_slice(&part.ldb);
            out.short_state[a..b].copy_from_slice(&part.short_state);
            out.medium_state[a..b].copy_from_slice(&part.medium_state);
            out.long_state[a..b].copy_from_slice(&part.long_state);
            out.entry_long[a..b].copy_from_slice(&part.entry_long);
            out.entry_short[a..b].copy_from_slice(&part.entry_short);
            out.atr[a..b].copy_from_slice(&part.atr);
            out.confidence[a..b].copy_from_slice(&part.confidence);
            out.sigma[a..b].copy_from_slice(&part.sigma);
        }
        return out;
    }
    let shared = Shared::new(o, p, starts);
    let leg = MediumLeg::new(&shared.hlc3, p);
    let Regimes { short_state, medium_state, long_state, entry_long, entry_short, confidence } =
        regimes(o, p, &shared, &leg, p.min_hold);
    let Shared { sb, lb, a, .. } = shared;
    let MediumLeg { mb, .. } = leg;

    let sdb = ratchet_band(&sb, &medium_state, &a, 2.0, BandKind::Tp);
    let mrt = ratchet_band(&mb, &medium_state, &a, 2.25, BandKind::Stop);
    let mdb = ratchet_band(&mb, &medium_state, &a, 4.5, BandKind::Tp);
    let lrt = ratchet_band(&lb, &long_state, &a, 2.25, BandKind::Stop);
    let ldb = ratchet_band(&lb, &long_state, &a, 4.5, BandKind::Tp);

    let residual: Vec<_> = o.close.iter().zip(&mb).map(|(c, m)| (c - m).powi(2)).collect();
    let sigma = rma(&residual, p.atr_n.saturating_mul(2)).into_iter().map(f64::sqrt).collect();
    AdaptiveTrend {
        confidence,
        sigma,
        sb,
        mb,
        lb,
        sdb,
        mrt,
        mdb,
        lrt,
        ldb,
        short_state,
        medium_state,
        long_state,
        entry_long,
        entry_short,
        atr: a,
    }
}

/// State that does not depend on the medium ER length or the minimum hold.
struct Shared {
    hlc3: Vec<f64>,
    sb: Vec<f64>,
    lb: Vec<f64>,
    a: Vec<f64>,
    sb_slope: Vec<f64>,
    lb_slope: Vec<f64>,
}
impl Shared {
    fn new(o: &Ohlcv, p: AdaptiveTrendParams, starts: Option<&[bool]>) -> Self {
        let hlc3 = o.hlc3();
        let sb = adaptive_ma(&hlc3, p.sb_er, p.sb_fast, p.sb_slow, p.method);
        let lb = adaptive_ma(&hlc3, p.lb_er, p.lb_fast, p.lb_slow, p.method);
        let a = match starts.filter(|_| p.session_atr) {
            Some(starts) => atr_session(o, p.atr_n, starts),
            None => atr(o, p.atr_n),
        };
        let sb_slope = diff(&sb);
        let lb_slope = diff(&lb);
        Self { hlc3, sb, lb, a, sb_slope, lb_slope }
    }
}

/// Medium baseline for one ER length (independent of the minimum hold).
struct MediumLeg {
    mb: Vec<f64>,
    mb_slope: Vec<f64>,
    er: Vec<f64>,
}
impl MediumLeg {
    fn new(hlc3: &[f64], p: AdaptiveTrendParams) -> Self {
        let mb = adaptive_ma(hlc3, p.mb_er, p.mb_fast, p.mb_slow, p.method);
        let mb_slope = diff(&mb);
        let er = efficiency_ratio(hlc3, p.mb_er);
        Self { mb, mb_slope, er }
    }
}

struct Regimes {
    short_state: Vec<i8>,
    medium_state: Vec<i8>,
    long_state: Vec<i8>,
    entry_long: Vec<bool>,
    entry_short: Vec<bool>,
    confidence: Vec<f64>,
}

fn regimes(o: &Ohlcv, p: AdaptiveTrendParams, s: &Shared, m: &MediumLeg, min_hold: usize) -> Regimes {
    let len = o.len();
    let (sb, lb, a, mb) = (&s.sb, &s.lb, &s.a, &m.mb);
    let mut sh_long = vec![false; len];
    let mut sh_short = vec![false; len];
    let mut med_long = vec![false; len];
    let mut med_short = vec![false; len];
    let mut lng_long = vec![false; len];
    let mut lng_short = vec![false; len];
    for i in 0..len {
        let confirm = p.confirm_mult * a[i];
        if !confirm.is_finite() {
            continue;
        }
        let c = o.close[i];
        sh_long[i] = c > sb[i] + 0.5 * confirm && s.sb_slope[i] > 0.0;
        sh_short[i] = c < sb[i] - 0.5 * confirm && s.sb_slope[i] < 0.0;
        med_long[i] = sb[i] > mb[i] + confirm && m.mb_slope[i] > 0.0 && c > mb[i];
        med_short[i] = sb[i] < mb[i] - confirm && m.mb_slope[i] < 0.0 && c < mb[i];
        lng_long[i] = mb[i] > lb[i] + confirm && s.lb_slope[i] > 0.0;
        lng_short[i] = mb[i] < lb[i] - confirm && s.lb_slope[i] < 0.0;
    }

    let short_state = sticky_state(&sh_long, &sh_short, min_hold);
    let medium_state = sticky_state(&med_long, &med_short, min_hold);
    let long_state = sticky_state(&lng_long, &lng_short, min_hold);

    let mut entry_long = vec![false; len];
    let mut entry_short = vec![false; len];
    for i in 1..len {
        if medium_state[i] == 1 && medium_state[i - 1] != 1 {
            entry_long[i] = true;
        }
        if medium_state[i] == -1 && medium_state[i - 1] != -1 {
            entry_short[i] = true;
        }
    }

    let er = &m.er;
    let confidence = (0..len)
        .map(|i| {
            if !mb[i].is_finite() || !a[i].is_finite() || !er[i].is_finite() {
                return NAN;
            }
            let state = medium_state[i];
            if state == 0 {
                return 0.0;
            }
            let separation = if a[i] > 0.0 { ((sb[i] - mb[i]).abs() / (2.0 * a[i])).clamp(0.0, 1.0) } else { 0.0 };
            let agree = (u8::from(short_state[i] == state) + u8::from(long_state[i] == state)) as f64 / 2.0;
            (0.40 * er[i] + 0.35 * separation + 0.25 * agree).clamp(0.0, 1.0)
        })
        .collect();
    Regimes { short_state, medium_state, long_state, entry_long, entry_short, confidence }
}

/// The medium-regime outputs a parameter grid needs from one Adaptive Trend run.
#[derive(Debug, Clone, PartialEq)]
pub struct GridTrend {
    pub confidence: Vec<f64>,
    pub medium_state: Vec<i8>,
    pub entry_long: Vec<bool>,
    pub entry_short: Vec<bool>,
}

/// Adaptive Trend over a medium-ER × min-hold grid, bit-identical to calling
/// [`adaptive_trend_with`] once per cell with `mb_er`/`min_hold` replaced.
/// Short/long baselines and ATR are computed once; the medium baseline once per
/// ER length; only the sticky state machine and confidence run per cell.
/// Output order is `holds` outer, `ers` inner.
pub fn adaptive_trend_grid(
    o: &Ohlcv,
    p: AdaptiveTrendParams,
    starts: Option<&[bool]>,
    ers: &[usize],
    holds: &[usize],
) -> Vec<GridTrend> {
    let len = o.len();
    if p.session_reset
        && let Some(starts) = starts
        && len > 0
    {
        let mut out: Vec<_> = (0..ers.len() * holds.len())
            .map(|_| GridTrend {
                confidence: vec![NAN; len],
                medium_state: vec![0; len],
                entry_long: vec![false; len],
                entry_short: vec![false; len],
            })
            .collect();
        let cuts: Vec<_> = std::iter::once(0)
            .chain((1..len).filter(|i| starts.get(*i) == Some(&true)))
            .chain(std::iter::once(len))
            .collect();
        for w in cuts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let slice = Ohlcv {
                ts: o.ts[a..b].to_vec(),
                open: o.open[a..b].to_vec(),
                high: o.high[a..b].to_vec(),
                low: o.low[a..b].to_vec(),
                close: o.close[a..b].to_vec(),
                volume: o.volume[a..b].to_vec(),
            };
            let parts = adaptive_trend_grid(
                &slice,
                AdaptiveTrendParams { session_reset: false, ..p },
                Some(&vec![false; b - a]),
                ers,
                holds,
            );
            for (cell, part) in out.iter_mut().zip(parts) {
                cell.confidence[a..b].copy_from_slice(&part.confidence);
                cell.medium_state[a..b].copy_from_slice(&part.medium_state);
                cell.entry_long[a..b].copy_from_slice(&part.entry_long);
                cell.entry_short[a..b].copy_from_slice(&part.entry_short);
            }
        }
        return out;
    }
    if len == 0 {
        return (0..ers.len() * holds.len())
            .map(|_| GridTrend { confidence: vec![], medium_state: vec![], entry_long: vec![], entry_short: vec![] })
            .collect();
    }
    let shared = Shared::new(o, p, starts);
    let legs: Vec<_> =
        ers.iter().map(|&mb_er| MediumLeg::new(&shared.hlc3, AdaptiveTrendParams { mb_er, ..p })).collect();
    let mut out = Vec::with_capacity(ers.len() * holds.len());
    for &min_hold in holds {
        for (&mb_er, leg) in ers.iter().zip(&legs) {
            let r = regimes(o, AdaptiveTrendParams { mb_er, min_hold, ..p }, &shared, leg, min_hold);
            out.push(GridTrend {
                confidence: r.confidence,
                medium_state: r.medium_state,
                entry_long: r.entry_long,
                entry_short: r.entry_short,
            });
        }
    }
    out
}

/// New York calendar session boundaries, including the first observed bar.
pub fn session_starts(ts: &[i64]) -> Vec<bool> {
    ts.iter().enumerate().map(|(i, t)| i == 0 || crate::ny::date_key(*t) != crate::ny::date_key(ts[i - 1])).collect()
}

/// Wilder ATR with the overnight gap omitted at session starts.
pub fn atr_session(o: &Ohlcv, n: usize, starts: &[bool]) -> Vec<f64> {
    let tr: Vec<_> = (0..o.len())
        .map(|i| {
            if i == 0 || starts.get(i).copied().unwrap_or(false) || !o.close[i - 1].is_finite() {
                o.high[i] - o.low[i]
            } else {
                o.high[i].max(o.close[i - 1]) - o.low[i].min(o.close[i - 1])
            }
        })
        .collect();
    rma(&tr, n)
}

/// Efficiency-ratio adaptive MA. KAMA squares the scaled constant; ADMA does not.
pub fn adaptive_ma(src: &[f64], er_len: usize, fast: usize, slow: usize, method: AdaptiveMethod) -> Vec<f64> {
    let n = src.len();
    let mut out = vec![NAN; n];
    if er_len == 0 || n == 0 {
        return out;
    }
    let first = match src.iter().position(|v| v.is_finite()) {
        Some(i) => i,
        None => return out,
    };
    let seed = first + er_len;
    if seed > n {
        return out;
    }
    let fast_sc = 2.0 / (fast as f64 + 1.0);
    let slow_sc = 2.0 / (slow as f64 + 1.0);
    if seed == 0 {
        return out;
    }
    out[seed - 1] = src[seed - 1];
    for i in seed..n {
        if !src[i].is_finite() {
            out[i] = if out[i - 1].is_finite() { out[i - 1] } else { NAN };
            continue;
        }
        let direction = (src[i] - src[i - er_len]).abs();
        let mut volatility = 0.0;
        for j in (i - er_len + 1)..=i {
            if src[j].is_finite() && src[j - 1].is_finite() {
                volatility += (src[j] - src[j - 1]).abs();
            }
        }
        let er = if volatility > 1e-12 { direction / volatility } else { 0.0 };
        let mut alpha = slow_sc + er * (fast_sc - slow_sc);
        if method == AdaptiveMethod::Kama {
            alpha *= alpha;
        }
        alpha = alpha.clamp(0.0, 1.0);
        let prev = if out[i - 1].is_finite() { out[i - 1] } else { src[i] };
        out[i] = prev + alpha * (src[i] - prev);
    }
    out
}

fn diff(values: &[f64]) -> Vec<f64> {
    values
        .iter()
        .enumerate()
        .map(|(i, v)| if i == 0 || !v.is_finite() || !values[i - 1].is_finite() { NAN } else { v - values[i - 1] })
        .collect()
}

/// Sticky regime: +1 after a long signal, −1 after a short signal, 0 until the first fire.
/// `min_hold` bars must pass before another flip.
pub fn sticky_state(long_cond: &[bool], short_cond: &[bool], min_hold: usize) -> Vec<i8> {
    let n = long_cond.len();
    let mut state = vec![0i8; n];
    let mut current = 0i8;
    let mut bars_since_flip = min_hold;
    for i in 0..n {
        bars_since_flip = bars_since_flip.saturating_add(1);
        if current >= 0 && short_cond[i] && bars_since_flip >= min_hold {
            current = -1;
            bars_since_flip = 0;
        } else if current <= 0 && long_cond[i] && bars_since_flip >= min_hold {
            current = 1;
            bars_since_flip = 0;
        }
        state[i] = current;
    }
    state
}

#[derive(Clone, Copy)]
enum BandKind {
    Tp,
    Stop,
}

/// One-sided ratcheting band. Resets on a regime flip; stays put when the regime is flat.
fn ratchet_band(center: &[f64], regime: &[i8], atr: &[f64], multiple: f64, kind: BandKind) -> Vec<f64> {
    let n = center.len();
    let mut band = vec![NAN; n];
    let mut prev_regime = 0i8;
    for i in 0..n {
        let r = regime[i];
        let c = center[i];
        let a = atr[i];
        if r == 0 || !c.is_finite() || !a.is_finite() {
            if i > 0 && band[i - 1].is_finite() {
                band[i] = band[i - 1];
            }
            prev_regime = r;
            continue;
        }
        let long_tp = r == 1 && matches!(kind, BandKind::Tp);
        let short_stop = r == -1 && matches!(kind, BandKind::Stop);
        let raw = if long_tp || short_stop { c + multiple * a } else { c - multiple * a };
        let flipped = r != prev_regime;
        if i == 0 || flipped || !band[i - 1].is_finite() {
            band[i] = raw;
        } else if r == 1 {
            band[i] = band[i - 1].max(raw);
        } else {
            band[i] = band[i - 1].min(raw);
        }
        prev_regime = r;
    }
    band
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Bar;
    use std::fs;
    use std::path::PathBuf;

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9 * (1.0 + b.abs())
    }

    fn bar(i: i64, close: f64, provenance: Provenance) -> Bar {
        Bar::new(i * 86_400, close - 0.5, close + 0.5, close - 0.5, close, 1_000.0).with_provenance(provenance)
    }

    fn series(closes: &[f64], provenance: Provenance) -> Vec<Bar> {
        closes.iter().enumerate().map(|(i, c)| bar(i as i64, *c, provenance)).collect()
    }

    fn rise_then_fall(up: usize, down: usize) -> Vec<f64> {
        let mut c = Vec::with_capacity(40 + up + down);
        for i in 0..40 {
            c.push(100.0 + if i % 2 == 0 { 0.4 } else { -0.4 });
        }
        for _ in 0..up {
            c.push(*c.last().unwrap() + 1.2);
        }
        for _ in 0..down {
            c.push(*c.last().unwrap() - 1.6);
        }
        c
    }

    #[test]
    fn session_reset_emits_nan_until_warm_and_matches_fresh_series_goldens() {
        let bars = series(&rise_then_fall(100, 100), Provenance::Real);
        let o = Ohlcv::new(&bars);
        let mut starts = vec![false; bars.len()];
        starts[0] = true;
        starts[100] = true;
        let p = AdaptiveTrendParams { session_atr: true, session_reset: true, ..Default::default() };
        let reset = adaptive_trend_with(&o, p, Some(&starts));
        assert!(reset.mb[100..119].iter().all(|v| v.is_nan()));
        assert!(reset.sigma[100..139].iter().all(|v| v.is_nan()));
        assert!(reset.confidence[100..120].iter().all(|v| v.is_nan()));
        for (a, b) in [(0, 100), (100, 200)] {
            let fresh =
                adaptive_trend_with(&Ohlcv::new(&bars[a..b]), AdaptiveTrendParams { session_reset: false, ..p }, None);
            assert_eq!(
                reset.sb[a..b].iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                fresh.sb.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                "sb"
            );
            assert_eq!(
                reset.mb[a..b].iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                fresh.mb.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                "mb"
            );
            assert_eq!(
                reset.lb[a..b].iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                fresh.lb.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                "lb"
            );
            assert_eq!(
                reset.sdb[a..b].iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                fresh.sdb.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                "sdb"
            );
            assert_eq!(
                reset.mrt[a..b].iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                fresh.mrt.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                "mrt"
            );
            assert_eq!(
                reset.mdb[a..b].iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                fresh.mdb.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                "mdb"
            );
            assert_eq!(
                reset.lrt[a..b].iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                fresh.lrt.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                "lrt"
            );
            assert_eq!(
                reset.ldb[a..b].iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                fresh.ldb.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                "ldb"
            );
            assert_eq!(
                reset.atr[a..b].iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                fresh.atr.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                "atr"
            );
            assert_eq!(
                reset.confidence[a..b].iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                fresh.confidence.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                "confidence"
            );
            assert_eq!(
                reset.sigma[a..b].iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                fresh.sigma.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                "sigma"
            );
            assert_eq!(reset.short_state[a..b], fresh.short_state, "short_state");
            assert_eq!(reset.medium_state[a..b], fresh.medium_state, "medium_state");
            assert_eq!(reset.long_state[a..b], fresh.long_state, "long_state");
            assert_eq!(reset.entry_long[a..b], fresh.entry_long, "entry_long");
            assert_eq!(reset.entry_short[a..b], fresh.entry_short, "entry_short");
        }
    }
    #[test]
    fn session_reset_off_matches_existing_goldens() {
        let bars = series(&rise_then_fall(80, 80), Provenance::Real);
        let o = Ohlcv::new(&bars);
        let mut starts = vec![false; bars.len()];
        starts[80] = true;
        let p = AdaptiveTrendParams { session_reset: false, ..Default::default() };
        let with = adaptive_trend_with(&o, p, Some(&starts));
        assert_eq!(format!("{with:?}"), format!("{:?}", adaptive_trend(&o, p)));
    }

    #[test]
    fn adaptive_trend_without_session_starts_matches_existing_goldens() {
        let o = Ohlcv::new(&series(&rise_then_fall(80, 80), Provenance::Real));
        let a = adaptive_trend(&o, Default::default());
        let b = adaptive_trend_with(&o, Default::default(), None);
        assert_eq!(format!("{a:?}"), format!("{b:?}"));
    }
    #[test]
    fn session_atr_ignores_the_overnight_gap() {
        let bars = series(&[100.0, 100.0, 105.0, 105.0], Provenance::Real);
        let o = Ohlcv::new(&bars);
        let starts = [true, false, true, false];
        assert_eq!(atr_session(&o, 2, &starts)[2], 1.0);
        assert!(atr(&o, 2)[2] > atr_session(&o, 2, &starts)[2]);
        assert!(session_starts(&o.ts).iter().all(|s| *s));
        let closes = rise_then_fall(38, 0);
        let last = *closes.last().unwrap();
        let mut sessions = series(&closes, Provenance::Real);
        for (i, b) in sessions.iter_mut().enumerate() {
            b.ts = 1_793_629_800 + i as i64 * 300;
        }
        let gap = sessions.len();
        sessions.push(
            Bar::new(1_793_716_200, last * 1.05, last * 1.05 + 0.5, last * 1.05 - 0.5, last * 1.05, 1000.0)
                .with_provenance(Provenance::Real),
        );
        let o = Ohlcv::new(&sessions);
        let plain = adaptive_trend(&o, Default::default());
        let session = adaptive_trend_with(
            &o,
            AdaptiveTrendParams { session_atr: true, ..Default::default() },
            Some(&session_starts(&o.ts)),
        );
        assert_eq!(session.medium_state[gap], 1);
        assert!(session.mrt[gap] > plain.mrt[gap], "session gap-open MRT should be tighter");
    }
    #[test]
    fn confidence_is_zero_when_flat_nan_in_warmup_and_bounded() {
        let o = Ohlcv::new(&series(&rise_then_fall(80, 80), Provenance::Real));
        let t = adaptive_trend(&o, Default::default());
        assert!(t.confidence[..20].iter().all(|v| v.is_nan()));
        for (c, state) in t.confidence.iter().zip(t.medium_state) {
            if c.is_finite() {
                assert!((0.0..=1.0).contains(c));
                if state == 0 {
                    assert_eq!(*c, 0.0);
                }
            }
        }
    }
    #[test]
    fn sigma_is_nan_until_its_window_fills_and_never_negative() {
        let o = Ohlcv::new(&series(&rise_then_fall(80, 80), Provenance::Real));
        let t = adaptive_trend(&o, Default::default());
        assert!(t.sigma[..58].iter().all(|v| v.is_nan()));
        assert!(t.sigma[58..].iter().all(|v| v.is_finite() && *v >= 0.0));
    }

    #[test]
    fn warmup_stays_nan_until_the_longest_window() {
        let o = Ohlcv::new(&series(&rise_then_fall(80, 20), Provenance::Real));
        let t = adaptive_trend(&o, AdaptiveTrendParams::default());
        // SB seeds at er=10 (index 9); MB at 19; LB at 39; ATR at 19.
        assert!(t.sb[8].is_nan() && t.sb[9].is_finite());
        assert!(t.mb[18].is_nan() && t.mb[19].is_finite());
        assert!(t.lb[38].is_nan() && t.lb[39].is_finite());
        assert!(t.atr[18].is_nan() && t.atr[19].is_finite());
        assert!(t.entry_long[0] == false && t.entry_short[0] == false);
        assert_eq!(t.short_state[0], 0);
    }

    #[test]
    fn rising_series_goes_long_then_falling_flips_short() {
        let o = Ohlcv::new(&series(&rise_then_fall(80, 80), Provenance::Real));
        let t = adaptive_trend(&o, AdaptiveTrendParams::default());
        let last_up = t.medium_state.iter().rposition(|&s| s == 1).expect("medium-long stretch");
        assert_eq!(t.medium_state[last_up], 1, "advance should be medium-long");
        assert_eq!(t.short_state[last_up].abs(), 1, "short horizon should have fired by then");
        assert!(t.entry_long.iter().any(|&e| e), "a first long flip should fire after warm-up");
        assert!(!t.entry_long[0], "the first bar has no prior context");
        let last = t.medium_state.len() - 1;
        assert_eq!(t.medium_state[last], -1, "the decline should flip medium-short");
        assert!(t.entry_short.iter().any(|&e| e));
        // Guides are checked on the flip bars: the last long bar of a dump can already
        // be through the ratcheted MRT, which is the stop firing, not a construction bug.
        let long_i = t.entry_long.iter().position(|&e| e).expect("long flip");
        let short_i = t.entry_short.iter().position(|&e| e).expect("short flip");
        assert!(
            t.mrt[long_i] < o.close[long_i],
            "long trailing stop sits below price on the flip ({:.4} vs {:.4})",
            t.mrt[long_i],
            o.close[long_i]
        );
        assert!(
            t.mrt[short_i] > o.close[short_i],
            "short trailing stop sits above price on the flip ({:.4} vs {:.4})",
            t.mrt[short_i],
            o.close[short_i]
        );
    }

    #[test]
    fn trailing_stop_only_ratchets_in_the_profit_direction() {
        let o = Ohlcv::new(&series(&rise_then_fall(90, 10), Provenance::Real));
        let t = adaptive_trend(&o, AdaptiveTrendParams::default());
        let mut last = f64::NAN;
        for i in 1..t.mrt.len() {
            if t.medium_state[i] != 1 || !t.mrt[i].is_finite() {
                continue;
            }
            if last.is_finite() {
                assert!(t.mrt[i] + 1e-12 >= last, "long MRT must not drop: {} < {last}", t.mrt[i]);
            }
            last = t.mrt[i];
        }
        assert!(last.is_finite(), "expected a long stretch with a stop");
    }

    #[test]
    fn sticky_state_holds_until_the_other_side_and_respects_min_hold() {
        let long = [true, false, false, false, true];
        let short = [false, false, true, false, false];
        let s = sticky_state(&long, &short, 5);
        assert_eq!(s, vec![1, 1, 1, 1, 1], "min_hold blocks the early short");
        let s = sticky_state(&long, &short, 1);
        assert_eq!(s, vec![1, 1, -1, -1, 1]);
        assert_eq!(sticky_state(&[false, false], &[false, false], 5), vec![0, 0]);
    }

    #[test]
    fn adma_is_more_responsive_than_kama_on_the_same_window() {
        let src: Vec<f64> = (0..40).map(|i| 10.0 + i as f64).collect();
        let k = adaptive_ma(&src, 10, 2, 30, AdaptiveMethod::Kama);
        let a = adaptive_ma(&src, 10, 2, 30, AdaptiveMethod::Adma);
        assert!(k[9].is_finite() && a[9].is_finite());
        // Linear alpha > squared alpha when ER < 1, so ADMA tracks a ramp more tightly.
        assert!((src[39] - a[39]).abs() < (src[39] - k[39]).abs());
    }

    #[test]
    fn research_gate_refuses_sample_unverified_and_empty() {
        let real = series(&rise_then_fall(60, 0), Provenance::Real);
        assert!(adaptive_trend_research(&real, AdaptiveTrendParams::default()).is_ok());
        let cached = series(&rise_then_fall(60, 0), Provenance::Cached);
        assert!(adaptive_trend_research(&cached, AdaptiveTrendParams::default()).is_ok());
        let sample = series(&rise_then_fall(60, 0), Provenance::Sample);
        let err = adaptive_trend_research(&sample, AdaptiveTrendParams::default()).unwrap_err();
        assert!(err.contains("SAMPLE"), "{err}");
        assert!(err.contains("never drive"), "{err}");
        let missing = series(&rise_then_fall(60, 0), Provenance::Missing);
        assert!(adaptive_trend_research(&missing, AdaptiveTrendParams::default()).unwrap_err().contains("UNVERIFIED"));
        assert!(adaptive_trend_research(&[], AdaptiveTrendParams::default()).unwrap_err().contains("no bars"));
        let mixed = {
            let mut v = real;
            v[3] = bar(3, 103.0, Provenance::Sample);
            v
        };
        assert!(adaptive_trend_research(&mixed, AdaptiveTrendParams::default()).unwrap_err().contains("SAMPLE"));
    }

    #[test]
    fn hud_names_states_and_stops() {
        let o = Ohlcv::new(&series(&rise_then_fall(80, 0), Provenance::Real));
        let t = adaptive_trend(&o, AdaptiveTrendParams::default());
        let hud = t.hud();
        assert!(hud.contains("AT  S"), "{hud}");
        assert!(hud.contains("medium manages the trade"), "{hud}");
    }

    #[derive(serde::Deserialize)]
    struct Golden {
        comment: String,
        closes: Vec<f64>,
        first_sb: usize,
        first_mb: usize,
        first_lb: usize,
        first_atr: usize,
        long_at: usize,
        short_at: usize,
        mrt_long: f64,
        mrt_short: f64,
        sb_long: f64,
        mb_long: f64,
    }

    fn golden_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/adaptive_trend_golden.json")
    }

    #[test]
    fn golden_vectors_match_wn_initialization_flip_and_stop() {
        let raw = fs::read_to_string(golden_path()).expect("adaptive_trend_golden.json");
        let g: Golden = serde_json::from_str(&raw).expect("golden json");
        assert!(g.comment.contains("Whats-News"), "{}", g.comment);
        let o = Ohlcv::new(&series(&g.closes, Provenance::Real));
        let t = adaptive_trend(&o, AdaptiveTrendParams::default());
        assert!(t.sb[g.first_sb.saturating_sub(1)].is_nan());
        assert!(t.sb[g.first_sb].is_finite());
        assert!(t.mb[g.first_mb].is_finite());
        assert!(t.lb[g.first_lb].is_finite());
        assert!(t.atr[g.first_atr].is_finite());
        assert_eq!(t.medium_state[g.long_at], 1);
        assert_eq!(t.medium_state[g.short_at], -1);
        assert!(near(t.mrt[g.long_at], g.mrt_long), "mrt long {} vs {}", t.mrt[g.long_at], g.mrt_long);
        assert!(near(t.mrt[g.short_at], g.mrt_short), "mrt short {} vs {}", t.mrt[g.short_at], g.mrt_short);
        assert!(near(t.sb[g.long_at], g.sb_long), "sb {} vs {}", t.sb[g.long_at], g.sb_long);
        assert!(near(t.mb[g.long_at], g.mb_long), "mb {} vs {}", t.mb[g.long_at], g.mb_long);
    }
}
