//! Whats-News Adaptive Trend: multi-horizon adaptive baselines, sticky regimes,
//! destination bands and ratcheting trailing stops.
//!
//! Conceptual port of Whats-News `adaptive_trend.py`. The functions here are
//! provenance-agnostic. Research overlays that claim live archive analysis must
//! call [`adaptive_trend_research`] so sample, unverified or empty series are
//! refused — never a silent fallback. Python is not vendored into the runtime.

use super::volatility::atr;
use super::{NAN, Ohlcv};
use crate::{Bar, Provenance};

/// Kaufman (squared) or ADMA (linear) efficiency-ratio smoothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, Copy, PartialEq)]
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
        }
    }
}

impl AdaptiveTrendParams {
    /// Registry parameters: ATR length, confirm multiple, method (0 = KAMA, 1 = ADMA).
    pub fn from_registry(p: &[f64]) -> Self {
        let mut s = Self::default();
        if let Some(&n) = p.first() {
            s.atr_n = (n as usize).max(1);
        }
        if let Some(&c) = p.get(1) {
            s.confirm_mult = c;
        }
        if let Some(&m) = p.get(2) {
            s.method = AdaptiveMethod::from_param(m);
        }
        s
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
    let len = o.len();
    if len == 0 {
        return AdaptiveTrend::empty(0);
    }
    let hlc3 = o.hlc3();
    let sb = adaptive_ma(&hlc3, p.sb_er, p.sb_fast, p.sb_slow, p.method);
    let mb = adaptive_ma(&hlc3, p.mb_er, p.mb_fast, p.mb_slow, p.method);
    let lb = adaptive_ma(&hlc3, p.lb_er, p.lb_fast, p.lb_slow, p.method);
    let a = atr(o, p.atr_n);

    let sb_slope = diff(&sb);
    let mb_slope = diff(&mb);
    let lb_slope = diff(&lb);

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
        sh_long[i] = c > sb[i] + 0.5 * confirm && sb_slope[i] > 0.0;
        sh_short[i] = c < sb[i] - 0.5 * confirm && sb_slope[i] < 0.0;
        med_long[i] = sb[i] > mb[i] + confirm && mb_slope[i] > 0.0 && c > mb[i];
        med_short[i] = sb[i] < mb[i] - confirm && mb_slope[i] < 0.0 && c < mb[i];
        lng_long[i] = mb[i] > lb[i] + confirm && lb_slope[i] > 0.0;
        lng_short[i] = mb[i] < lb[i] - confirm && lb_slope[i] < 0.0;
    }

    let short_state = sticky_state(&sh_long, &sh_short, p.min_hold);
    let medium_state = sticky_state(&med_long, &med_short, p.min_hold);
    let long_state = sticky_state(&lng_long, &lng_short, p.min_hold);

    let sdb = ratchet_band(&sb, &medium_state, &a, 2.0, BandKind::Tp);
    let mrt = ratchet_band(&mb, &medium_state, &a, 2.25, BandKind::Stop);
    let mdb = ratchet_band(&mb, &medium_state, &a, 4.5, BandKind::Tp);
    let lrt = ratchet_band(&lb, &long_state, &a, 2.25, BandKind::Stop);
    let ldb = ratchet_band(&lb, &long_state, &a, 4.5, BandKind::Tp);

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

    AdaptiveTrend {
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
