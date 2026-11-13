//! Distribution Studio: verified close-to-close log returns, descriptive tails and regimes.
//! No data fetching, imputation or random simulation. See docs/STUDIO_DIST.md for conventions.
use chrono::{DateTime, Months, Utc};
use serde::{Deserialize, Serialize};
use tr_core::{AssetClass, Bar, Provenance, Timeframe};

pub const BINS: usize = 40;
pub const VOL_WINDOW: usize = 20;
pub const EXTREMES: usize = 10;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Window {
    OneYear,
    ThreeYears,
    FiveYears,
    #[default]
    All,
}
impl Window {
    pub const ALL: [Self; 4] = [Self::OneYear, Self::ThreeYears, Self::FiveYears, Self::All];
    pub fn label(self) -> &'static str {
        match self {
            Self::OneYear => "1Y",
            Self::ThreeYears => "3Y",
            Self::FiveYears => "5Y",
            Self::All => "ALL",
        }
    }
    fn months(self) -> Option<u32> {
        match self {
            Self::OneYear => Some(12),
            Self::ThreeYears => Some(36),
            Self::FiveYears => Some(60),
            Self::All => None,
        }
    }
}

/// Weekly/monthly inputs are intentionally excluded: the studio models bar returns.
pub fn parse_timeframe(s: &str) -> Result<Timeframe, String> {
    let tf = if s.trim().is_empty() { Timeframe::D1 } else { s.parse::<Timeframe>()? };
    if tf.is_intraday() || tf == Timeframe::D1 {
        Ok(tf)
    } else {
        Err("DIST supports 1D, 1m, 5m, 15m, 30m, 1H or 4H bars.".into())
    }
}

