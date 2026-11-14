//! Verified, closed US cash-session volume-at-price. Pure computation; no I/O.
use crate::{intraday, time_link};
use chrono::NaiveDate;
use std::collections::{BTreeMap, BTreeSet};
use tr_core::{AssetClass, Bar, Timeframe};

pub const MAX_SESSIONS: usize = 20;
pub const TARGET_ROWS: f64 = 40.;
pub const FINE_HOLE_TOLERANCE: f64 = 0.05;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AuctionParams {
    pub lookback: usize,
    pub value_area_pct: u8,
    pub max_age: usize,
}
impl Default for AuctionParams {
    fn default() -> Self {
        Self { lookback: 20, value_area_pct: 70, max_age: 10 }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuctionKey {
    pub generation: u64,
    pub tf: Timeframe,
    pub closed: usize,
    pub inputs: u64,
    pub params: AuctionParams,
    pub epoch: u32,
    pub gaps: Vec<(i64, i64)>,
}
impl AuctionKey {
    pub fn new(generation: u64, tf: Timeframe, closed: usize, params: AuctionParams, bars: &[Bar], epoch: u32) -> Self {
        Self {
            generation,
            tf,
            closed,
            params,
            epoch,
            gaps: vec![],
            inputs: crate::stable_hash::bars_digest(&bars[..closed.min(bars.len())]),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    Complete,
    Developing,
    Partial,
}
#[derive(Clone, Debug)]
pub struct PrintRun {
    pub lo: i64,
    pub hi: i64,
    pub tail: bool,
}
#[derive(Clone, Copy, Debug)]
pub struct Ib {
    pub high: f64,
    pub low: f64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenLocation {
    AboveValue,
    InValue,
    BelowValue,
    OutsideRange,
}
#[derive(Clone, Debug)]
pub struct SessionProfile {
    pub date: NaiveDate,
    pub open_ts: i64,
    pub close_ts: i64,
    pub first_bar_ts: i64,
    pub last_bar_ts: i64,
    pub first_bin: i64,
    pub volume: Vec<f64>,
    pub brackets: Vec<u8>,
    pub total: f64,
    pub poc_bin: i64,
    pub va_bins: (i64, i64),
    pub va_share: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub single_prints: Vec<PrintRun>,
    pub lvn: Vec<i64>,
    pub ib: Option<Ib>,
    pub open_location: Option<OpenLocation>,
    pub source_tf: Timeframe,
    pub bars_used: usize,
    pub expected: usize,
    pub holes: usize,
    pub no_volume: usize,
    pub status: SessionStatus,
    pub half_day: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LevelKind {
    Poc,
    Vah,
    Val,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LevelEnd {
    Naked,
    Touched { ts: i64 },
    CutByGap { ts: i64 },
}
#[derive(Clone, Debug)]
pub struct Level {
    pub kind: LevelKind,
    pub price: f64,
    pub origin: NaiveDate,
    pub origin_open_ts: i64,
    pub origin_close_ts: i64,
    pub age: usize,
    pub tests: usize,
    pub gapped_over: bool,
    pub uncertain: bool,
    pub end: LevelEnd,
}
#[derive(Clone, Debug)]
pub struct AuctionReport {
    pub bin: f64,
    pub tick: f64,
    pub sessions: Vec<SessionProfile>,
    pub levels: Vec<Level>,
    pub gaps: Vec<(i64, i64)>,
    pub closed_thru: i64,
    pub source: String,
    pub fine_note: Option<String>,
    pub eth_ignored: usize,
    pub params: AuctionParams,
}
#[derive(Clone, Debug)]
pub struct Composite {
    pub first_bin: i64,
    pub volume: Vec<f64>,
    pub total: f64,
    pub poc_bin: i64,
    pub va_bins: (i64, i64),
    pub va_share: f64,
    pub partial: usize,
}
fn bin_at(p: f64, w: f64) -> i64 {
    (p / w + 1e-9).floor() as i64
}
fn nice_ceil(x: f64) -> f64 {
    if x <= 1. {
        return 1.;
    }
    let unit = 10_f64.powf(x.log10().floor());
    [1., 2., 5., 10.].into_iter().find(|n| n * unit >= x - 1e-9).unwrap() * unit
}
fn spread(b: &Bar, w: f64, first: i64, v: &mut [f64]) {
    let Some(vol) = b.volume_value() else { return };
    if b.high == b.low {
        v[(bin_at(b.low, w) - first) as usize] += vol;
    } else {
        for k in bin_at(b.low, w)..=bin_at(b.high, w) {
            let overlap = (b.high.min((k + 1) as f64 * w) - b.low.max(k as f64 * w)).max(0.);
            v[(k - first) as usize] += vol * overlap / (b.high - b.low);
        }
    }
}
fn poc(v: &[f64], midpoint: f64) -> usize {
    (0..v.len())
        .min_by(|&a, &b| {
            v[b].total_cmp(&v[a])
                .then_with(|| (a as f64 - midpoint).abs().total_cmp(&(b as f64 - midpoint).abs()))
                .then(a.cmp(&b))
        })
        .unwrap_or(0)
}
fn value_area(v: &[f64], p: usize, pct: u8) -> (usize, usize, f64) {
    let total: f64 = v.iter().sum();
    let (mut lo, mut hi, mut sum) = (p, p, v[p]);
    let mut below: f64 = v[..p].iter().sum();
    let mut above: f64 = v[p + 1..].iter().sum();
    while sum < total * pct as f64 / 100. && (lo > 0 || hi + 1 < v.len()) {
        let down = if lo == 0 {
            false
        } else if hi + 1 == v.len() {
            true
        } else {
            v[lo - 1] > v[hi + 1] || (v[lo - 1] == v[hi + 1] && below >= above)
        };
        if down {
            lo -= 1;
            sum += v[lo];
            below -= v[lo];
        } else {
            hi += 1;
            sum += v[hi];
            above -= v[hi];
        }
    }
    (lo, hi, if total > 0. { sum / total } else { 0. })
}
fn prints(v: &[u8], first: i64) -> Vec<PrintRun> {
    let mut out = vec![];
    let mut i = 0;
    while i < v.len() {
        if v[i] != 1 {
            i += 1;
            continue;
        }
        let start = i;
        while i < v.len() && v[i] == 1 {
            i += 1;
        }
        if i - start >= 2 {
            out.push(PrintRun { lo: first + start as i64, hi: first + i as i64 - 1, tail: start == 0 || i == v.len() });
        }
    }
    out
}
fn lvns(v: &[f64], first: i64) -> Vec<i64> {
    let s: Vec<_> = (0..v.len())
        .map(|i| {
            0.5 * v[i] + 0.25 * (v.get(i.wrapping_sub(1)).copied().unwrap_or(0.) + v.get(i + 1).copied().unwrap_or(0.))
        })
        .collect();
    (2..s.len().saturating_sub(2))
        .filter(|&i| {
            s[i - 2..=i + 2].iter().enumerate().all(|(j, x)| j == 2 || s[i] < *x)
                && s[i]
                    <= 0.35
                        * s[..i].iter().copied().fold(0., f64::max).min(s[i + 1..].iter().copied().fold(0., f64::max))
        })
        .map(|i| first + i as i64)
        .collect()
}
fn bounds(date: NaiveDate) -> (i64, i64) {
    let open = tr_core::ny::to_utc(date.and_hms_opt(9, 30, 0).unwrap());
    let close = intraday::close_minute(date);
    (open, tr_core::ny::to_utc(date.and_hms_opt(close / 60, close % 60, 0).unwrap()))
}
// Cash sessions never cross a DST transition. Calculate the two NY endpoints
// once per date; minute offsets within that regular session are exact in UTC.
fn session_opens(date: NaiveDate, tf: Timeframe) -> Vec<i64> {
    let (open, close) = bounds(date);
    (open..close).step_by(tf.seconds() as usize).collect()
}
fn grouped(bars: &[Bar], tf: Timeframe, cut: i64) -> BTreeMap<NaiveDate, Vec<Bar>> {
    let mut out: BTreeMap<_, Vec<_>> = BTreeMap::new();
    let mut current = None;
    let mut window = (0, 0);
    for b in bars {
        let date = tr_core::ny::civil(b.ts).date();
        if current != Some(date) {
            current = Some(date);
            window = if crate::history::calendar::is_session(date) { bounds(date) } else { (0, 0) };
        }
        if b.ts >= window.0
            && b.ts < window.1
            && (b.ts - window.0) % tf.seconds() == 0
            && (b.ts + tf.seconds()).min(window.1) <= cut
        {
            out.entry(date).or_default().push(*b);
        }
    }
    out
}
fn expected(date: NaiveDate, tf: Timeframe, cut: i64) -> Vec<i64> {
    let (_, close) = bounds(date);
    session_opens(date, tf).into_iter().filter(|&t| (t + tf.seconds()).min(close) <= cut).collect()
}
fn holes(bars: &[Bar], opens: &[i64]) -> usize {
    opens.iter().filter(|&&t| bars.binary_search_by_key(&t, |b| b.ts).is_err()).count()
}
/// Callers pass a closed chart prefix. A second cut gate also excludes forming
/// fine bars and guards direct callers. No missing bucket is ever manufactured.
#[allow(clippy::too_many_arguments)]
pub fn build(
    chart: &[Bar],
    tf: Timeframe,
    fine: Option<&[Bar]>,
    class: AssetClass,
    cut: i64,
    gaps: &[(i64, i64)],
    params: AuctionParams,
    tick: f64,
) -> Result<AuctionReport, String> {
    if !time_link::cash_class(class) {
        return Err("Auction Map needs a US cash session".into());
    }
    if !tf.is_intraday() || tf.seconds() > 3600 {
        return Err("Auction Map needs a live intraday chart (1m–1h)".into());
    }
    Bar::require_verified(chart, "Auction Map")?;
    if !(1..=20).contains(&params.lookback)
        || !(1..=20).contains(&params.max_age)
        || !(50..=95).contains(&params.value_area_pct)
        || !tick.is_finite()
        || tick <= 0.
    {
        return Err("Auction Map invalid parameters".into());
    }
    if chart.windows(2).any(|w| w[0].ts >= w[1].ts) || chart.iter().any(|b| !b.valid_ohlc() || b.low <= 0.) {
        return Err("Auction Map needs ordered, valid verified bars".into());
    }
    let chart_days = grouped(chart, tf, cut);
    let fine = fine.filter(|b| {
        Bar::require_verified(b, "Auction Map").is_ok()
            && b.windows(2).all(|w| w[0].ts < w[1].ts)
            && b.iter().all(|b| b.valid_ohlc() && b.low > 0.)
    });
    let fine_days = grouped(fine.unwrap_or(&[]), Timeframe::M1, cut);
    let dates: BTreeSet<_> = chart_days.keys().chain(fine_days.keys()).copied().collect();
    let dates: Vec<_> =
        dates.into_iter().rev().take(params.lookback.max(params.max_age).min(MAX_SESSIONS) + 1).collect();
    let mut chosen = vec![];
    for date in dates.into_iter().rev() {
        let f = fine_days.get(&date);
        let opens = if f.is_some() { expected(date, Timeframe::M1, cut) } else { vec![] };
        let fine_ok = f.is_some_and(|b| {
            !opens.is_empty()
                && b.first().is_some_and(|b| b.ts < opens[0] + tf.seconds())
                && b.last().is_some_and(|b| b.ts + 60 >= opens[opens.len() - 1] + 60)
                && holes(b, &opens) as f64 <= opens.len() as f64 * FINE_HOLE_TOLERANCE
        });
        let (bars, source_tf) = if fine_ok {
            (f.unwrap(), Timeframe::M1)
        } else if let Some(b) = chart_days.get(&date) {
            (b, tf)
        } else {
            continue;
        };
        let scheduled = session_opens(date, source_tf);
        let open_ts = scheduled[0];
        let close_ts = bounds(date).1;
        let wanted = expected(date, source_tf, cut);
        let missing = holes(bars, &wanted);
        let no_volume = bars.iter().filter(|b| b.volume_value().is_none()).count();
        let status = if missing > 0 || no_volume > 0 {
            SessionStatus::Partial
        } else if cut < close_ts {
            SessionStatus::Developing
        } else {
            SessionStatus::Complete
        };
        chosen.push((date, bars, source_tf, open_ts, close_ts, wanted.len(), missing, no_volume, status));
    }
    let range = |b: &[Bar]| {
        (
            b.iter().map(|b| b.high).fold(f64::NEG_INFINITY, f64::max),
            b.iter().map(|b| b.low).fold(f64::INFINITY, f64::min),
        )
    };
    let mut ranges: Vec<_> = chosen
        .iter()
        .filter(|x| x.8 == SessionStatus::Complete)
        .map(|x| {
            let (h, l) = range(x.1);
            h - l
        })
        .collect();
    ranges.sort_by(f64::total_cmp);
    let r = if ranges.is_empty() {
        chosen.last().map_or(0., |x| {
            let (h, l) = range(x.1);
            h - l
        })
    } else {
        ranges[ranges.len() / 2]
    };
    let bin = tick * nice_ceil(r / TARGET_ROWS / tick);
    if !bin.is_finite() || bin <= 0. {
        return Err("Auction Map price range exceeds the lattice limit".into());
    }
    let mut sessions = vec![];
    let mut source_bars = vec![];
    for (date, bars, source_tf, open_ts, close_ts, expected, holes, no_volume, status) in chosen {
        let (high, low) = range(bars);
        if high / bin >= i64::MAX as f64 {
            return Err("Auction Map price range exceeds the lattice limit".into());
        }
        let first_bin = bin_at(low, bin);
        let last_bin = bin_at(high, bin);
        // Refuse pathological price spans rather than allocating unbounded memory.
        if last_bin.saturating_sub(first_bin) > 20_000 {
            return Err("Auction Map price range exceeds the lattice limit".into());
        }
        let mut volume = vec![0.; (last_bin - first_bin + 1) as usize];
        let mut masks = vec![0_u16; volume.len()];
        for b in bars {
            spread(b, bin, first_bin, &mut volume);
            if source_tf.seconds() <= 1800 {
                let bracket = ((b.ts - open_ts) / 1800) as u32;
                for k in bin_at(b.low, bin)..=bin_at(b.high, bin) {
                    masks[(k - first_bin) as usize] |= 1 << bracket;
                }
            }
        }
        let brackets: Vec<_> =
            if source_tf.seconds() <= 1800 { masks.iter().map(|m| m.count_ones() as u8).collect() } else { vec![] };
        let total: f64 = volume.iter().sum();
        if !total.is_finite() {
            return Err("Auction Map volume exceeds the numeric limit".into());
        }
        let p = poc(&volume, (high + low) / 2. / bin - first_bin as f64 - 0.5);
        let (vl, vh, va_share) = value_area(&volume, p, params.value_area_pct);
        let hour: Vec<_> = (open_ts..open_ts + 3600).step_by(source_tf.seconds() as usize).collect();
        let ib = (cut >= open_ts + 3600 && holes_in_hour(bars, &hour) && source_tf.seconds() <= 3600).then(|| {
            let b: Vec<_> = bars.iter().take_while(|b| b.ts < open_ts + 3600).copied().collect();
            let (high, low) = range(&b);
            Ib { high, low }
        });
        let open_location = sessions.last().and_then(|prior: &SessionProfile| {
            (prior.status == SessionStatus::Complete
                && prior.total > 0.
                && crate::history::calendar::previous_session(date) == prior.date
                && bars[0].ts == open_ts)
                .then(|| {
                    let o = bars[0].open;
                    if o > prior.high || o < prior.low {
                        OpenLocation::OutsideRange
                    } else if o > (prior.va_bins.1 + 1) as f64 * bin {
                        OpenLocation::AboveValue
                    } else if o < prior.va_bins.0 as f64 * bin {
                        OpenLocation::BelowValue
                    } else {
                        OpenLocation::InValue
                    }
                })
        });
        sessions.push(SessionProfile {
            date,
            open_ts,
            close_ts,
            first_bar_ts: bars[0].ts,
            last_bar_ts: bars.last().unwrap().ts,
            first_bin,
            single_prints: prints(&brackets, first_bin),
            lvn: lvns(&volume, first_bin),
            volume,
            brackets,
            total,
            poc_bin: first_bin + p as i64,
            va_bins: (first_bin + vl as i64, first_bin + vh as i64),
            va_share,
            open: bars[0].open,
            high,
            low,
            close: bars.last().unwrap().close,
            ib,
            open_location,
            source_tf,
            bars_used: bars.len(),
            expected,
            holes,
            no_volume,
            status,
            half_day: intraday::early_close(date),
        });
        source_bars.push(bars);
    }
    if !sessions.iter().any(|s| s.total > 0.) {
        return Err("Auction Map needs verified volume".into());
    }
    let mut levels = vec![];
    for (i, s) in sessions.iter().enumerate().filter(|(_, s)| s.status == SessionStatus::Complete && s.total > 0.) {
        let age = sessions.len() - 1 - i;
        if age > params.max_age {
            continue;
        }
        for (kind, price) in [
            (LevelKind::Poc, (s.poc_bin as f64 + 0.5) * bin),
            (LevelKind::Vah, (s.va_bins.1 + 1) as f64 * bin),
            (LevelKind::Val, s.va_bins.0 as f64 * bin),
        ] {
            let mut level = Level {
                kind,
                price,
                origin: s.date,
                origin_open_ts: s.open_ts,
                origin_close_ts: s.close_ts,
                age,
                tests: 0,
                gapped_over: false,
                uncertain: false,
                end: LevelEnd::Naked,
            };
            let gap = gaps
                .iter()
                .filter(|(a, b)| *b > s.close_ts && *a >= s.first_bar_ts && *a < cut)
                .min_by_key(|(a, _)| *a);
            let mut prev = source_bars[i].last().copied();
            'later: for (j, later) in sessions.iter().enumerate().skip(i + 1) {
                for b in source_bars[j] {
                    if let Some(&(a, _)) = gap
                        && b.ts > a
                    {
                        level.end = LevelEnd::CutByGap { ts: a };
                        break 'later;
                    }
                    if b.low <= price && price <= b.high {
                        level.end = LevelEnd::Touched { ts: b.ts };
                        break 'later;
                    }
                    if let Some(a) = prev
                        && ((a.close < price && b.open > price) || (a.close > price && b.open < price))
                        && !(a.low <= price && price <= a.high)
                    {
                        level.gapped_over = true;
                    }
                    prev = Some(*b);
                }
                if (price - later.high).max(later.low - price).max(0.) <= bin {
                    level.tests += 1;
                }
                if later.status == SessionStatus::Partial {
                    level.uncertain = true;
                }
            }
            // A known gap may extend beyond the last session with usable data.
            if level.end == LevelEnd::Naked
                && let Some(&(a, _)) = gap
            {
                level.end = LevelEnd::CutByGap { ts: a };
            }
            levels.push(level);
        }
    }
    let fallback = sessions.iter().any(|s| s.source_tf != Timeframe::M1);
    Ok(AuctionReport {
        bin,
        tick,
        sessions,
        levels,
        gaps: gaps.to_vec(),
        closed_thru: cut,
        source: "verified chart bars".into(),
        fine_note: fallback.then(|| format!("1m unavailable for some sessions · {tf} fallback")),
        eth_ignored: chart.iter().filter(|b| !intraday::in_regular_session(b.ts)).count(),
        params,
    })
}
fn holes_in_hour(b: &[Bar], opens: &[i64]) -> bool {
    !opens.is_empty()
        && opens.iter().all(|t| {
            b.binary_search_by_key(t, |b| b.ts).is_ok_and(|i| b[i].provenance.is_verified() && b[i].valid_ohlc())
        })
}
pub fn composite(sessions: &[&SessionProfile], pct: u8) -> Option<Composite> {
    let first = sessions.iter().filter(|s| s.total > 0.).map(|s| s.first_bin).min()?;
    let last = sessions.iter().filter(|s| s.total > 0.).map(|s| s.first_bin + s.volume.len() as i64).max()?;
    if last.saturating_sub(first) > 20_000 {
        return None;
    }
    let mut volume = vec![0.; (last - first) as usize];
    for s in sessions.iter().filter(|s| s.total > 0.) {
        for (i, v) in s.volume.iter().enumerate() {
            volume[(s.first_bin - first) as usize + i] += v;
        }
    }
    let total: f64 = volume.iter().sum();
    if !total.is_finite() {
        return None;
    }
    let p = poc(&volume, (volume.len() - 1) as f64 / 2.);
    let (lo, hi, va_share) = value_area(&volume, p, pct);
    Some(Composite {
        first_bin: first,
        volume,
        total,
        poc_bin: first + p as i64,
        va_bins: (first + lo as i64, first + hi as i64),
        va_share,
        partial: sessions.iter().filter(|s| s.status == SessionStatus::Partial).count(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tr_core::Provenance;
    fn date(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }
    fn day(s: &str, tf: Timeframe) -> Vec<Bar> {
        intraday::regular_opens(date(s), tf)
            .into_iter()
            .map(|ts| Bar::new(ts, 100., 101., 99., 100., 100.).with_provenance(Provenance::Real))
            .collect()
    }
    fn run(b: &[Bar], tf: Timeframe, fine: Option<&[Bar]>, gaps: &[(i64, i64)]) -> AuctionReport {
        build(
            b,
            tf,
            fine,
            AssetClass::Equity,
            time_link::bar_end(tf, b.last().unwrap().ts, true),
            gaps,
            Default::default(),
            0.01,
        )
        .unwrap()
    }
    #[test]
    fn uniform_spread_conserves_volume_and_splits_by_overlap() {
        let b = Bar::new(0, 1., 2.5, 0.5, 2., 200.);
        let mut v = vec![0.; 3];
        spread(&b, 1., 0, &mut v);
        assert_eq!(v, vec![50., 100., 50.]);
        assert_eq!(v.iter().sum::<f64>(), 200.);
    }
    #[test]
    fn flat_bar_puts_all_volume_in_one_bin() {
        let mut v = vec![0.; 3];
        spread(&Bar::new(0, 1., 1., 1., 1., 7.), 1., 0, &mut v);
        assert_eq!(v, vec![0., 7., 0.]);
    }
    #[test]
    fn lattice_width_is_a_nice_tick_multiple_near_forty_rows() {
        for x in [0.2, 1., 1.1, 4., 8., 19., 51.] {
            let n = nice_ceil(x);
            assert!(n >= x && n <= x.max(1.) * 2.5);
            let unit = 10_f64.powf(n.log10().floor());
            assert!([1., 2., 5.].iter().any(|t| (n / unit - t).abs() < 1e-8));
        }
        let r = run(&day("2026-11-10", Timeframe::M1), Timeframe::M1, None, &[]);
        assert!((r.bin - 0.05).abs() < 1e-10);
    }
    #[test]
    fn lattice_is_stable_when_the_developing_session_extends_its_range() {
        let mut b = day("2026-11-09", Timeframe::M1);
        b.extend(day("2026-11-10", Timeframe::M1).into_iter().take(10));
        let r = run(&b, Timeframe::M1, None, &[]);
        b.last_mut().unwrap().high = 180.;
        assert_eq!(run(&b, Timeframe::M1, None, &[]).bin, r.bin);
    }
    #[test]
    fn poc_on_a_known_triangular_distribution_and_tie_breaks_to_midpoint_then_lower() {
        assert_eq!(poc(&[1., 2., 4., 2., 1.], 2.), 2);
        assert_eq!(poc(&[4., 1., 4., 1., 4.], 2.), 2);
        assert_eq!(poc(&[4., 1., 4.], 1.), 0);
    }
    #[test]
    fn value_area_reaches_seventy_percent_and_stays_contiguous_across_empty_rows() {
        let (lo, hi, share) = value_area(&[2., 0., 4., 0., 2.], 2, 70);
        assert_eq!((lo, hi), (0, 2));
        assert!(share >= 0.7);
    }
    #[test]
    fn value_area_tie_takes_the_side_with_more_remaining_volume_then_lower() {
        assert_eq!(value_area(&[1., 1., 5., 1., 4.], 2, 50).0, 2);
        assert_eq!(value_area(&[2., 5., 2.], 1, 70).0, 0);
    }
    #[test]
    fn value_area_of_a_single_row_is_that_row() {
        assert_eq!(value_area(&[8.], 0, 70), (0, 0, 1.));
    }
    #[test]
    fn sessions_split_on_new_york_dates_across_dst_start_and_end() {
        use chrono::{DateTime, Timelike, Utc};
        for (a, b, ha, hb) in [("2026-03-06", "2026-03-09", 14, 13), ("2026-10-30", "2026-11-02", 13, 14)] {
            let mut bars = day(a, Timeframe::M1);
            bars.extend(day(b, Timeframe::M1));
            let r = run(&bars, Timeframe::M1, None, &[]);
            assert_eq!(r.sessions.len(), 2);
            assert_eq!(DateTime::<Utc>::from_timestamp(r.sessions[0].open_ts, 0).unwrap().hour(), ha);
            assert_eq!(DateTime::<Utc>::from_timestamp(r.sessions[1].open_ts, 0).unwrap().hour(), hb);
            assert_eq!(r.sessions[1].date, date(b));
        }
    }
    #[test]
    fn half_day_session_expects_210_minutes_and_is_complete() {
        let r = run(&day("2026-11-27", Timeframe::M1), Timeframe::M1, None, &[]);
        assert_eq!(r.sessions[0].expected, 210);
        assert_eq!(r.sessions[0].status, SessionStatus::Complete);
        assert!(r.sessions[0].half_day);
    }
    #[test]
    fn holiday_is_not_a_gap_and_not_an_age_step() {
        let mut b = day("2026-11-25", Timeframe::M1);
        b.extend(day("2026-11-27", Timeframe::M1));
        let r = run(&b, Timeframe::M1, None, &[]);
        assert_eq!(r.sessions.len(), 2);
        assert_eq!(r.levels[0].age, 1);
        assert!(r.gaps.is_empty());
    }
    #[test]
    fn fine_bars_are_preferred_and_a_holey_fine_session_falls_back_to_chart_bars() {
        let b = day("2026-11-10", Timeframe::M5);
        let mut f = day("2026-11-10", Timeframe::M1);
        assert_eq!(run(&b, Timeframe::M5, Some(&f), &[]).sessions[0].source_tf, Timeframe::M1);
        f.drain(30..60);
        let r = run(&b, Timeframe::M5, Some(&f), &[]);
        assert_eq!(r.sessions[0].source_tf, Timeframe::M5);
        assert!(r.fine_note.is_some());
    }
    #[test]
    fn tolerated_fine_holes_still_mark_partial_and_missing_edges_fall_back() {
        let b = day("2026-11-10", Timeframe::M5);
        let mut f = day("2026-11-10", Timeframe::M1);
        f.remove(10);
        let r = run(&b, Timeframe::M5, Some(&f), &[]);
        assert_eq!(r.sessions[0].holes, 1);
        assert_eq!(r.sessions[0].status, SessionStatus::Partial);
        f.pop();
        assert_eq!(run(&b, Timeframe::M5, Some(&f), &[]).sessions[0].source_tf, Timeframe::M5);
    }
    #[test]
    fn fine_bars_after_the_chart_cut_are_excluded() {
        let b: Vec<_> = day("2026-11-10", Timeframe::M5).into_iter().take(2).collect();
        let mut f = day("2026-11-10", Timeframe::M1);
        for b in f.iter_mut().skip(10) {
            b.high = 1000.;
            b.volume = 1e9;
        }
        let r = run(&b, Timeframe::M5, Some(&f), &[]);
        assert_eq!(r.sessions[0].bars_used, 10);
        assert_eq!(r.sessions[0].high, 101.);
        assert!((r.sessions[0].total - 1000.).abs() < 1e-8);
    }
    #[test]
    fn missing_volume_bar_marks_the_session_partial_and_adds_nothing() {
        let mut b = day("2026-11-10", Timeframe::M1);
        b[5].volume = f64::NAN;
        let r = run(&b, Timeframe::M1, None, &[]);
        assert_eq!(r.sessions[0].status, SessionStatus::Partial);
        assert_eq!(r.sessions[0].no_volume, 1);
        assert!((r.sessions[0].total - 38900.).abs() < 1e-6);
    }
    #[test]
    fn zero_volume_session_has_no_profile() {
        let mut b = day("2026-11-10", Timeframe::M1);
        for b in &mut b {
            b.volume = 0.;
        }
        assert!(
            build(
                &b,
                Timeframe::M1,
                None,
                AssetClass::Equity,
                b.last().unwrap().ts + 60,
                &[],
                Default::default(),
                0.01
            )
            .unwrap_err()
            .contains("verified volume")
        );
    }
    #[test]
    fn single_print_run_needs_two_rows_and_flags_tails() {
        let p = prints(&[1, 1, 2, 1, 2, 1, 1], 100);
        assert_eq!(p.len(), 2);
        assert!(p.iter().all(|p| p.tail));
        assert_eq!(p[1].lo, 105);
        assert!(!prints(&[2, 1, 1, 2], 0)[0].tail);
    }
    #[test]
    fn brackets_are_unavailable_above_thirty_minute_source_bars() {
        let r = run(&day("2026-11-10", Timeframe::H1), Timeframe::H1, None, &[]);
        assert!(r.sessions[0].brackets.is_empty());
        assert!(r.sessions[0].single_prints.is_empty());
    }
    #[test]
    fn lvn_is_a_strict_smoothed_minimum_below_the_threshold() {
        assert_eq!(lvns(&[100., 100., 80., 2., 0., 2., 80., 100., 100.], 0), vec![4]);
        assert!(lvns(&[1.; 10], 0).is_empty());
    }
    #[test]
    fn initial_balance_needs_every_first_hour_bucket() {
        let mut b = day("2026-11-10", Timeframe::M5);
        assert!(run(&b, Timeframe::M5, None, &[]).sessions[0].ib.is_some());
        b.remove(4);
        assert!(run(&b, Timeframe::M5, None, &[]).sessions[0].ib.is_none());
    }
    #[test]
    fn open_location_against_prior_value_and_none_without_a_complete_prior() {
        let mut b = day("2026-11-09", Timeframe::M1);
        b.extend(day("2026-11-10", Timeframe::M1));
        assert_eq!(run(&b, Timeframe::M1, None, &[]).sessions[1].open_location, Some(OpenLocation::InValue));
        b[390].open = 101.;
        b[390].high = 102.;
        assert_eq!(run(&b, Timeframe::M1, None, &[]).sessions[1].open_location, Some(OpenLocation::AboveValue));
        b.remove(5);
        assert!(run(&b, Timeframe::M1, None, &[]).sessions[1].open_location.is_none());
    }
    fn flat_day(d: &str, p: f64) -> Vec<Bar> {
        day(d, Timeframe::M1)
            .into_iter()
            .map(|b| Bar::new(b.ts, p, p, p, p, 100.).with_provenance(Provenance::Real))
            .collect()
    }
    #[test]
    fn naked_level_ends_at_the_first_bar_whose_range_contains_it() {
        let mut b = flat_day("2026-11-09", 100.);
        let mut next = flat_day("2026-11-10", 102.);
        next[10].low = 99.;
        b.extend(next);
        let r = run(&b, Timeframe::M1, None, &[]);
        assert_eq!(r.levels[0].end, LevelEnd::Touched { ts: b[400].ts });
    }
    #[test]
    fn gap_over_a_level_keeps_it_naked_and_flags_it() {
        let mut b = flat_day("2026-11-09", 100.);
        b.extend(flat_day("2026-11-10", 102.));
        let r = run(&b, Timeframe::M1, None, &[]);
        assert_eq!(r.levels[0].end, LevelEnd::Naked);
        assert!(r.levels[0].gapped_over);
    }
    #[test]
    fn tests_count_sessions_within_one_bin_without_touching() {
        let mut b = flat_day("2026-11-09", 100.);
        b.extend(flat_day("2026-11-10", 100.01));
        assert_eq!(run(&b, Timeframe::M1, None, &[]).levels[0].tests, 1);
    }
    #[test]
    fn level_is_cut_at_an_unverified_vault_gap() {
        let mut b = flat_day("2026-11-09", 100.);
        let at = b.last().unwrap().ts;
        b.extend(flat_day("2026-11-11", 102.));
        let r = run(&b, Timeframe::M1, None, &[(at, b[390].ts)]);
        assert_eq!(r.levels[0].end, LevelEnd::CutByGap { ts: at });
    }
    #[test]
    fn levels_older_than_max_age_are_dropped_and_partial_sessions_emit_none() {
        let mut b = flat_day("2026-11-06", 100.);
        b.extend(flat_day("2026-11-09", 102.));
        b.extend(flat_day("2026-11-10", 103.));
        let p = AuctionParams { max_age: 1, ..Default::default() };
        let r = build(&b, Timeframe::M1, None, AssetClass::Equity, b.last().unwrap().ts + 60, &[], p, 0.01).unwrap();
        assert!(r.levels.iter().all(|l| l.age <= 1));
        b.remove(0);
        assert!(run(&b, Timeframe::M1, None, &[]).levels.iter().all(|l| l.origin != date("2026-11-06")));
    }
    #[test]
    fn forming_bar_never_touches_a_level() {
        let mut b = flat_day("2026-11-09", 100.);
        b.extend(flat_day("2026-11-10", 102.).into_iter().take(2));
        let cut = b.last().unwrap().ts;
        b.last_mut().unwrap().low = 99.;
        let r = build(&b, Timeframe::M1, None, AssetClass::Equity, cut, &[], Default::default(), 0.01).unwrap();
        assert_eq!(r.levels[0].end, LevelEnd::Naked);
        assert_eq!(r.sessions[1].bars_used, 1);
    }
    #[test]
    fn composite_equals_the_sum_of_session_vectors_and_its_poc_matches() {
        let mut b = day("2026-11-09", Timeframe::M1);
        b.extend(day("2026-11-10", Timeframe::M1));
        let r = run(&b, Timeframe::M1, None, &[]);
        let c = composite(&r.sessions.iter().collect::<Vec<_>>(), 70).unwrap();
        for (i, v) in c.volume.iter().enumerate() {
            assert!((*v - r.sessions[0].volume[i] * 2.).abs() < 1e-8);
        }
        assert_eq!(c.poc_bin, r.sessions[0].poc_bin);
        assert!((c.total - 78000.).abs() < 1e-6);
    }
    #[test]
    fn extended_hours_bars_are_ignored_and_counted() {
        let mut b = day("2026-11-10", Timeframe::M1);
        let mut pre = b[0];
        pre.ts -= 60;
        pre.high = 1000.;
        b.insert(0, pre);
        let r = run(&b, Timeframe::M1, None, &[]);
        assert_eq!(r.eth_ignored, 1);
        assert_eq!(r.sessions[0].high, 101.);
    }
    #[test]
    fn extreme_prices_and_composite_volume_overflow_refuse_without_panicking() {
        let bars = flat_day("2026-11-10", 1e30);
        assert!(
            build(
                &bars,
                Timeframe::M1,
                None,
                AssetClass::Equity,
                bars.last().unwrap().ts + 60,
                &[],
                Default::default(),
                0.01,
            )
            .unwrap_err()
            .contains("lattice limit")
        );

        let r = run(&flat_day("2026-11-10", 100.), Timeframe::M1, None, &[]);
        let mut s = r.sessions[0].clone();
        s.volume = vec![f64::MAX];
        s.total = f64::MAX;
        assert!(composite(&[&s, &s], 70).is_none());
    }
    #[test]
    fn non_cash_and_invalid_or_unverified_inputs_refuse() {
        let mut b = day("2026-11-10", Timeframe::M1);
        let cut = b.last().unwrap().ts + 60;
        for c in [AssetClass::Crypto, AssetClass::Cmdty, AssetClass::Fx, AssetClass::Govt] {
            assert!(build(&b, Timeframe::M1, None, c, cut, &[], Default::default(), 0.01).is_err());
        }
        b[0].provenance = Provenance::Sample;
        assert!(build(&b, Timeframe::M1, None, AssetClass::Equity, cut, &[], Default::default(), 0.01).is_err());
    }
}
