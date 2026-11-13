//! Verified calendar seasonality. Returns are fractions, never annualised estimates.
use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, NaiveDate, Timelike, Utc};
use tr_core::{Bar, Provenance, Timeframe};

use crate::history::calendar::{is_session, last_expected_session, next_session};
use crate::intraday::{close_minute, regular_bucket, regular_opens};
use crate::stats::{ReturnStats, describe, require_research_bars};

pub const MIN_N: usize = 3;
pub const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
pub const WEEKDAYS: [&str; 5] = ["Mon", "Tue", "Wed", "Thu", "Fri"];

#[derive(Debug, Clone, PartialEq)]
pub struct Bucket {
    pub label: String,
    pub stats: ReturnStats,
    /// Descriptive mean / sample standard error; undefined at zero variance.
    pub t_stat: Option<f64>,
}

fn bucket(label: impl Into<String>, values: &[f64]) -> Bucket {
    let stats = describe(values, 1);
    let t_stat = stats.std.filter(|s| *s > 0.0).zip(stats.mean).map(|(s, m)| m / s * (stats.count as f64).sqrt());
    Bucket { label: label.into(), stats, t_stat }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MonthCell {
    pub year: i32,
    pub month: u32,
    /// Number of observed daily returns, including the prior month close anchor.
    pub n: usize,
    pub expected: usize,
    /// None for partial months or any missing daily return.
    pub value: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClockBucket {
    pub minute: u32,
    pub returns: Bucket,
    pub absolute: Bucket,
    pub volume_share: Bucket,
    pub weekdays: Vec<Bucket>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ForwardCompare {
    pub horizon: usize,
    pub month: ReturnStats,
    pub weekday: ReturnStats,
    pub baseline: ReturnStats,
    /// Actual completed outcomes, retained for distribution charts.
    pub month_values: Vec<f64>,
    pub weekday_values: Vec<f64>,
    pub baseline_values: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeasonReport {
    pub symbol: String,
    pub timeframe: Timeframe,
    pub provenance: Provenance,
    pub source: String,
    pub observed_at: i64,
    pub start: String,
    pub end: String,
    pub bars: usize,
    pub current_month: u32,
    pub current_weekday: usize,
    pub months: Vec<MonthCell>,
    pub month_profile: Vec<Bucket>,
    pub weekdays: Vec<Bucket>,
    pub turn_of_month: Vec<Bucket>,
    pub weeks: Vec<Bucket>,
    pub clock: Vec<ClockBucket>,
    pub forwards: Vec<ForwardCompare>,
}

pub fn parse_timeframe(spec: &str) -> Result<Timeframe, String> {
    let tf = if spec.trim().is_empty() { Timeframe::D1 } else { spec.trim().parse::<Timeframe>()? };
    if matches!(tf, Timeframe::D1 | Timeframe::M5 | Timeframe::M15 | Timeframe::M30 | Timeframe::H1) {
        Ok(tf)
    } else {
        Err("SEASON supports daily, 5m, 15m, 30m or 1h bars".into())
    }
}

fn date(ts: i64) -> Result<NaiveDate, String> {
    DateTime::<Utc>::from_timestamp(ts, 0)
        .map(|d| d.date_naive())
        .ok_or_else(|| "SEASON refused: invalid timestamp".into())
}

fn month_sessions(d: NaiveDate) -> Vec<NaiveDate> {
    let mut day = d.with_day(1).expect("valid date");
    let mut out = Vec::new();
    while day.month() == d.month() {
        if is_session(day) {
            out.push(day);
        }
        let Some(next) = day.succ_opt() else { break };
        day = next;
    }
    out
}

/// A deterministic report at an explicit observation clock. All inputs pass the
/// research gate before even pending/out-of-session bars are removed.
pub fn compute(symbol: &str, tf: Timeframe, input: &[Bar], source: &str, now: i64) -> Result<SeasonReport, String> {
    compute_at(symbol, tf, input, source, now, now)
}

/// Separate the last source observation from the current calendar clock. A stale
/// vault cannot close a pending bar merely because the user reopens the pane.
pub fn compute_at(
    symbol: &str,
    tf: Timeframe,
    input: &[Bar],
    source: &str,
    observed: i64,
    now: i64,
) -> Result<SeasonReport, String> {
    parse_timeframe(tf.label())?;
    require_research_bars(input, "Seasonality")?;
    let provenance = Bar::provenance(input);
    provenance.require_verified("Seasonality")?;
    let current = DateTime::<Utc>::from_timestamp(observed, 0).ok_or("SEASON refused: invalid observation clock")?;
    date(now)?;
    let today = tr_core::ny::civil(now).date();
    let mut bars = Vec::new();
    let mut dates = Vec::new();
    for b in input {
        let d = if tf == Timeframe::D1 {
            date(b.ts)?
        } else {
            date(b.ts)?;
            tr_core::ny::civil(b.ts).date()
        };
        if tf == Timeframe::D1 {
            if !is_session(d) {
                return Err(format!("SEASON refused: daily timestamp {d} is not a US cash session"));
            }
            if dates.last() == Some(&d) {
                return Err("SEASON refused: duplicate daily session".into());
            }
            if d > last_expected_session(current) {
                continue;
            }
        } else {
            if !crate::intraday::in_regular_session(b.ts) {
                continue;
            }
            if regular_bucket(tf, b.ts) != Some(b.ts) {
                return Err("SEASON refused: intraday timestamps must match the New York session grid".into());
            }
            let close = close_minute(d);
            let end = tr_core::ny::to_utc(d.and_hms_opt(close / 60, close % 60, 0).unwrap());
            if (b.ts + tf.seconds()).min(end) > observed {
                continue;
            }
        }
        if b.volume < 0.0 {
            return Err("SEASON refused: negative volume".into());
        }
        bars.push(*b);
        dates.push(d);
    }
    if bars.len() < 2 {
        return Err("SEASON needs at least two verified closed bars; sample/display prices are never used.".into());
    }
    // A break in the expected session sequence invalidates crossing returns.
    let mut breaks = vec![0usize; bars.len()];
    for i in 1..bars.len() {
        let adjacent = if tf == Timeframe::D1 {
            next_session(dates[i - 1]) == dates[i]
        } else {
            let opens = regular_opens(dates[i - 1], tf);
            let next = opens
                .into_iter()
                .find(|t| *t > bars[i - 1].ts)
                .or_else(|| regular_opens(next_session(dates[i - 1]), tf).first().copied());
            next == Some(bars[i].ts)
        };
        breaks[i] = breaks[i - 1] + usize::from(!adjacent);
    }
    let returns: Vec<Option<f64>> = (0..bars.len())
        .map(|i| {
            let r = if tf.is_intraday() {
                Some(bars[i].close / bars[i].open - 1.0)
            } else if i > 0 && breaks[i] == breaks[i - 1] {
                Some(bars[i].close / bars[i - 1].close - 1.0)
            } else {
                None
            };
            r.filter(|r| r.is_finite())
        })
        .collect();
    if returns.iter().flatten().count() == 0 {
        return Err("SEASON has no adjacent completed daily returns".into());
    }
    let mut weekdays = vec![Vec::new(); 5];
    let mut weeks = vec![Vec::new(); 5];
    let mut turn = vec![Vec::new(); 2];
    let mut month_data: BTreeMap<(i32, u32), Vec<usize>> = BTreeMap::new();
    for (i, d) in dates.iter().enumerate() {
        if let Some(r) = returns[i] {
            weekdays[d.weekday().num_days_from_monday() as usize].push(r);
            weeks[((d.day() - 1) / 7) as usize].push(r);
            let sessions = month_sessions(*d);
            let rank = sessions.iter().position(|day| day == d).unwrap();
            turn[usize::from(rank >= 3 && rank + 3 < sessions.len())].push(r);
        }
        if tf == Timeframe::D1 {
            month_data.entry((d.year(), d.month())).or_default().push(i);
        }
    }
    let mut months = Vec::new();
    let mut monthly = vec![Vec::new(); 12];
    for ((year, month), rows) in month_data {
        let expected = month_sessions(dates[rows[0]]).len();
        let n = rows.iter().filter(|i| returns[**i].is_some()).count();
        let value = if n == expected {
            let first = rows[0];
            Some(bars[first + expected - 1].close / bars[first - 1].close - 1.0).filter(|r| r.is_finite())
        } else {
            None
        };
        if let Some(r) = value {
            monthly[month as usize - 1].push(r);
        }
        months.push(MonthCell { year, month, n, expected, value });
    }
    let mut clock = Vec::new();
    if tf.is_intraday() {
        let mut sessions: BTreeMap<NaiveDate, Vec<usize>> = BTreeMap::new();
        for (i, d) in dates.iter().enumerate() {
            sessions.entry(*d).or_default().push(i);
        }
        let mut volumes = vec![None; bars.len()];
        for (d, indices) in sessions {
            let expected = regular_opens(d, tf);
            if indices.len() == expected.len() && indices.iter().zip(&expected).all(|(i, ts)| bars[*i].ts == *ts) {
                let total: f64 = indices.iter().map(|i| bars[*i].volume).sum();
                if total > 0.0 && total.is_finite() {
                    for i in indices {
                        volumes[i] = Some(bars[i].volume / total);
                    }
                }
            }
        }
        // Include every clock bucket, even if no observations exist.
        for minute in (570..960).step_by((tf.seconds() / 60) as usize) {
            let mut rs = Vec::new();
            let mut vs = Vec::new();
            let mut days = vec![Vec::new(); 5];
            for (i, b) in bars.iter().enumerate() {
                let wall = tr_core::ny::civil(b.ts);
                if wall.hour() * 60 + wall.minute() != minute {
                    continue;
                }
                if let Some(r) = returns[i] {
                    rs.push(r);
                    days[dates[i].weekday().num_days_from_monday() as usize].push(r);
                }
                if let Some(v) = volumes[i] {
                    vs.push(v);
                }
            }
            let label = format!("{:02}:{:02}", minute / 60, minute % 60);
            clock.push(ClockBucket {
                minute,
                returns: bucket(&label, &rs),
                absolute: bucket(&label, &rs.iter().map(|r| r.abs()).collect::<Vec<_>>()),
                volume_share: bucket(&label, &vs),
                weekdays: days.iter().zip(WEEKDAYS).map(|(v, l)| bucket(l, v)).collect(),
            });
        }
    }
    let mut forwards = Vec::new();
    for h in [5, 10, 20] {
        let mut baseline = Vec::new();
        let mut month = Vec::new();
        let mut weekday = Vec::new();
        for i in 0..bars.len().saturating_sub(h) {
            if breaks[i] != breaks[i + h] {
                continue;
            }
            let r = bars[i + h].close / bars[i].close - 1.0;
            if !r.is_finite() {
                continue;
            }
            baseline.push(r);
            if dates[i].month() == today.month() {
                month.push(r);
            }
            if dates[i].weekday() == today.weekday() {
                weekday.push(r);
            }
        }
        forwards.push(ForwardCompare {
            horizon: h,
            month: describe(&month, h),
            weekday: describe(&weekday, h),
            baseline: describe(&baseline, h),
            month_values: month,
            weekday_values: weekday,
            baseline_values: baseline,
        });
    }
    Ok(SeasonReport {
        symbol: symbol.into(),
        timeframe: tf,
        provenance,
        source: source.into(),
        observed_at: observed,
        start: dates[0].to_string(),
        end: dates[dates.len() - 1].to_string(),
        bars: bars.len(),
        current_month: today.month(),
        current_weekday: today.weekday().num_days_from_monday() as usize,
        months,
        month_profile: monthly.iter().zip(MONTHS).map(|(v, l)| bucket(l, v)).collect(),
        weekdays: weekdays.iter().zip(WEEKDAYS).map(|(v, l)| bucket(l, v)).collect(),
        weeks: weeks.iter().enumerate().map(|(i, v)| bucket(format!("W{}", i + 1), v)).collect(),
        turn_of_month: turn.iter().zip(["Last 3 + first 3", "Rest"]).map(|(v, l)| bucket(l, v)).collect(),
        clock,
        forwards,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }
    fn real(ts: i64, c: f64) -> Bar {
        Bar::new(ts, 100.0, 200.0, 50.0, c, 10.0).with_provenance(Provenance::Real)
    }
    fn daily() -> Vec<Bar> {
        let mut day = d("2025-12-31");
        let mut out = Vec::new();
        while day <= d("2026-02-27") {
            out.push(real(
                day.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp(),
                100.0 * 1.01_f64.powi(out.len() as i32),
            ));
            day = next_session(day);
        }
        out
    }
    fn now() -> i64 {
        d("2026-03-02").and_hms_opt(22, 0, 0).unwrap().and_utc().timestamp()
    }
    fn report(b: &[Bar]) -> Result<SeasonReport, String> {
        compute("X", Timeframe::D1, b, "test", now())
    }
    #[test]
    fn known_months_profiles_and_completed_forward_returns() {
        let b = daily();
        let r = report(&b).unwrap();
        let jan = r.months.iter().find(|c| c.month == 1).unwrap();
        assert_eq!(jan.n, 20);
        assert!((jan.value.unwrap() - (1.01_f64.powi(20) - 1.0)).abs() < 1e-12);
        assert_eq!(r.month_profile[0].stats.count, 1);
        assert_eq!(r.month_profile[0].stats.win_rate, Some(1.0));
        assert!(r.weekdays.iter().all(|b| (b.stats.mean.unwrap() - 0.01).abs() < 1e-12));
        assert_eq!(r.forwards[0].baseline.count, b.len() - 5);
        assert!((r.forwards[0].baseline.mean.unwrap() - (1.01_f64.powi(5) - 1.0)).abs() < 1e-12);
        assert_eq!(r.forwards[0].baseline.effective_count, (b.len() - 5) as f64 / 5.0);
        assert_eq!(r.turn_of_month.iter().map(|b| b.stats.count).sum::<usize>(), b.len() - 1);
        assert_eq!(r.turn_of_month[0].stats.count, 12);
    }
    #[test]
    fn incomplete_months_and_missing_sessions_are_not_extrapolated() {
        let mut b = daily();
        b.remove(5);
        let r = report(&b).unwrap();
        assert!(r.months.iter().find(|c| c.month == 1).unwrap().value.is_none());
        assert_eq!(r.month_profile[0].stats.count, 0);
        assert!(r.forwards[0].baseline.count < b.len() - 5);
        assert!(report(&daily()[..10]).unwrap().months.iter().all(|m| m.value.is_none()));
    }
    #[test]
    fn refuses_unverified_mixed_nan_duplicate_and_too_few() {
        let b = daily();
        assert!(report(&[]).is_err());
        assert!(report(&b[..1]).is_err());
        for p in [Provenance::Sample, Provenance::Missing] {
            let mut bad = b.clone();
            bad[4].provenance = p;
            assert!(report(&bad).is_err());
        }
        let mut bad = b.clone();
        bad[4].close = f64::NAN;
        assert!(report(&bad).is_err());
        let mut bad = b.clone();
        bad[4].ts = bad[3].ts;
        assert!(report(&bad).is_err());
        let mut bad = b;
        bad[4].ts = bad[3].ts + 1;
        assert!(report(&bad).is_err());
    }
    #[test]
    fn summary_matches_sample_t_stat_and_interpolated_median() {
        let b = bucket("known", &[-0.01, 0.01, 0.03]);
        assert!((b.stats.mean.unwrap() - 0.01).abs() < 1e-12);
        assert!((b.stats.median.unwrap() - 0.01).abs() < 1e-12);
        assert!((b.stats.std.unwrap() - 0.02).abs() < 1e-12);
        assert!((b.t_stat.unwrap() - 3.0_f64.sqrt() / 2.0).abs() < 1e-12);
        assert_eq!(b.stats.win_rate, Some(2.0 / 3.0));
        assert!(bucket("one", &[0.01]).t_stat.is_none());
    }
    #[test]
    fn stale_observation_does_not_change_current_calendar_condition() {
        let b = daily();
        let observation = d("2026-02-10").and_hms_opt(22, 0, 0).unwrap().and_utc().timestamp();
        let r = compute_at("X", Timeframe::D1, &b, "test", observation, now()).unwrap();
        assert_eq!(r.end, "2026-02-10");
        assert_eq!(r.current_month, 3);
        assert_eq!(r.current_weekday, 0);
        assert_eq!(r.forwards[0].month.count, 0);
        assert_eq!(r.observed_at, observation);
    }
    #[test]
    fn flat_returns_have_no_t_stat() {
        let mut b = daily();
        for bar in &mut b {
            bar.close = 100.0;
        }
        let r = report(&b).unwrap();
        assert_eq!(r.weekdays[0].stats.mean, Some(0.0));
        assert_eq!(r.weekdays[0].stats.std, Some(0.0));
        assert_eq!(r.weekdays[0].t_stat, None);
    }
    #[test]
    fn clock_uses_ny_dst_grid_complete_volume_and_closed_bars() {
        let mut b = Vec::new();
        for day in [d("2026-03-06"), d("2026-03-09")] {
            for ts in regular_opens(day, Timeframe::M30) {
                b.push(real(ts, 101.0));
            }
        }
        let now = b.last().unwrap().ts + 1800;
        let r = compute("X", Timeframe::M30, &b, "test", now).unwrap();
        assert_eq!(b[13].ts - b[0].ts, 3 * 86400 - 3600);
        assert_eq!(r.clock[0].minute, 570);
        assert_eq!(r.clock[0].returns.stats.count, 2);
        assert!((r.clock[0].returns.stats.mean.unwrap() - 0.01).abs() < 1e-12);
        assert!((r.clock.iter().map(|c| c.volume_share.stats.mean.unwrap()).sum::<f64>() - 1.0).abs() < 1e-12);
        let pending = compute("X", Timeframe::M30, &b, "test", now - 1).unwrap();
        assert_eq!(pending.bars, b.len() - 1);
        assert_eq!(pending.clock[0].volume_share.stats.count, 1);
        b[1].ts += 1;
        assert!(compute("X", Timeframe::M30, &b, "test", now).unwrap_err().contains("grid"));
    }
    #[test]
    fn hour_final_half_bar_and_zero_volume_are_honest() {
        let b: Vec<_> = regular_opens(d("2026-11-27"), Timeframe::H1)
            .into_iter()
            .map(|ts| {
                let mut b = real(ts, 100.0);
                b.volume = 0.0;
                b
            })
            .collect();
        let close = tr_core::ny::to_utc(d("2026-11-27").and_hms_opt(13, 0, 0).unwrap());
        let r = compute("X", Timeframe::H1, &b, "test", close).unwrap();
        assert_eq!(r.bars, 4);
        assert!(r.clock.iter().all(|c| c.volume_share.stats.mean.is_none()));
        assert!(parse_timeframe("4h").is_err());
        assert!(parse_timeframe("30M").is_ok());
    }
}
