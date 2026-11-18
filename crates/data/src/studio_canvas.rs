//! Quick canvas arithmetic. All admission, transforms, sorting and event alignment
//! run on workers. No interpolation, carried values, or substituted peers.
use crate::{
    stable_hash::{Fnv64, bars_digest},
    studio::{ScalarSeries, SeriesReceipt, SeriesUnit},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};
use tr_core::{AssetClass, Bar, Timeframe};

pub const MAX_BARS: usize = 120_000;
pub const MAX_EVENTS: usize = 2_000;
pub const BINS: usize = 32;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CompareMode {
    #[default]
    Level,
    Index100,
    Change,
    Z,
    Vol,
}
impl CompareMode {
    pub const ALL: [Self; 5] = [Self::Level, Self::Index100, Self::Change, Self::Z, Self::Vol];
    pub fn code(self) -> &'static str {
        match self {
            Self::Level => "level",
            Self::Index100 => "index100",
            Self::Change => "change",
            Self::Z => "z",
            Self::Vol => "vol",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.code().eq_ignore_ascii_case(s))
    }
    pub fn formula(self) -> &'static str {
        match self {
            Self::Level => "x[t] = P[t]",
            Self::Index100 => "x[t] = 100 × P[t] / P[base]",
            Self::Change => "x[t] = P[t] − P[base] (native units)",
            Self::Z => "x[t] = (P[t] − mean(P[t−w..t])) / sample_sd(P[t−w..t])",
            Self::Vol => "x[t] = 10⁴ × sample_sd(log(P[j]/P[j−1]), j=t−w..t−1)",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CanvasSpec {
    pub mode: CompareMode,
    pub base_ts: i64,
    pub window: usize,
    pub strip: bool,
}
impl Default for CanvasSpec {
    fn default() -> Self {
        Self { mode: CompareMode::Level, base_ts: 0, window: 63, strip: true }
    }
}
#[derive(Clone)]
pub struct CanvasInput {
    pub symbol: String,
    pub tf: Timeframe,
    pub bars: Arc<Vec<Bar>>,
    pub source: String,
    pub observed_at: Option<i64>,
    pub stitch: Option<Arc<crate::vault::StitchInfo>>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventTime {
    Instant(i64),
    Date { start: chrono::NaiveDate, end: chrono::NaiveDate },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    News,
    Earnings,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanvasEvent {
    pub kind: EventKind,
    pub time: EventTime,
    pub source: String,
    pub headline: String,
}
#[derive(Debug, Clone)]
pub struct AlignedEvent {
    pub event: CanvasEvent,
    pub lo: usize,
    pub hi: usize,
}
#[derive(Debug, Clone)]
pub struct MarginDistribution {
    pub counts: Vec<usize>,
    pub mass: Vec<f64>,
    pub peak: usize,
    pub lo: f64,
    pub hi: f64,
    pub n: usize,
    pub now: Option<f64>,
    pub percentile: Option<f64>,
    pub less: usize,
    pub equal: usize,
    pub span: Option<(i64, i64)>,
}
#[derive(Debug, Clone)]
pub struct CanvasLine {
    pub symbol: String,
    pub base: Option<f64>,
    pub unavailable: Option<String>,
    pub now_index: Option<usize>,
    pub now_change: Option<f64>,
    /// Substituted inputs for the last point (rolling references exclude it).
    pub prior_mean: Option<f64>,
    pub prior_sd: Option<f64>,
    pub prior_n: usize,
}
#[derive(Debug, Clone)]
pub struct CanvasReport {
    pub spec: CanvasSpec,
    pub lines: Vec<ScalarSeries>,
    pub labels: Vec<CanvasLine>,
    pub strip: Option<MarginDistribution>,
    pub events: Vec<AlignedEvent>,
    pub selected_strip: Option<MarginDistribution>,
    pub comparison_strip: Option<MarginDistribution>,
    pub selected_count: usize,
    pub visible_span: Option<(i64, i64)>,
    pub key: u64,
}
/// Native price identities are deliberately conservative where no currency metadata exists.
pub fn native_unit(symbol: &str) -> SeriesUnit {
    let s = crate::security(symbol);
    SeriesUnit::Native(
        if matches!(s.class, AssetClass::Equity | AssetClass::Etf)
            && crate::cash_session::require_us_session(&s.ticker, None).is_ok()
        {
            "USD".into()
        } else {
            format!("{} native", s.ticker)
        },
    )
}
/// Validate the entire supplied source, including forming/outside-visible input.
pub fn validate(input: &CanvasInput) -> Result<(), String> {
    if input.tf != Timeframe::D1 && !input.tf.is_intraday() {
        return Err("Quick canvas supports daily or native intraday intervals".into());
    }
    if input.source.trim().is_empty() {
        return Err("Quick canvas requires an explicit source receipt".into());
    }
    let sec = crate::security(&input.symbol);
    if crate::time_link::cash_class(sec.class) {
        crate::cash_session::require_us_session(&sec.ticker, None)?;
    }
    if input.bars.len() > MAX_BARS {
        return Err("Canvas input exceeds 120,000-bar bound".into());
    }
    let bars = crate::vault::research_bars(&input.bars, input.stitch.as_deref())?;
    crate::stats::require_research_bars(&bars, "Quick canvas")?;
    for b in bars.iter() {
        if chrono::DateTime::from_timestamp(b.ts, 0).is_none()
            || b.high < b.low
            || b.high < b.open.max(b.close)
            || b.low > b.open.min(b.close)
            || b.volume < 0.
        {
            return Err("Quick canvas refused: invalid OHLCV/timestamp".into());
        }
    }
    if input.observed_at.is_some_and(|t| chrono::DateTime::from_timestamp(t, 0).is_none()) {
        return Err("Invalid canvas observation clock".into());
    }
    Ok(())
}
/// Validates before key creation/cache lookup. Includes full corrections, clocks,
/// provenance, witnesses, methods, events, visible span and committed selection.
pub fn input_key(
    inputs: &[CanvasInput],
    spec: &CanvasSpec,
    visible: Option<(i64, i64)>,
    selected: Option<(i64, i64)>,
    events: &[CanvasEvent],
) -> Result<u64, String> {
    if inputs.is_empty() || inputs.len() > 5 {
        return Err("Canvas needs one asset and at most four comparisons".into());
    }
    if events.len() > MAX_EVENTS {
        return Err("Canvas event bound exceeded".into());
    }
    if !(2..=1000).contains(&spec.window) {
        return Err("Canvas window must be 2..1000".into());
    }
    let mut h = Fnv64::default();
    h.write_str("studio.canvas.v1");
    h.write_str(&format!("{spec:?}|{visible:?}|{selected:?}"));
    for i in inputs {
        validate(i)?;
        h.write_str(&i.symbol);
        h.write_str(i.tf.label());
        h.write_str(&i.source);
        h.write_u64(bars_digest(&i.bars));
        h.write_str(&format!("{:?}", i.observed_at));
        for b in i.bars.iter() {
            h.write_str(b.provenance.label());
        }
        if let Some(s) = &i.stitch {
            h.write_str(&format!("{}|{}|{:?}|{:?}|{:?}", s.source, s.epoch, s.fetched_at, s.gaps, s.note));
        }
    }
    for e in events {
        h.write_str(&format!("{e:?}"));
    }
    Ok(h.finish())
}
fn sample_stats(v: &[f64]) -> Option<(f64, f64)> {
    if v.len() < 2 {
        return None;
    }
    // Stable two-pass sample variance, bounded window; no cancellation of levels.
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    let sd = (v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (v.len() - 1) as f64).sqrt();
    (mean.is_finite() && sd.is_finite()).then_some((mean, sd))
}
pub(crate) fn expected_breaks(input: &CanvasInput, bars: &[Bar]) -> Vec<bool> {
    let class = crate::security(&input.symbol).class;
    let cash = crate::time_link::cash_class(class);
    let mut grid = BTreeMap::new();
    if cash && input.tf.is_intraday() {
        for b in bars {
            grid.entry(tr_core::ny::civil(b.ts).date())
                .or_insert_with(|| crate::intraday::regular_opens(tr_core::ny::civil(b.ts).date(), input.tf));
        }
    }
    let gaps = input.stitch.as_ref().map_or(&[][..], |s| s.gaps.as_slice());
    (0..bars.len())
        .map(|i| {
            if i == 0 {
                return true;
            }
            let (a, b) = (bars[i - 1].ts, bars[i].ts);
            let adjacent = if cash && input.tf == Timeframe::D1 {
                let ad = chrono::DateTime::from_timestamp(a, 0).unwrap().date_naive();
                let bd = chrono::DateTime::from_timestamp(b, 0).unwrap().date_naive();
                crate::history::calendar::next_session(ad) == bd
            } else if cash && input.tf.is_intraday() {
                let date = tr_core::ny::civil(a).date();
                let opens = &grid[&date];
                opens.binary_search(&a).ok().and_then(|k| {
                    opens.get(k + 1).copied().or_else(|| {
                        crate::intraday::regular_opens(crate::history::calendar::next_session(date), input.tf)
                            .first()
                            .copied()
                    })
                }) == Some(b)
            } else {
                b - a == input.tf.seconds()
            };
            !adjacent || crate::studio_dist::spans_gap(gaps, a, b)
        })
        .collect()
}
pub(crate) fn transform(input: &CanvasInput, spec: &CanvasSpec) -> Result<(ScalarSeries, CanvasLine), String> {
    let bars = crate::vault::research_bars(&input.bars, input.stitch.as_deref())?;
    let class = crate::security(&input.symbol).class;
    let n = crate::closed::closed_len(input.tf, &bars, class, input.observed_at);
    let bars = &bars[..n];
    let breaks = expected_breaks(input, bars);
    let base = bars.binary_search_by_key(&spec.base_ts, |b| b.ts).ok().map(|i| bars[i].close);
    let unit = match spec.mode {
        CompareMode::Index100 => SeriesUnit::Index100,
        CompareMode::Z => SeriesUnit::Z,
        CompareMode::Vol => SeriesUnit::BpPerBar,
        _ => native_unit(&input.symbol),
    };
    let rolling_supported = class == AssetClass::Crypto
        || (crate::time_link::cash_class(class)
            && (input.tf == Timeframe::D1 || crate::cash_session::cash_bar_size(input.tf)));
    let unavailable = if n == 0 {
        Some("No closed observations under the source clock".into())
    } else if matches!(spec.mode, CompareMode::Index100 | CompareMode::Change) && base.is_none() {
        Some(format!("No closed observation at pinned base {} (no snapping)", spec.base_ts))
    } else if matches!(spec.mode, CompareMode::Z | CompareMode::Vol) && !rolling_supported {
        Some("Rolling reference requires a supported cash calendar or explicitly labelled crypto UTC grid".into())
    } else {
        None
    };
    let mut values = vec![None; n];
    let mut meta = CanvasLine {
        symbol: input.symbol.clone(),
        base,
        unavailable,
        now_index: None,
        now_change: None,
        prior_mean: None,
        prior_sd: None,
        prior_n: 0,
    };
    let mut run_start = 0;
    let prices: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let mut rolling: Option<(f64, f64)> = None;
    let returns: Vec<f64> = (0..n)
        .map(|i| if i > 0 && !breaks[i] { (bars[i].close.ln() - bars[i - 1].close.ln()) as f64 } else { f64::NAN })
        .collect();
    if meta.unavailable.is_none() {
        for t in 0..n {
            if breaks[t] {
                run_start = t;
                rolling = None;
            }
            let x = bars[t].close;
            values[t] = match spec.mode {
                CompareMode::Level => Some(x),
                CompareMode::Index100 => base.map(|b| 100. * x / b),
                CompareMode::Change => base.map(|b| x - b),
                CompareMode::Z | CompareMode::Vol => {
                    let w = spec.window;
                    let enough =
                        if spec.mode == CompareMode::Vol { t >= run_start + w + 1 } else { t >= run_start + w };
                    if enough {
                        let reference = if spec.mode == CompareMode::Vol { &returns } else { &prices };
                        // Welford removal/addition with a deterministic two-pass
                        // refresh every 256 points. Same n−1 estimator, no window
                        // allocation per score; resets fully at a gap.
                        let moments = if rolling.is_none() || t % 256 == 0 {
                            sample_stats(&reference[t - w..t]).map(|(mean, sd)| (mean, sd * sd * (w - 1) as f64))
                        } else {
                            let (mean, m2) = rolling.unwrap();
                            let old = reference[t - w - 1];
                            let new = reference[t - 1];
                            let removed_mean = mean + (mean - old) / (w - 1) as f64;
                            let removed_m2 = m2 - (old - mean) * (old - removed_mean);
                            let d = new - removed_mean;
                            let next_mean = removed_mean + d / w as f64;
                            let next_m2 = removed_m2 + d * (new - next_mean);
                            (next_mean.is_finite() && next_m2.is_finite()).then_some((next_mean, next_m2.max(0.)))
                        };
                        rolling = moments;
                        moments.map(|(mean, m2)| (mean, (m2 / (w - 1) as f64).sqrt())).and_then(|(mean, sd)| {
                            if t + 1 == n {
                                meta.prior_mean = Some(mean);
                                meta.prior_sd = Some(sd);
                                meta.prior_n = w;
                            }
                            if spec.mode == CompareMode::Vol {
                                Some(sd * 1e4)
                            } else {
                                (sd > 64. * f64::EPSILON * mean.abs()).then_some((x - mean) / sd)
                            }
                        })
                    } else {
                        None
                    }
                }
            }
            .filter(|v| v.is_finite());
        }
    }
    meta.now_index = values.iter().rposition(Option::is_some);
    // Never present an earlier valid rolling point as "now".
    if n > 0 && values[n - 1].is_none() {
        meta.now_index = None;
    }
    meta.now_change = base.zip(bars.last()).map(|(b, p)| p.close - b);
    if meta.unavailable.is_none() && meta.now_index.is_none() && matches!(spec.mode, CompareMode::Z | CompareMode::Vol)
    {
        meta.unavailable = Some(format!(
            "Latest closed point needs {} prior consecutive {} and nonzero variance for Z; current point excluded",
            spec.window,
            if spec.mode == CompareMode::Vol { "one-bar log returns" } else { "levels" }
        ));
    }

    let receipt = SeriesReceipt {
        source: input.source.clone(),
        provenance: Bar::provenance(&bars),
        observed_at: input.observed_at,
        closed_thru: bars.last().map_or(0, |b| {
            if input.tf == Timeframe::D1 && crate::time_link::cash_class(class) {
                let date = chrono::DateTime::from_timestamp(b.ts, 0).unwrap().date_naive();
                let minute = crate::intraday::close_minute(date);
                tr_core::ny::to_utc(date.and_hms_opt(minute / 60, minute % 60, 0).unwrap())
            } else {
                crate::time_link::bar_end(input.tf, b.ts, crate::time_link::cash_class(class))
            }
        }),
        input_digest: bars_digest(&input.bars),
        gaps: input.stitch.as_ref().map_or_else(Vec::new, |s| s.gaps.clone()),
    };
    Ok((ScalarSeries { ts: bars.iter().map(|b| b.ts).collect(), values, break_before: breaks, unit, receipt }, meta))
}
pub fn margin_distribution(line: &ScalarSeries, span: Option<(i64, i64)>) -> MarginDistribution {
    margin_on_domain(line, span, None)
}
fn margin_on_domain(line: &ScalarSeries, span: Option<(i64, i64)>, domain: Option<(f64, f64)>) -> MarginDistribution {
    let mut v: Vec<f64> = line
        .ts
        .iter()
        .zip(&line.values)
        .filter(|(t, _)| span.is_none_or(|(a, b)| a <= **t && **t < b))
        .filter_map(|(_, v)| *v)
        .collect();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let now = line.values.last().copied().flatten();
    let less = now.map_or(0, |x| v.partition_point(|p| *p < x));
    let equal = now.map_or(0, |x| v.partition_point(|p| *p <= x).saturating_sub(less));
    let percentile = (n > 0).then_some((less as f64 + 0.5 * equal as f64) / n.max(1) as f64).filter(|_| now.is_some());
    let (lo, mut hi) = domain.unwrap_or_else(|| (v.first().copied().unwrap_or(0.), v.last().copied().unwrap_or(1.)));
    if hi == lo {
        hi = lo + lo.abs().max(1.) * 1e-6;
    }
    let mut counts = vec![0; BINS];
    for x in v {
        let k = (((x - lo) / (hi - lo) * BINS as f64) as usize).min(BINS - 1);
        counts[k] += 1;
    }
    let mass = counts.iter().map(|&c| c as f64 / n.max(1) as f64).collect();
    let peak = counts.iter().copied().max().unwrap_or(0);
    MarginDistribution { counts, mass, peak, lo, hi, n, now, percentile, less, equal, span }
}
/// Timestamp stays an instant; date-only records keep their date range, not midnight.
pub fn align_events(events: &[CanvasEvent], ts: &[i64], tf: Timeframe, cash: bool) -> Vec<AlignedEvent> {
    let mut out = Vec::new();
    for event in events.iter().take(MAX_EVENTS) {
        let range = match event.time {
            EventTime::Instant(t) => {
                let hi = ts.partition_point(|s| *s <= t);
                if hi == 0 || crate::time_link::bar_end(tf, ts[hi - 1], cash) <= t {
                    continue;
                }
                hi - 1..hi
            }
            EventTime::Date { start, end } => {
                if end < start {
                    continue;
                }
                let date = |t: i64| {
                    if cash && tf.is_intraday() {
                        tr_core::ny::civil(t).date()
                    } else {
                        chrono::DateTime::from_timestamp(t, 0).unwrap().date_naive()
                    }
                };
                ts.partition_point(|t| date(*t) < start)..ts.partition_point(|t| date(*t) <= end)
            }
        };
        if range.is_empty() {
            continue;
        }
        out.push(AlignedEvent { event: event.clone(), lo: range.start, hi: range.end });
    }
    out.sort_by_key(|e| {
        (
            match e.event.kind {
                EventKind::News => 0,
                EventKind::Earnings => 1,
            },
            e.lo,
            e.hi,
        )
    });
    out
}
pub fn compute(
    inputs: &[CanvasInput],
    spec: &CanvasSpec,
    visible: Option<(i64, i64)>,
    selected: Option<(i64, i64)>,
    events: &[CanvasEvent],
) -> Result<CanvasReport, String> {
    let key = input_key(inputs, spec, visible, selected, events)?;
    let mut lines = Vec::new();
    let mut labels = Vec::new();
    for input in inputs {
        if input.tf != inputs[0].tf {
            return Err("Canvas intervals must match".into());
        }
        let (l, m) = transform(input, spec)?;
        lines.push(l);
        labels.push(m);
    }
    let strip = spec.strip.then(|| margin_distribution(&lines[0], visible));
    let mut selected_strip = selected.map(|span| margin_distribution(&lines[0], Some(span)));
    let comparison_strip = selected.and_then(|span| {
        let selected = selected_strip.as_ref()?;
        let visible_dist = margin_distribution(&lines[0], visible);
        let domain = (selected.lo.min(visible_dist.lo), selected.hi.max(visible_dist.hi));
        selected_strip = Some(margin_on_domain(&lines[0], Some(span), Some(domain)));
        Some(margin_on_domain(&lines[0], visible, Some(domain)))
    });
    let selected_count = lines[0].ts.iter().filter(|t| selected.is_some_and(|(a, b)| a <= **t && **t < b)).count();
    let events = align_events(
        events,
        &lines[0].ts,
        inputs[0].tf,
        crate::time_link::cash_class(crate::security(&inputs[0].symbol).class),
    );
    Ok(CanvasReport {
        spec: spec.clone(),
        lines,
        labels,
        strip,
        events,
        selected_strip,
        comparison_strip,
        selected_count,
        visible_span: visible,
        key,
    })
}
/// Stable ascending packing, deterministic ties by input order. At most five flags.
pub fn pack_flags(ys: &[f32], top: f32, bottom: f32) -> Vec<f32> {
    let n = ys.len().min(5);
    if n == 0 {
        return vec![];
    }
    let mut order: Vec<_> = (0..n).collect();
    order.sort_by(|&a, &b| ys[a].total_cmp(&ys[b]).then(a.cmp(&b)));
    let mut out = vec![0.; n];
    let mut prev = top - 16.;
    for &i in &order {
        out[i] = ys[i].clamp(top, bottom).max(prev + 16.);
        prev = out[i];
    }
    let excess = (prev - bottom).max(0.);
    for v in &mut out {
        *v -= excess;
    }
    // Callers reserve >= 80px for 5 flags.
    out
}
/// QUICK accepts up to four exact comparison identities, one interval and one mode.
pub fn parse_spec(s: &str) -> Result<(Timeframe, CompareMode, Vec<String>), String> {
    let mut tf = Timeframe::D1;
    let mut mode = CompareMode::Level;
    let mut peers = Vec::new();
    let (mut has_tf, mut has_mode) = (false, false);
    for w in s.split_whitespace() {
        if let Some((key, v)) = w.split_once('=').filter(|(k, _)| k.eq_ignore_ascii_case("mode")) {
            let _ = key;
            if has_mode {
                return Err("Repeated QUICK mode".into());
            }
            mode = CompareMode::parse(v).ok_or("QUICK mode: level|index100|change|z|vol")?;
            has_mode = true;
        } else if let Ok(t) = w.parse::<Timeframe>() {
            if has_tf || (!t.is_intraday() && t != Timeframe::D1) {
                return Err("QUICK needs one daily/intraday interval".into());
            }
            tf = t;
            has_tf = true;
        } else if ["US", "Equity", "Index", "Curncy", "Comdty"].iter().any(|s| w.eq_ignore_ascii_case(s)) {
            return Err("QUICK ticker qualifier".into());
        } else if peers.len() < 4
            && w.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '^' | '='))
            && !w.contains('=')
        {
            peers.push(crate::security(w).key());
        } else {
            return Err(format!("Unknown QUICK input: {w}"));
        }
    }
    Ok((tf, mode, peers))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tr_core::Provenance;
    fn input(v: &[f64]) -> CanvasInput {
        CanvasInput {
            symbol: "BTCUSD Curncy".into(),
            tf: Timeframe::M1,
            bars: Arc::new(
                v.iter()
                    .enumerate()
                    .map(|(i, &x)| {
                        Bar::new(1_790_000_040 + i as i64 * 60, x, x, x, x, 1.).with_provenance(Provenance::Real)
                    })
                    .collect(),
            ),
            source: "test only".into(),
            observed_at: Some(1_800_000_000),
            stitch: None,
        }
    }
    fn run(v: &[f64], mode: CompareMode, w: usize) -> CanvasReport {
        let i = input(v);
        let spec = CanvasSpec { base_ts: i.bars[0].ts, mode, window: w, strip: true };
        compute(&[i], &spec, None, None, &[]).unwrap()
    }
    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-8, "{a} != {b}");
    }
    #[test]
    fn canvas_modes_hand_values() {
        for (mode, expected) in [
            (CompareMode::Level, vec![100., 110., 99.]),
            (CompareMode::Index100, vec![100., 110., 99.]),
            (CompareMode::Change, vec![0., 10., -1.]),
        ] {
            let r = run(&[100., 110., 99.], mode, 3);
            for (x, y) in r.lines[0].values.iter().zip(expected) {
                approx(x.unwrap(), y);
            }
        }
        let p = [100., 100. * (-0.01_f64).exp(), 100. * (-0.01_f64).exp(), 100., 105.];
        let r = run(&p, CompareMode::Vol, 3);
        approx(r.lines[0].values[4].unwrap(), 100.);
        assert_eq!(r.lines[0].unit, SeriesUnit::BpPerBar);
    }
    #[test]
    fn canvas_z_excludes_current() {
        let r = run(&[1., 2., 3., 4.], CompareMode::Z, 3);
        approx(r.lines[0].values[3].unwrap(), 2.);
        approx(r.labels[0].prior_sd.unwrap(), 1.);
    }
    #[test]
    fn canvas_base_survives_pan() {
        let i = input(&[100., 110., 99.]);
        let spec = CanvasSpec { base_ts: i.bars[0].ts, mode: CompareMode::Index100, ..Default::default() };
        let a = compute(&[i.clone()], &spec, None, None, &[]).unwrap();
        let b = compute(&[i.clone()], &spec, Some((i.bars[1].ts, i.bars[2].ts + 60)), None, &[]).unwrap();
        assert_eq!(a.lines, b.lines);
        assert_ne!(a.key, b.key);
        assert_eq!(b.spec.base_ts, spec.base_ts);
        let mut missing = spec;
        missing.base_ts += 1;
        let r = compute(&[i], &missing, None, None, &[]).unwrap();
        assert!(r.lines[0].values.iter().all(Option::is_none));
        assert!(r.labels[0].unavailable.as_ref().unwrap().contains("no snapping"));
    }
    #[test]
    fn strip_midrank_ties() {
        let r = run(&[1., 2., 2., 4., 2.], CompareMode::Level, 3);
        let s = margin_distribution(&r.lines[0], Some((r.lines[0].ts[0], r.lines[0].ts[4])));
        assert_eq!(s.n, 4);
        approx(s.percentile.unwrap(), 0.5);
        assert_eq!(s.counts.iter().sum::<usize>(), 4);
    }
    #[test]
    fn events_do_not_invent_times() {
        let i = input(&[1., 2., 3.]);
        let date = chrono::DateTime::from_timestamp(i.bars[0].ts, 0).unwrap().date_naive();
        let e = CanvasEvent {
            kind: EventKind::Earnings,
            time: EventTime::Date { start: date, end: date },
            source: "actual provider".into(),
            headline: "earnings date only".into(),
        };
        let r = compute(&[i.clone()], &CanvasSpec::default(), None, None, &[e.clone()]).unwrap();
        assert_eq!(r.events[0].event, e);
        assert_eq!(r.events[0].lo, 0);
        assert_eq!(r.events[0].hi, 3);
        let instant = CanvasEvent { time: EventTime::Instant(i.bars[0].ts - 1), ..e };
        assert!(align_events(&[instant], &r.lines[0].ts, Timeframe::M1, false).is_empty());
    }
    #[test]
    fn last_flags_do_not_overlap() {
        let a = pack_flags(&[40., 40., 40., 40., 40.], 8., 100.);
        assert_eq!(a, pack_flags(&[40.; 5], 8., 100.));
        let mut b = a.clone();
        b.sort_by(f32::total_cmp);
        assert!(b.windows(2).all(|w| w[1] - w[0] >= 16.));
        assert!(b[0] >= 8. && b[4] <= 100.);
    }
    #[test]
    fn sample_canvas_has_no_analytics() {
        let mut i = input(&[1., 2., 3.]);
        Arc::make_mut(&mut i.bars)[2].provenance = Provenance::Sample;
        assert!(compute(&[i], &CanvasSpec::default(), Some((0, 1)), None, &[]).is_err());
    }
    #[test]
    fn full_source_admission_precedes_trim_and_key() {
        let mut i = input(&[1., 2., 3.]);
        let base = CanvasSpec::default();
        let k = input_key(&[i.clone()], &base, None, None, &[]).unwrap();
        Arc::make_mut(&mut i.bars)[0].close = 1.01;
        Arc::make_mut(&mut i.bars)[0].high = 1.01;
        assert_ne!(k, input_key(&[i.clone()], &base, None, None, &[]).unwrap());
        Arc::make_mut(&mut i.bars)[0].low = f64::NAN;
        assert!(input_key(&[i], &base, Some((1_799_000_000, 1_799_000_001)), None, &[]).is_err());
    }
    #[test]
    fn fixed_bar_gaps_and_prefix_invariance() {
        for mode in [CompareMode::Z, CompareMode::Vol] {
            let v: Vec<f64> = (1..30).map(|i| i as f64 + 100.).collect();
            let a = run(&v[..20], mode, 3);
            let b = run(&v, mode, 3);
            assert_eq!(a.lines[0].values, b.lines[0].values[..20]);
            let mut i = input(&v);
            Arc::make_mut(&mut i.bars).remove(10);
            let r = compute(&[i], &CanvasSpec { mode, window: 3, ..Default::default() }, None, None, &[]).unwrap();
            assert!(r.lines[0].break_before[10]);
            assert!(r.lines[0].values[10..13].iter().all(Option::is_none));
        }
    }
    #[test]
    fn canvas_key_covers_clock_gap_receipt_and_view_inputs() {
        let i = input(&[100., 101., 99., 102., 101.]);
        let spec = CanvasSpec { base_ts: i.bars[0].ts, ..Default::default() };
        let k = input_key(&[i.clone()], &spec, None, None, &[]).unwrap();
        for field in 0..10 {
            let mut next = i.clone();
            let mut s = spec.clone();
            let (mut visible, mut selected) = (None, None);
            match field {
                0 => next.observed_at = None,
                1 => next.source.push('x'),
                2 => next.symbol = "ETHUSD Curncy".into(),
                3 => s.base_ts += 1,
                4 => s.window += 1,
                5 => s.mode = CompareMode::Vol,
                6 => s.strip = false,
                7 => visible = Some((10, 20)),
                8 => selected = Some((10, 20)),
                _ => {
                    next.stitch = Some(Arc::new(crate::vault::StitchInfo {
                        gaps: vec![(i.bars[1].ts, i.bars[2].ts)],
                        source: "witness".into(),
                        epoch: 1,
                        ..Default::default()
                    }))
                }
            }
            assert_ne!(k, input_key(&[next], &s, visible, selected, &[]).unwrap(), "field {field}");
        }
    }
    #[test]
    fn brush_distribution_uses_shared_edges_and_each_own_mass() {
        let i = input(&[1., 2., 2., 4., 6., 9.]);
        let selected = (i.bars[1].ts, i.bars[4].ts);
        let r = compute(&[i], &CanvasSpec::default(), None, Some(selected), &[]).unwrap();
        let a = r.selected_strip.unwrap();
        let b = r.comparison_strip.unwrap();
        assert_eq!((a.lo, a.hi), (b.lo, b.hi));
        assert_eq!(a.n, 3);
        assert_eq!(b.n, 6);
        approx(a.mass.iter().sum(), 1.);
        approx(b.mass.iter().sum(), 1.);
    }
    #[test]
    fn rolling_references_match_direct_sample_variance_and_flat_is_unavailable() {
        let v: Vec<_> = (0..1600).map(|i| 100. + (i as f64 * 0.17).sin()).collect();
        let r = run(&v, CompareMode::Z, 63);
        for t in 63..v.len() {
            let (mean, sd) = sample_stats(&v[t - 63..t]).unwrap();
            assert!((r.lines[0].values[t].unwrap() - (v[t] - mean) / sd).abs() < 1e-9);
        }
        let flat = run(&[5.; 100], CompareMode::Z, 63);
        assert!(flat.lines[0].values.iter().all(Option::is_none));
        let vol = run(&[5.; 100], CompareMode::Vol, 63);
        assert_eq!(vol.lines[0].values[99], Some(0.));
    }
    #[test]
    fn nonpositive_prices_refused_and_unsupported_calendar_withholds_rolling() {
        let mut i = input(&[1., 2., 3.]);
        i.symbol = "CL1 Comdty".into();
        let r =
            compute(&[i.clone()], &CanvasSpec { mode: CompareMode::Z, ..Default::default() }, None, None, &[]).unwrap();
        assert!(r.labels[0].unavailable.as_ref().unwrap().contains("calendar"));
        Arc::make_mut(&mut i.bars)[1].low = -1.;
        assert!(compute(&[i], &CanvasSpec::default(), None, None, &[]).is_err());
    }
    #[test]
    fn pending_clock_and_request_identity() {
        let mut i = input(&[1., 2., 3.]);
        i.observed_at = Some(i.bars[2].ts + 1);
        let r = compute(&[i.clone()], &CanvasSpec::default(), None, None, &[]).unwrap();
        assert_eq!(r.lines[0].ts.len(), 2);
        let key = r.key;
        i.observed_at = None;
        assert_ne!(key, input_key(&[i], &CanvasSpec::default(), None, None, &[]).unwrap());
        assert!(parse_spec("QQQ 5m mode=index100").is_ok());
        assert!(parse_spec("QQQ 5m mode=nope").is_err());
    }
}
