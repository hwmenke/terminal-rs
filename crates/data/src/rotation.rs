//! Open relative-rotation formula, computed only from verified daily research bars.
use crate::{history::calendar, stats::require_research_bars};
use chrono::{Datelike, NaiveDate};
use std::collections::HashSet;
use tr_core::{
    AssetClass, Bar, Provenance,
    indicators::{ema, sma},
};

pub const MAX_SYMBOLS: usize = 40;
pub const FRAMES: usize = 260;
pub const TAILS: [usize; 5] = [5, 10, 15, 20, 30];
const SECTORS: &str = "XLK XLF XLE XLV XLI XLY XLP XLU XLB XLRE XLC";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cadence {
    Daily,
    Weekly,
}
#[derive(Debug, Clone, Copy)]
pub struct Params {
    pub smooth: usize,
    pub lookback: usize,
    pub momentum: usize,
}
impl Cadence {
    pub fn params(self) -> Params {
        match self {
            Self::Daily => Params { smooth: 5, lookback: 50, momentum: 10 },
            Self::Weekly => Params { smooth: 2, lookback: 10, momentum: 4 },
        }
    }
}
#[derive(Debug, Clone)]
pub struct RotationQuery {
    pub symbols: Vec<String>,
    pub benchmark: String,
    pub cadence: Cadence,
    pub tail: usize,
    pub label: String,
    /// Parser exclusions retained so the footer can explain the cap and asset filter.
    pub skipped: Vec<Skipped>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quadrant {
    Leading,
    Weakening,
    Lagging,
    Improving,
}
impl Quadrant {
    pub fn index(self) -> usize {
        self as usize
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Leading => "Leading",
            Self::Weakening => "Weakening",
            Self::Lagging => "Lagging",
            Self::Improving => "Improving",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub ratio: f32,
    pub momentum: f32,
}
#[derive(Debug, Clone)]
pub struct Track {
    pub symbol: String,
    pub points: Vec<Option<Point>>,
    pub last_date: Option<NaiveDate>,
    pub gaps: usize,
    pub stale_sessions: usize,
}
#[derive(Debug, Clone)]
pub struct Skipped {
    pub symbol: String,
    pub reason: String,
}
#[derive(Debug, Clone)]
pub struct RotationReport {
    pub query: RotationQuery,
    pub params: Params,
    pub dates: Vec<NaiveDate>,
    pub tracks: Vec<Track>,
    pub skipped: Vec<Skipped>,
    pub source: String,
    pub provenance: Provenance,
    pub as_of: NaiveDate,
    pub expected: NaiveDate,
    pub stale_sessions: usize,
    pub week_in_progress: bool,
}
/// Watchlist name is resolved against the UI's groups before passing members here.
pub fn watchlist_group(spec: &str) -> Option<String> {
    let words: Vec<_> = spec.split_whitespace().collect();
    let i = words.iter().position(|s| s.eq_ignore_ascii_case("WL"))?;
    Some(words[i + 1..].iter().take_while(|s| !is_option(s)).copied().collect::<Vec<_>>().join(" "))
}
fn is_option(s: &str) -> bool {
    matches!(s.to_ascii_uppercase().as_str(), "VS" | "DAILY" | "1D" | "WEEKLY" | "1W" | "TAIL" | "SECTORS" | "WL")
}
pub fn parse_query(spec: &str, watchlist: &[String]) -> Result<RotationQuery, String> {
    let words: Vec<_> = spec.split_whitespace().collect();
    let mut q = RotationQuery {
        symbols: vec![],
        benchmark: "SPY".into(),
        cadence: Cadence::Daily,
        tail: 10,
        label: "SECTORS".into(),
        skipped: vec![],
    };
    let mut names = vec![];
    let mut universe = false;
    let mut i = 0;
    while i < words.len() {
        match words[i].to_ascii_uppercase().as_str() {
            "VS" => {
                i += 1;
                q.benchmark = words.get(i).ok_or("VS needs a benchmark")?.to_ascii_uppercase();
            }
            "TAIL" => {
                i += 1;
                let n = words
                    .get(i)
                    .ok_or("TAIL needs a length")?
                    .parse::<usize>()
                    .map_err(|_| "TAIL needs a positive integer")?;
                q.tail = *TAILS.iter().min_by_key(|t| t.abs_diff(n)).unwrap();
            }
            "DAILY" | "1D" => q.cadence = Cadence::Daily,
            "WEEKLY" | "1W" => q.cadence = Cadence::Weekly,
            "SECTORS" => {
                names.extend(SECTORS.split_whitespace().map(str::to_string));
                universe = true;
            }
            "WL" => {
                let start = i + 1;
                while i + 1 < words.len() && !is_option(words[i + 1]) {
                    i += 1;
                }
                let group = words[start..=i].join(" ");
                q.label = if group.is_empty() { "WL (selected)".into() } else { format!("WL {group}") };
                if watchlist.is_empty() {
                    return Err(format!("Rotation refused: empty or unknown watchlist group {}", q.label));
                }
                names.extend(watchlist.iter().cloned());
                universe = true;
            }
            _ => names.push(words[i].to_string()),
        }
        i += 1;
    }
    if names.is_empty() && !universe {
        names.extend(SECTORS.split_whitespace().map(str::to_string));
    } else if !universe {
        q.label = "custom".into();
    }
    let bench = crate::security(&q.benchmark);
    if !matches!(bench.class, AssetClass::Equity | AssetClass::Etf | AssetClass::Index) {
        return Err("Rotation refused: benchmark must be an equity, ETF or index".into());
    }
    q.benchmark = bench.ticker;
    let mut seen = HashSet::new();
    for name in names {
        let sec = crate::security(&name);
        if sec.ticker == q.benchmark || !seen.insert(sec.ticker.clone()) {
            continue;
        }
        let reason = if !matches!(sec.class, AssetClass::Equity | AssetClass::Etf | AssetClass::Index) {
            Some("requires an equity, ETF or index")
        } else if q.symbols.len() >= MAX_SYMBOLS {
            Some("over 40-name cap")
        } else {
            None
        };
        if let Some(reason) = reason {
            q.skipped.push(Skipped { symbol: sec.ticker, reason: reason.into() });
        } else {
            q.symbols.push(sec.ticker);
        }
    }
    Ok(q)
}
pub fn fetch_limit(c: Cadence) -> usize {
    match c {
        Cadence::Daily => 420,
        Cadence::Weekly => 2_000,
    }
}
fn date(b: &Bar) -> NaiveDate {
    chrono::DateTime::from_timestamp(b.ts, 0).unwrap_or_default().date_naive()
}
fn closes(bars: &[Bar], cadence: Cadence) -> Vec<(NaiveDate, f64)> {
    let mut out: Vec<(NaiveDate, f64)> = vec![];
    for b in bars {
        let d = date(b);
        if cadence == Cadence::Weekly && out.last().is_some_and(|(prev, _)| prev.iso_week() == d.iso_week()) {
            *out.last_mut().unwrap() = (d, b.close);
        } else {
            out.push((d, b.close));
        }
    }
    out
}
fn sessions(last: NaiveDate, expected: NaiveDate) -> usize {
    let mut n = 0;
    let mut d = last;
    while d < expected {
        d = d.succ_opt().unwrap();
        n += usize::from(calendar::is_session(d));
    }
    n
}
fn divide(a: f64, b: f64) -> f64 {
    if a.is_finite() && b.is_finite() && b != 0.0 { a / b } else { f64::NAN }
}
pub fn build_report(
    bench: &[Bar],
    named: Vec<(String, Result<Vec<Bar>, String>)>,
    q: &RotationQuery,
    source: &str,
    expected: NaiveDate,
) -> Result<RotationReport, String> {
    require_research_bars(bench, "Rotation benchmark")?;
    let as_of = date(bench.last().unwrap());
    let bc = closes(bench, q.cadence);
    let params = q.cadence.params();
    let need = params.smooth + params.lookback + params.momentum;
    if bc.len() < need {
        return Err(format!("Rotation refused: benchmark needs {need} bars, has {}", bc.len()));
    }
    let offset = bc.len().saturating_sub(FRAMES);
    let dates = bc[offset..].iter().map(|(d, _)| *d).collect::<Vec<_>>();
    let mut report = RotationReport {
        query: q.clone(),
        params,
        dates,
        tracks: vec![],
        skipped: q.skipped.clone(),
        source: source.into(),
        provenance: Bar::provenance(bench),
        as_of,
        expected,
        stale_sessions: sessions(as_of, expected),
        week_in_progress: false,
    };
    if q.cadence == Cadence::Weekly {
        // Last session of this ISO week accounts for Friday holidays as well.
        let mut end = as_of;
        while end.weekday().num_days_from_monday() < 4 {
            end = end.succ_opt().unwrap();
        }
        while !calendar::is_session(end) {
            end = end.pred_opt().unwrap();
        }
        report.week_in_progress = as_of < end;
    }
    let mut seen = HashSet::new();
    for (symbol, bars) in named {
        if !seen.insert(symbol.clone()) {
            continue;
        }
        let result = (|| {
            if report.tracks.len() >= MAX_SYMBOLS {
                return Err("over 40-name cap".into());
            }
            let bars = bars?;
            if bars.is_empty() {
                return Err("not in archive".into());
            }
            require_research_bars(&bars, "Rotation")?;
            let nc = closes(&bars, q.cadence);
            let mut aligned = vec![];
            let mut indices = vec![];
            let mut real_dates = vec![];
            // Both series are sorted: intersect in O(bars), without forward filling.
            let mut next = 0;
            for (i, (d, c)) in bc.iter().enumerate() {
                while next < nc.len() && nc[next].0 < *d {
                    next += 1;
                }
                if let Some((real, close)) = nc.get(next).filter(|(real, _)| real == d) {
                    aligned.push(divide(*close, *c));
                    indices.push(i);
                    real_dates.push(*real);
                }
            }
            if aligned.len() < need {
                return Err(format!("needs {need} bars, has {}", aligned.len()));
            }
            let smoothed = ema(&aligned, params.smooth);
            let base = sma(&smoothed, params.lookback);
            let ratio: Vec<_> = smoothed.iter().zip(base).map(|(a, b)| 100.0 * divide(*a, b)).collect();
            let mom_base = sma(&ratio, params.momentum);
            let mut points = vec![None; report.dates.len()];
            for (j, &i) in indices.iter().enumerate() {
                let momentum = 100.0 * divide(ratio[j], mom_base[j]);
                let p = Point { ratio: ratio[j] as f32, momentum: momentum as f32 };
                if i >= offset && p.ratio.is_finite() && p.momentum.is_finite() {
                    points[i - offset] = Some(p);
                }
            }
            if !points.iter().any(Option::is_some) {
                return Err("no plottable points in frame window".into());
            }
            let first = indices[0].max(offset);
            let last = *indices.last().unwrap();
            let present: HashSet<_> = indices.into_iter().collect();
            let gaps = (first..=last).filter(|i| !present.contains(i)).count();
            let last_date = real_dates.last().copied();
            report.provenance = report.provenance.combine(Bar::provenance(&bars));
            Ok(Track {
                symbol: symbol.clone(),
                points,
                last_date,
                gaps,
                stale_sessions: sessions(last_date.unwrap(), expected),
            })
        })();
        match result {
            Ok(t) => report.tracks.push(t),
            Err(reason) => report.skipped.push(Skipped { symbol, reason }),
        }
    }
    Ok(report)
}
pub fn quadrant(p: Point) -> Quadrant {
    match (p.ratio >= 100.0, p.momentum >= 100.0) {
        (true, true) => Quadrant::Leading,
        (true, false) => Quadrant::Weakening,
        (false, false) => Quadrant::Lagging,
        (false, true) => Quadrant::Improving,
    }
}
/// Clockwise from north: N, NE, E, SE, S, SW, W, NW. Stationary is north.
pub fn heading(prev: Point, cur: Point) -> u8 {
    ((f32::atan2(cur.ratio - prev.ratio, cur.momentum - prev.momentum) / std::f32::consts::FRAC_PI_4).round() as i32)
        .rem_euclid(8) as u8
}
pub fn frames_in_quadrant(track: &Track, at: usize) -> usize {
    let Some(Some(p)) = track.points.get(at) else {
        return 0;
    };
    track.points[..=at].iter().rev().take_while(|v| v.is_some_and(|v| quadrant(v) == quadrant(*p))).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bars(n: usize, f: impl Fn(usize) -> f64) -> Vec<Bar> {
        let start = NaiveDate::from_ymd_opt(2025, 1, 2).unwrap();
        (0..n)
            .map(|i| {
                let c = f(i);
                Bar::new(
                    (start + chrono::Duration::days(i as i64)).and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp(),
                    c,
                    c + 1.0,
                    c * 0.99,
                    c,
                    100.0,
                )
                .with_provenance(Provenance::Real)
            })
            .collect()
    }
    fn report(f: impl Fn(usize) -> f64) -> RotationReport {
        let b = bars(400, |_| 100.0);
        build_report(
            &b,
            vec![("AAPL".into(), Ok(bars(400, f)))],
            &parse_query("AAPL", &[]).unwrap(),
            "unit",
            date(b.last().unwrap()),
        )
        .unwrap()
    }
    #[test]
    fn parse_query_defaults_to_sectors_vs_spy_daily_tail_10() {
        let q = parse_query("", &[]).unwrap();
        assert_eq!(q.symbols.len(), 11);
        assert_eq!(q.benchmark, "SPY");
        assert_eq!(q.cadence, Cadence::Daily);
        assert_eq!(q.tail, 10);
    }
    #[test]
    fn parse_query_reads_vs_weekly_tail_and_watchlist() {
        let q = parse_query("WL Growth VS QQQ WEEKLY TAIL 18", &["AAPL".into()]).unwrap();
        assert_eq!(q.label, "WL Growth");
        assert_eq!(q.cadence, Cadence::Weekly);
        assert_eq!(q.tail, 20);
        assert_eq!(q.benchmark, "QQQ");
        assert_eq!(parse_query("W D", &[]).unwrap().symbols, vec!["W", "D"]);
        assert!(parse_query("WL Missing", &[]).unwrap_err().contains("Missing"));
    }
    #[test]
    fn parse_query_caps_at_forty_and_dedupes() {
        let s = (0..43).map(|i| format!("TEST{i}")).collect::<Vec<_>>().join(" ");
        let q = parse_query(&format!("{s} TEST0 SPY"), &[]).unwrap();
        assert_eq!(q.symbols.len(), 40);
        assert_eq!(q.skipped.len(), 3);
    }
    #[test]
    fn benchmark_against_itself_sits_at_100_100() {
        let bench = bars(400, |i| 100.0 + (i as f64 * 0.13).sin() * 4.0 + i as f64 * 0.2);
        let r = build_report(
            &bench,
            vec![("SPY".into(), Ok(bench.clone()))],
            &parse_query("", &[]).unwrap(),
            "unit",
            date(bench.last().unwrap()),
        )
        .unwrap();
        for p in r.tracks[0].points.iter().flatten() {
            assert!((p.ratio - 100.0).abs() < 1e-4 && (p.momentum - 100.0).abs() < 1e-4);
        }
    }
    #[test]
    fn steady_outperformer_ends_leading_and_steady_underperformer_lagging() {
        for (sign, q) in [(1.0, Quadrant::Leading), (-1.0, Quadrant::Lagging)] {
            let r = report(|i| 100.0 * (sign * 0.00001 * (i * i) as f64).exp());
            assert_eq!(quadrant(r.tracks[0].points.last().unwrap().unwrap()), q);
        }
    }
    #[test]
    fn accelerating_laggard_is_improving_and_fading_leader_is_weakening() {
        for (sign, q) in [(1.0, Quadrant::Improving), (-1.0, Quadrant::Weakening)] {
            let r = report(|i| {
                100.0 * (sign * (-0.005 * i as f64 + 0.00006 * (i.saturating_sub(360).pow(2)) as f64)).exp()
            });
            assert_eq!(quadrant(r.tracks[0].points.last().unwrap().unwrap()), q);
        }
    }
    #[test]
    fn missing_dates_are_gaps_not_forward_fills() {
        let b = bars(400, |_| 100.0);
        let mut n = b.clone();
        n.remove(390);
        let r = build_report(
            &b,
            vec![("AAPL".into(), Ok(n))],
            &parse_query("AAPL", &[]).unwrap(),
            "unit",
            date(b.last().unwrap()),
        )
        .unwrap();
        assert!(r.tracks[0].points[250].is_none());
        assert_eq!(r.tracks[0].gaps, 1);
    }
    #[test]
    fn short_history_is_skipped_with_reason() {
        let b = bars(100, |_| 100.0);
        let r = build_report(
            &b,
            vec![("AAPL".into(), Ok(b[..20].to_vec()))],
            &parse_query("AAPL", &[]).unwrap(),
            "unit",
            date(b.last().unwrap()),
        )
        .unwrap();
        assert_eq!(r.skipped[0].reason, "needs 65 bars, has 20");
    }
    #[test]
    fn sample_or_unverified_bars_are_refused() {
        for provenance in [Provenance::Sample, Provenance::Missing] {
            let b = bars(100, |_| 100.0);
            let n = b.iter().cloned().map(|b| b.with_provenance(provenance)).collect::<Vec<_>>();
            let q = parse_query("AAPL", &[]).unwrap();
            let r =
                build_report(&b, vec![("AAPL".into(), Ok(n.clone()))], &q, "unit", date(b.last().unwrap())).unwrap();
            assert!(r.tracks.is_empty());
            assert!(r.skipped[0].reason.contains("refused"));
            assert!(build_report(&n, vec![], &q, "unit", date(b.last().unwrap())).unwrap_err().contains("refused"));
        }
    }
    #[test]
    fn weekly_uses_last_close_per_iso_week_and_flags_week_in_progress() {
        let b = bars(400, |i| 100.0 + i as f64);
        let c = closes(&b, Cadence::Weekly);
        assert_eq!(c[0], (NaiveDate::from_ymd_opt(2025, 1, 5).unwrap(), 103.0));
        let b = bars(399, |_| 100.0);
        let q = parse_query("AAPL WEEKLY", &[]).unwrap();
        let r = build_report(&b, vec![("AAPL".into(), Ok(b.clone()))], &q, "unit", date(b.last().unwrap())).unwrap();
        assert!(r.week_in_progress);
        assert_eq!(r.params.smooth, 2);
    }
    #[test]
    fn stale_sessions_counts_from_benchmark_last_bar_to_expected() {
        let r = report(|_| 100.0);
        let expected = r.as_of + chrono::Duration::days(7);
        let b = bars(400, |_| 100.0);
        let r = build_report(&b, vec![], &r.query, "unit", expected).unwrap();
        assert_eq!(r.stale_sessions, 5);
    }
    #[test]
    fn frames_in_quadrant_counts_back_to_the_crossing() {
        let mut t = report(|_| 100.0).tracks.remove(0);
        t.points = vec![
            Some(Point { ratio: 99.0, momentum: 101.0 }),
            Some(Point { ratio: 101.0, momentum: 101.0 }),
            Some(Point { ratio: 102.0, momentum: 101.0 }),
        ];
        assert_eq!(frames_in_quadrant(&t, 2), 2);
        assert_eq!(heading(t.points[1].unwrap(), t.points[2].unwrap()), 2);
    }
}