/// Cash uses actual nominal buckets per session (including the short final bucket).
/// Crypto uses 365 continuous days; other intraday assets use 252 continuous days.
pub fn periods_per_year(tf: Timeframe, class: AssetClass) -> f64 {
    if tf == Timeframe::D1 {
        return if class == AssetClass::Crypto { 365.0 } else { 252.0 };
    }
    if class == AssetClass::Crypto {
        365.0 * 86_400.0 / tf.seconds() as f64
    } else if matches!(class, AssetClass::Equity | AssetClass::Etf | AssetClass::Index) {
        252.0 * (23_400.0 / tf.seconds() as f64).ceil()
    } else {
        252.0 * 86_400.0 / tf.seconds() as f64
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Moments {
    pub n: usize,
    pub mean: f64,
    pub sigma: f64,
    pub annual_vol: f64,
    pub skew: Option<f64>,
    pub excess_kurtosis: Option<f64>,
    pub jarque_bera: Option<f64>,
    pub jb_p_value: Option<f64>,
    pub var95: f64,
    pub cvar95: f64,
    pub var99: f64,
    pub cvar99: f64,
    pub tail_ratio: Option<f64>,
    /// Shares below mean-kσ and above mean+kσ, using population σ.
    pub beyond2: Option<[f64; 2]>,
    pub beyond3: Option<[f64; 2]>,
    pub student_df: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistogramBin {
    pub left: f64,
    pub right: f64,
    pub count: usize,
}
#[derive(Debug, Clone, PartialEq)]
pub struct ReturnPoint {
    pub ts: i64,
    pub value: f64,
}
#[derive(Debug, Clone, PartialEq)]
pub struct QqPoint {
    pub theoretical: f64,
    pub observed: f64,
    pub tail: bool,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Drawdown {
    pub peak: i64,
    pub trough: i64,
    pub recovery: Option<i64>,
    /// Negative simple price drawdown, not a log return.
    pub depth: f64,
    pub duration_bars: usize,
    pub duration_days: f64,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Underwater {
    pub ts: i64,
    pub depth: f64,
    pub rolling_vol: Option<f64>,
    /// Lagged 20-return volatility; None until a full preceding window exists.
    pub regime: Option<usize>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct RegimeStats {
    pub n: usize,
    pub mean: Option<f64>,
    pub annual_vol: Option<f64>,
    pub hit_rate: Option<f64>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub symbol: String,
    pub source: String,
    pub provenance: Provenance,
    pub timeframe: Timeframe,
    pub window: Window,
    pub start: i64,
    pub end: i64,
    pub observed_at: Option<i64>,
    pub bars: usize,
    pub annual_periods: f64,
    pub moments: Moments,
    pub returns: Vec<ReturnPoint>,
    pub histogram: Vec<HistogramBin>,
    /// (return, expected histogram count) evaluated on a shared smooth grid.
    pub normal_curve: Vec<(f64, f64)>,
    pub student_curve: Vec<(f64, f64)>,
    pub qq: Vec<QqPoint>,
    pub best: Vec<ReturnPoint>,
    pub worst: Vec<ReturnPoint>,
    pub underwater: Vec<Underwater>,
    pub drawdowns: Vec<Drawdown>,
    pub vol_terciles: Option<[f64; 2]>,
    pub regimes: [RegimeStats; 3],
    pub transitions: [[usize; 3]; 3],
}

/// Input must already be a closed-bar prefix. Gate the entire input before selecting a window.
pub fn compute(
    symbol: &str,
    bars: &[Bar],
    tf: Timeframe,
    window: Window,
    source: &str,
    observed_at: Option<i64>,
) -> Result<Report, String> {
    crate::stats::require_research_bars(bars, "Distribution Studio")?;
    parse_timeframe(tf.label())?;
    if bars.iter().any(|b| DateTime::<Utc>::from_timestamp(b.ts, 0).is_none()) {
        return Err("Distribution Studio refused: invalid timestamp.".into());
    }
    let end = bars.last().unwrap().ts;
    let cutoff = window
        .months()
        .and_then(|m| DateTime::<Utc>::from_timestamp(end, 0)?.checked_sub_months(Months::new(m)))
        .map(|d| d.timestamp());
    let first = cutoff.map(|c| bars.partition_point(|b| b.ts < c)).unwrap_or(0);
    let bars = &bars[first..];
    if bars.len() < 5 {
        return Err("Distribution Studio needs at least 5 verified closed bars (4 log returns) in this window.".into());
    }
    let returns: Vec<_> =
        bars.windows(2).map(|w| ReturnPoint { ts: w[1].ts, value: w[1].close.ln() - w[0].close.ln() }).collect();
    let values: Vec<_> = returns.iter().map(|r| r.value).collect();
    let sec = crate::security(symbol);
    let annual = periods_per_year(tf, sec.class);
    let moments = describe(&values, annual)?;
    let mut sorted = values.clone();
    sorted.sort_by(f64::total_cmp);
    let low = sorted[0];
    let high = sorted[sorted.len() - 1];
    let (low, high) = if high > low { (low, high) } else { (low - 0.001, high + 0.001) };
    let width = (high - low) / BINS as f64;
    let mut histogram: Vec<_> = (0..BINS)
        .map(|i| HistogramBin { left: low + i as f64 * width, right: low + (i + 1) as f64 * width, count: 0 })
        .collect();
    for x in &values {
        let i = (((x - low) / width).floor() as usize).min(BINS - 1);
        histogram[i].count += 1;
    }
    let mut normal_curve = Vec::new();
    let mut student_curve = Vec::new();
    // sigma is population σ, matching the fitted moments and standardized QQ coordinates.
    let sigma = population_sigma(&values, moments.mean);
    if sigma > 0.0 {
        for i in 0..=200 {
            let x = low + (high - low) * i as f64 / 200.0;
            let z = (x - moments.mean) / sigma;
            let scale = values.len() as f64 * width;
            normal_curve.push((x, (-0.5 * z * z).exp() / (sigma * (2.0 * std::f64::consts::PI).sqrt()) * scale));
            if let Some(df) = moments.student_df {
                let t_scale = sigma * ((df - 2.0) / df).sqrt();
                student_curve.push((x, student_density((x - moments.mean) / t_scale, df) / t_scale * scale));
            }
        }
    }
    let qq = if sigma > 0.0 {
        sorted
            .iter()
            .enumerate()
            .map(|(i, x)| {
                let p = (i as f64 + 0.5) / sorted.len() as f64;
                QqPoint {
                    theoretical: normal_quantile(p),
                    observed: (x - moments.mean) / sigma,
                    tail: !(0.025..=0.975).contains(&p),
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    let mut worst = returns.clone();
    worst.sort_by(|a, b| a.value.total_cmp(&b.value).then(a.ts.cmp(&b.ts)));
    let best = worst.iter().rev().take(EXTREMES).cloned().collect();
    worst.truncate(EXTREMES);
    let mut vol = vec![None; returns.len()];
    for i in VOL_WINDOW - 1..returns.len() {
        let xs = &values[i + 1 - VOL_WINDOW..=i];
        let mean = xs.iter().sum::<f64>() / VOL_WINDOW as f64;
        vol[i] = Some(population_sigma(xs, mean) * (VOL_WINDOW as f64 / (VOL_WINDOW - 1) as f64 * annual).sqrt());
    }
    let mut vol_sorted: Vec<_> = vol.iter().flatten().copied().collect();
    vol_sorted.sort_by(f64::total_cmp);
    let terciles =
        (!vol_sorted.is_empty()).then(|| [quantile(&vol_sorted, 1.0 / 3.0), quantile(&vol_sorted, 2.0 / 3.0)]);
    let mut regime_values: [Vec<f64>; 3] = std::array::from_fn(|_| Vec::new());
    let mut transitions = [[0; 3]; 3];
    let mut previous: Option<usize> = None;
    let mut underwater = vec![Underwater { ts: bars[0].ts, depth: 0.0, rolling_vol: None, regime: None }];
    let mut peak = 0;
    let mut trough = 0;
    let mut drawdowns = Vec::new();
    for i in 1..bars.len() {
        let r = i - 1;
        let regime = r.checked_sub(1).and_then(|p| vol[p]).zip(terciles).map(|(v, [a, b])| {
            if a == b {
                1
            } else if v <= a {
                0
            } else if v <= b {
                1
            } else {
                2
            }
        });
        if let Some(k) = regime {
            regime_values[k].push(values[r]);
            if let Some(p) = previous {
                transitions[p][k] += 1;
            }
        }
        previous = regime;
        if bars[i].close >= bars[peak].close {
            if trough > peak {
                drawdowns.push(episode(bars, peak, trough, Some(i)));
            }
            peak = i;
            trough = i;
        } else if bars[i].close < bars[trough].close {
            trough = i;
        }
        underwater.push(Underwater {
            ts: bars[i].ts,
            depth: bars[i].close / bars[peak].close - 1.0,
            rolling_vol: vol[r],
            regime,
        });
    }
    if trough > peak {
        drawdowns.push(episode(bars, peak, trough, None));
    }
    drawdowns.sort_by(|a, b| a.depth.total_cmp(&b.depth).then(a.peak.cmp(&b.peak)));
    drawdowns.truncate(5);
    let regimes = std::array::from_fn(|i| {
        let xs = &regime_values[i];
        let mean = (!xs.is_empty()).then(|| xs.iter().sum::<f64>() / xs.len() as f64);
        RegimeStats {
            n: xs.len(),
            mean,
            annual_vol: mean
                .filter(|_| xs.len() > 1)
                .map(|m| population_sigma(xs, m) * (xs.len() as f64 / (xs.len() - 1) as f64 * annual).sqrt()),
            hit_rate: mean.map(|_| xs.iter().filter(|x| **x > 0.0).count() as f64 / xs.len() as f64),
        }
    });
    Ok(Report {
        symbol: symbol.into(),
        source: source.into(),
        provenance: Bar::provenance(bars),
        timeframe: tf,
        window,
        start: bars[0].ts,
        end,
        observed_at,
        bars: bars.len(),
        annual_periods: annual,
        moments,
        returns,
        histogram,
        normal_curve,
        student_curve,
        qq,
        best,
        worst,
        underwater,
        drawdowns,
        vol_terciles: terciles,
        regimes,
        transitions,
    })
}

fn episode(bars: &[Bar], peak: usize, trough: usize, recovery: Option<usize>) -> Drawdown {
    let end = recovery.unwrap_or(bars.len() - 1);
    Drawdown {
        peak: bars[peak].ts,
        trough: bars[trough].ts,
        recovery: recovery.map(|i| bars[i].ts),
        depth: bars[trough].close / bars[peak].close - 1.0,
        duration_bars: end - peak,
        duration_days: (bars[end].ts - bars[peak].ts) as f64 / 86_400.0,
    }
}

fn population_sigma(xs: &[f64], mean: f64) -> f64 {
    (xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / xs.len() as f64).sqrt()
}

/// Linear-interpolated empirical quantile (type 7). Requires finite sorted input.
fn quantile(xs: &[f64], p: f64) -> f64 {
    let index = (xs.len() - 1) as f64 * p;
    let lo = index.floor() as usize;
    let hi = index.ceil() as usize;
    xs[lo] + (xs[hi] - xs[lo]) * index.fract()
}

/// Exact lower-tail integral of the empirical distribution, with fractional boundary weight.
fn expected_shortfall(xs: &[f64], alpha: f64) -> f64 {
    let mass = xs.len() as f64 * alpha;
    let whole = mass.floor() as usize;
    -(xs[..whole].iter().sum::<f64>() + xs[whole.min(xs.len() - 1)] * mass.fract()) / mass
}

pub fn describe(values: &[f64], annual: f64) -> Result<Moments, String> {
    if values.len() < 4 || values.iter().any(|x| !x.is_finite()) || !annual.is_finite() || annual <= 0.0 {
        return Err("Distribution Studio needs at least 4 finite returns and a positive annualisation factor.".into());
    }
    let n = values.len();
    let mean = values.iter().sum::<f64>() / n as f64;
    let sigma = population_sigma(values, mean);
    // Reject overflow rather than returning plausible-looking non-finite statistics.
    if !mean.is_finite() || !sigma.is_finite() {
        return Err("Distribution Studio refused: numeric overflow.".into());
    }
    let sample_sigma = sigma * (n as f64 / (n - 1) as f64).sqrt();
    let (skew, excess, jb, p, beyond2, beyond3) = if sigma > 0.0 {
        let zs: Vec<_> = values.iter().map(|x| (x - mean) / sigma).collect();
        let skew = zs.iter().map(|z| z.powi(3)).sum::<f64>() / n as f64;
        let excess = zs.iter().map(|z| z.powi(4)).sum::<f64>() / n as f64 - 3.0;
        let jb = n as f64 / 6.0 * (skew * skew + excess * excess / 4.0);
        let beyond = |k: f64| {
            Some([
                zs.iter().filter(|z| **z < -k).count() as f64 / n as f64,
                zs.iter().filter(|z| **z > k).count() as f64 / n as f64,
            ])
        };
        (Some(skew), Some(excess), Some(jb), Some((-jb / 2.0).exp()), beyond(2.0), beyond(3.0))
    } else {
        (None, None, None, None, None, None)
    };
    let mut xs = values.to_vec();
    xs.sort_by(f64::total_cmp);
    let denominator = quantile(&xs, 0.05).abs();
    let student_df = excess.filter(|k| *k > 0.0).map(|k| 4.0 + 6.0 / k).filter(|df| df.is_finite());
    let annual_vol = sample_sigma * annual.sqrt();
    if !annual_vol.is_finite() {
        return Err("Distribution Studio refused: annual volatility overflow.".into());
    }
    Ok(Moments {
        n,
        mean,
        sigma: sample_sigma,
        annual_vol,
        skew,
        excess_kurtosis: excess,
        jarque_bera: jb,
        jb_p_value: p,
        var95: -quantile(&xs, 0.05),
        cvar95: expected_shortfall(&xs, 0.05),
        var99: -quantile(&xs, 0.01),
        cvar99: expected_shortfall(&xs, 0.01),
        tail_ratio: (denominator > 0.0).then(|| quantile(&xs, 0.95).abs() / denominator).filter(|v| v.is_finite()),
        beyond2,
        beyond3,
        student_df,
    })
}

// Acklam's inverse-normal rational approximation. Error is below 1e-7 over plotted probabilities.
fn normal_quantile(p: f64) -> f64 {
    let a = [
        -39.69683028665376,
        220.9460984245205,
        -275.9285104469687,
        138.357_751_867_269,
        -30.66479806614716,
        2.506628277459239,
    ];
    let b = [-54.47609879822406, 161.5858368580409, -155.6989798598866, 66.80131188771972, -13.28068155288572];
    let c = [
        -0.007784894002430293,
        -0.3223964580411365,
        -2.400758277161838,
        -2.549732539343734,
        4.374664141464968,
        2.938163982698783,
    ];
    let d = [0.007784695709041462, 0.3224671290700398, 2.445134137142996, 3.754408661907416];
    if !(0.02425..=0.97575).contains(&p) {
        let q = (-2.0 * if p < 0.5 { p } else { 1.0 - p }.ln()).sqrt();
        let x = (((((c[0] * q + c[1]) * q + c[2]) * q + c[3]) * q + c[4]) * q + c[5])
            / ((((d[0] * q + d[1]) * q + d[2]) * q + d[3]) * q + 1.0);
        if p < 0.5 { x } else { -x }
    } else {
        let q = p - 0.5;
        let r = q * q;
        (((((a[0] * r + a[1]) * r + a[2]) * r + a[3]) * r + a[4]) * r + a[5]) * q
            / (((((b[0] * r + b[1]) * r + b[2]) * r + b[3]) * r + b[4]) * r + 1.0)
    }
}
fn log_gamma(z: f64) -> f64 {
    let coefficients = [
        676.5203681218851,
        -1259.1392167224028,
        771.3234287776531,
        -176.6150291621406,
        12.507343278686905,
        -0.13857109526572012,
        9.984369578019572e-6,
        1.5056327351493116e-7,
    ];
    let x = coefficients.iter().enumerate().fold(0.9999999999998099, |s, (i, c)| s + c / (z + i as f64));
    let t = z + 6.5;
    0.5 * (2.0 * std::f64::consts::PI).ln() + (z - 0.5) * t.ln() - t + x.ln()
}
fn student_density(t: f64, df: f64) -> f64 {
    if df > 1e6 {
        return (-0.5 * t * t).exp() / (2.0 * std::f64::consts::PI).sqrt();
    }
    (log_gamma((df + 1.0) / 2.0)
        - log_gamma(df / 2.0)
        - 0.5 * (df * std::f64::consts::PI).ln()
        - (df + 1.0) / 2.0 * (t * t / df).ln_1p())
    .exp()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }
    fn bars(prices: &[f64]) -> Vec<Bar> {
        prices
            .iter()
            .enumerate()
            .map(|(i, p)| {
                Bar::new(1_600_000_000 + i as i64 * 86_400, *p, *p, *p, *p, 1.0).with_provenance(Provenance::Real)
            })
            .collect()
    }
    fn report(bs: &[Bar]) -> Result<Report, String> {
        compute("AAPL US Equity", bs, Timeframe::D1, Window::All, "unit", None)
    }
    #[test]
    fn known_moments_jb_and_tails() {
        let m = describe(&[-0.02, -0.01, 0.0, 0.01, 0.02], 252.0).unwrap();
        near(m.mean, 0.0);
        near(m.sigma, 0.00025_f64.sqrt());
        near(m.annual_vol, 0.063_f64.sqrt());
        near(m.skew.unwrap(), 0.0);
        near(m.excess_kurtosis.unwrap(), -1.3);
        near(m.jarque_bera.unwrap(), 5.0 / 24.0 * 1.69);
        near(m.jb_p_value.unwrap(), (-5.0_f64 / 48.0 * 1.69).exp());
        near(m.var95, 0.018);
        near(m.var99, 0.0196);
        near(m.cvar95, 0.02);
        near(m.cvar99, 0.02);
        near(m.tail_ratio.unwrap(), 1.0);
        assert!(m.student_df.is_none());
    }
    #[test]
    fn empirical_shortfall_fractional_boundary() {
        near(expected_shortfall(&[-4.0, -2.0, 1.0, 3.0], 0.375), 10.0 / 3.0);
        near(quantile(&[1.0, 2.0, 3.0, 4.0], 0.25), 1.75);
    }
    #[test]
    fn zero_variance_is_honest() {
        let r = report(&bars(&[10.0; 30])).unwrap();
        assert_eq!(r.moments.sigma, 0.0);
        assert_eq!(r.moments.skew, None);
        assert_eq!(r.moments.jb_p_value, None);
        assert!(r.qq.is_empty() && r.normal_curve.is_empty() && r.student_curve.is_empty());
        assert_eq!(r.moments.tail_ratio, None);
        assert_eq!(r.moments.beyond2, None);
        assert!(r.drawdowns.is_empty());
        assert_eq!(r.regimes[1].n, 9);
        assert_eq!(r.transitions[1][1], 8);
    }
    #[test]
    fn rejects_short_nan_overflow_and_bad_period() {
        assert!(report(&bars(&[1.0; 4])).is_err());
        assert!(describe(&[f64::NAN; 4], 252.0).is_err());
        assert!(describe(&[f64::MAX; 4], 252.0).is_err());
        assert!(describe(&[0.0; 4], 0.0).is_err());
        let mut bs = bars(&[1.0; 5]);
        bs[3].close = f64::NAN;
        assert!(report(&bs).is_err());
        bs[3].close = 0.0;
        assert!(report(&bs).is_err());
    }
    #[test]
    fn refuses_sample_missing_mixed_and_disordered_timestamps() {
        for p in [Provenance::Sample, Provenance::Missing] {
            let mut bs = bars(&[1.0; 6]);
            bs[3].provenance = p;
            assert!(report(&bs).is_err());
        }
        let mut bs = bars(&[1.0; 6]);
        bs[3].ts = bs[2].ts;
        assert!(report(&bs).is_err());
        bs[3].ts = bs[2].ts - 1;
        assert!(report(&bs).is_err());
        bs[3].ts = i64::MAX;
        assert!(report(&bs).is_err());
    }
    #[test]
    fn log_returns_and_drawdown_recovery_known_answer() {
        let bs = bars(&[100.0, 80.0, 90.0, 100.0, 120.0, 60.0]);
        let r = report(&bs).unwrap();
        near(r.returns[0].value, 0.8_f64.ln());
        assert_eq!(r.drawdowns.len(), 2);
        near(r.drawdowns[0].depth, -0.5);
        assert_eq!(r.drawdowns[0].recovery, None);
        assert_eq!(r.drawdowns[0].duration_bars, 1);
        near(r.drawdowns[1].depth, -0.2);
        assert_eq!(r.drawdowns[1].recovery, Some(bs[3].ts));
        assert_eq!(r.drawdowns[1].duration_bars, 3);
        near(r.drawdowns[1].duration_days, 3.0);
        assert_eq!(r.best[0].ts, bs[4].ts);
        assert_eq!(r.worst[0].ts, bs[5].ts);
        assert_eq!(r.histogram.iter().map(|b| b.count).sum::<usize>(), 5);
        assert_eq!(r.qq.len(), 5);
        assert!(r.regimes.iter().all(|s| s.n == 0));
    }
    #[test]
    fn window_uses_calendar_time_and_gates_excluded_data() {
        let bs = bars(&vec![10.0; 2200]);
        let r = compute("AAPL", &bs, Timeframe::D1, Window::OneYear, "unit", None).unwrap();
        assert!((366..=367).contains(&r.bars));
        let mut bad = bs;
        bad[0].provenance = Provenance::Sample;
        assert!(compute("AAPL", &bad, Timeframe::D1, Window::OneYear, "unit", None).is_err());
    }
    #[test]
    fn regimes_use_preceding_window_and_account_for_all_transitions() {
        let prices: Vec<_> =
            (0..100).map(|i| (5.0 + (i as f64 * 0.9).sin() * (i / 25 + 1) as f64 * 0.01).exp()).collect();
        let r = report(&bars(&prices)).unwrap();
        assert!(r.underwater[..=20].iter().all(|p| p.regime.is_none()));
        assert!(r.underwater[21].regime.is_some());
        assert_eq!(r.regimes.iter().map(|s| s.n).sum::<usize>(), 79);
        assert_eq!(r.transitions.iter().flatten().sum::<usize>(), 78);
        assert!(r.regimes.iter().all(|s| s.n > 0));
        let xs: Vec<_> = r.returns[..20].iter().map(|p| p.value).collect();
        near(r.underwater[20].rolling_vol.unwrap(), describe(&xs, 252.0).unwrap().annual_vol);
    }
    #[test]
    fn overlays_and_normal_quantiles_known_answer() {
        near(normal_quantile(0.5), 0.0);
        assert!((normal_quantile(0.975) - 1.95996398454).abs() < 1e-7);
        assert!((normal_quantile(0.001) + 3.09023230617).abs() < 1e-7);
        near(student_density(0.0, 1.0), 1.0 / std::f64::consts::PI);
        let mut xs = vec![0.0; 100];
        xs[0] = -1.0;
        xs[99] = 1.0;
        let m = describe(&xs, 252.0).unwrap();
        near(m.student_df.unwrap(), 4.0 + 6.0 / 47.0);
        near(m.beyond3.unwrap().iter().sum(), 0.02);
        let prices: Vec<_> = xs
            .iter()
            .scan(5.0, |s, x| {
                *s += x;
                Some(s.exp())
            })
            .collect();
        let r = report(&bars(&prices)).unwrap();
        assert_eq!(r.normal_curve.len(), 201);
        assert_eq!(r.student_curve.len(), 201);
        assert!(r.student_curve.iter().all(|(_, y)| y.is_finite() && *y >= 0.0));
        assert!(r.qq.iter().any(|p| p.tail));
    }
    #[test]
    fn timeframe_and_annualisation_conventions() {
        assert_eq!(parse_timeframe("").unwrap(), Timeframe::D1);
        assert_eq!(parse_timeframe("1H").unwrap(), Timeframe::H1);
        assert!(parse_timeframe("1W").is_err());
        assert!(parse_timeframe("no").is_err());
        near(periods_per_year(Timeframe::H1, AssetClass::Equity), 1764.0);
        near(periods_per_year(Timeframe::M5, AssetClass::Crypto), 105120.0);
    }
}
