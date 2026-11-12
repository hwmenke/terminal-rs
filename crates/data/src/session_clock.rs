//! Descriptive weekday × cash-bucket returns. One vote per session
//! and bucket; holes and ambiguous prints never acquire replacement values.
use chrono::{Datelike, NaiveDate};
use tr_core::{Bar, Provenance, Timeframe};

use crate::{
    cash_session,
    closed::closed_len,
    intraday,
    nulls::{SplitMix64, benjamini_hochberg, sign_test},
};

pub const MIN_SAMPLES: usize = 8;
pub const ALPHA: f64 = 0.10;
pub const PERMUTATIONS: usize = 999;
pub const ROWS: [&str; 6] = ["Mon", "Tue", "Wed", "Thu", "Fri", "ALL"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockQuery {
    pub timeframe: Timeframe,
}

pub fn parse_query(spec: &str, tf_field: &str) -> Result<ClockQuery, String> {
    let tokens: Vec<_> = spec.split_whitespace().collect();
    if tokens.len() > 1 {
        return Err("CLOCK takes one bucket size (AAPL CLOCK 1h).".into());
    }
    let token =
        tokens.first().copied().unwrap_or_else(|| if tf_field.trim().is_empty() { "30m" } else { tf_field.trim() });
    let timeframe = token.parse::<Timeframe>()?;
    cash_session::require_bar_size(timeframe, "Session Clock")?;
    Ok(ClockQuery { timeframe })
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClockCell {
    pub n: usize,
    pub mean: Option<f64>,
    pub median: Option<f64>,
    pub hit_rate: Option<f64>,
    pub t_stat: Option<f64>,
    pub p: Option<f64>,
    pub q: Option<f64>,
    pub first: Option<NaiveDate>,
    pub last: Option<NaiveDate>,
}

impl ClockCell {
    pub fn passes(&self, joint_p: Option<f64>) -> bool {
        self.n >= MIN_SAMPLES && self.q.is_some_and(|q| q <= ALPHA) && joint_p.is_some_and(|p| p <= ALPHA)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionClock {
    pub symbol: String,
    pub ticker: String,
    pub timeframe: Timeframe,
    pub source: String,
    pub provenance: Provenance,
    pub date: NaiveDate,
    pub minutes: Vec<u32>,
    pub cells: Vec<Vec<ClockCell>>,
    /// Closed observations for the current NY date, never used by inference.
    pub today: Vec<Option<f64>>,
    pub joint_p: Option<f64>,
    pub tested: usize,
    pub passing: usize,
    pub sessions: usize,
    pub last_ts: i64,
    pub stale: bool,
}

impl SessionClock {
    pub fn joint_passes(&self) -> bool {
        self.joint_p.is_some_and(|p| p <= ALPHA)
    }
}

struct Session {
    date: NaiveDate,
    values: Vec<Option<f64>>,
}

/// `now` chooses TODAY. `observed_at` is the input's observation clock; None
/// leaves the final print pending. Call on data arrival, never from paint.
pub fn build_clock(
    symbol: &str,
    bars: &[Bar],
    query: &ClockQuery,
    source: &str,
    now: i64,
    observed_at: Option<i64>,
) -> Result<SessionClock, String> {
    cash_session::require_bar_size(query.timeframe, "Session Clock")?;
    let sec = crate::security(symbol);
    if symbol.trim().is_empty() || !tr_core::security::looks_like_ticker(&sec.ticker) {
        return Err("CLOCK needs a ticker. Sample prices are never used.".into());
    }
    cash_session::require_us_session(&sec.ticker, None)?;
    if !cash_session::is_cash(&sec) {
        return Err("Session Clock supports US cash sessions only.".into());
    }
    Bar::require_verified(bars, "Session Clock")?;
    if bars.is_empty() {
        return Err("No verified regular-session bars. Sample prices are never used.".into());
    }
    let mut ordered = bars.to_vec();
    ordered.sort_by_key(|b| b.ts);
    let tf = query.timeframe;
    let date = tr_core::ny::civil(now).date();
    let step = (tf.seconds() / 60) as u32;
    let minutes: Vec<_> = (0..390).step_by(step as usize).collect();
    let by_day = cash_session::sessions(tf, &ordered);
    if by_day.is_empty() {
        return Err("No verified regular-session bars. Premarket is not the cash open.".into());
    }
    let closed = closed_len(tf, &ordered, sec.class, observed_at);
    let closed_days = cash_session::sessions(tf, &ordered[..closed]);
    let mut sample = Vec::new();
    for (&day, buckets) in by_day.iter().filter(|(day, _)| **day < date) {
        let mut values = vec![None; minutes.len()];
        for ts in intraday::regular_opens(day, tf) {
            let index = cash_session::minute_offset(ts).unwrap() as usize / step as usize;
            if cash_session::one_print(buckets, ts).is_some() {
                values[index] = closed_days.get(&day).and_then(|day| bucket_return(day, ts));
            }
        }
        if values.iter().any(Option::is_some) {
            sample.push(Session { date: day, values });
        }
    }
    let mut cells = vec![vec![ClockCell::default(); minutes.len()]; ROWS.len()];
    for (row, cells) in cells.iter_mut().enumerate() {
        for (column, cell) in cells.iter_mut().enumerate() {
            let values: Vec<_> = sample
                .iter()
                .filter(|s| row == 5 || s.date.weekday().num_days_from_monday() as usize == row)
                .filter_map(|s| s.values[column].map(|r| (s.date, r)))
                .collect();
            *cell = summarize(&values);
        }
    }
    let tests: Vec<_> = cells
        .iter()
        .flatten()
        .enumerate()
        .filter_map(|(i, c)| (c.n >= MIN_SAMPLES).then(|| c.p.map(|p| (i, p))).flatten())
        .collect();
    let q = benjamini_hochberg(&tests.iter().map(|(_, p)| *p).collect::<Vec<_>>());
    for ((i, _), q) in tests.iter().zip(q) {
        cells[*i / minutes.len()][*i % minutes.len()].q = Some(q);
    }
    let joint_p = joint_test(&sample, &cells[5]);
    let passing = cells.iter().flatten().filter(|c| c.passes(joint_p)).count();
    let closed_today = cash_session::day_buckets(tf, date, &ordered[..closed]);
    // Check ambiguity against ALL prints too: excluding a pending duplicate
    // must not accidentally turn its predecessor into a single valid print.
    let full_today = cash_session::day_buckets(tf, date, &ordered);
    let mut today = vec![None; minutes.len()];
    let close_minute = intraday::close_minute(date);
    let session_end = tr_core::ny::to_utc(date.and_hms_opt(close_minute / 60, close_minute % 60, 0).unwrap());
    let today_through = observed_at.unwrap_or(ordered.last().unwrap().ts).min(now);
    for ts in intraday::regular_opens(date, tf) {
        let end = (ts + tf.seconds()).min(session_end);
        // closed_len uses UTC buckets. This extra bound respects cash H1
        // buckets starting at :30 and the shortened final bucket.
        if end <= today_through && cash_session::one_print(&full_today, ts).is_some() {
            today[cash_session::minute_offset(ts).unwrap() as usize / step as usize] = bucket_return(&closed_today, ts);
        }
    }
    let last_cash = by_day.values().flat_map(|b| b.keys()).max().copied().unwrap();
    let expected = intraday::regular_opens(date, tf).into_iter().filter(|ts| *ts <= now).next_back();
    let latest_due = if let Some(ts) = expected {
        Some(ts)
    } else {
        let prior = crate::history::calendar::last_expected_session(chrono::DateTime::from_timestamp(now, 0).unwrap());
        intraday::regular_opens(prior, tf).last().copied()
    };
    let stale = observed_at.is_none() || latest_due.is_some_and(|due| last_cash < due);
    Ok(SessionClock {
        symbol: sec.key(),
        ticker: sec.ticker,
        timeframe: tf,
        source: source.into(),
        provenance: Bar::provenance(bars),
        date,
        minutes,
        cells,
        today,
        joint_p,
        tested: tests.len(),
        passing,
        sessions: sample.len(),
        last_ts: last_cash,
        stale,
    })
}

fn bucket_return(day: &std::collections::BTreeMap<i64, Vec<&Bar>>, ts: i64) -> Option<f64> {
    let bar = cash_session::one_print(day, ts).filter(|b| cash_session::usable_range(b))?;
    let value = (bar.close / bar.open - 1.0) * 10_000.0;
    value.is_finite().then_some(value)
}

fn summarize(values: &[(NaiveDate, f64)]) -> ClockCell {
    let n = values.len();
    if n == 0 {
        return ClockCell::default();
    }
    let mean = values.iter().map(|(_, r)| r).sum::<f64>() / n as f64;
    let positive = values.iter().filter(|(_, r)| *r > 0.0).count();
    let negative = values.iter().filter(|(_, r)| *r < 0.0).count();
    let mut sorted: Vec<_> = values.iter().map(|(_, r)| *r).collect();
    sorted.sort_by(f64::total_cmp);
    let median = (sorted[(n - 1) / 2] + sorted[n / 2]) / 2.0;
    let se = if n > 1 {
        (values.iter().map(|(_, r)| (r - mean).powi(2)).sum::<f64>() / (n - 1) as f64 / n as f64).sqrt()
    } else {
        0.0
    };
    ClockCell {
        n,
        mean: Some(mean),
        median: Some(median),
        hit_rate: Some(positive as f64 / n as f64),
        t_stat: (se > 0.0).then(|| mean / se),
        p: Some(sign_test(positive, negative).unwrap_or(1.0)),
        q: None,
        first: values.first().map(|v| v.0),
        last: values.last().map(|v| v.0),
    }
}

fn variance(sums: &[f64], counts: &[usize], eligible: &[usize]) -> f64 {
    let mean = eligible.iter().map(|&j| sums[j] / counts[j] as f64).sum::<f64>() / eligible.len() as f64;
    eligible.iter().map(|&j| (sums[j] / counts[j] as f64 - mean).powi(2)).sum::<f64>() / eligible.len() as f64
}

fn joint_test(sample: &[Session], all: &[ClockCell]) -> Option<f64> {
    let eligible: Vec<_> = all.iter().enumerate().filter_map(|(j, c)| (c.n > 0).then_some(j)).collect();
    if eligible.len() < 2 {
        return None;
    }
    let counts: Vec<_> = all.iter().map(|c| c.n).collect();
    let mut observed_sums = vec![0.0; all.len()];
    for s in sample {
        for (j, value) in s.values.iter().enumerate() {
            if let Some(value) = value {
                observed_sums[j] += value;
            }
        }
    }
    let observed = variance(&observed_sums, &counts, &eligible);
    let mut rng = SplitMix64(
        sample.first()?.date.num_days_from_ce() as u64
            ^ (sample.len() as u64).rotate_left(17)
            ^ (sample.last()?.date.num_days_from_ce() as u64).rotate_left(33)
            ^ all.len() as u64,
    );
    let mut extreme = 0;
    // Permute only observed slots in each session: holes remain holes and
    // early-close sessions cannot contribute to afternoon columns.
    let mut slots: Vec<_> = sample
        .iter()
        .map(|s| s.values.iter().enumerate().filter_map(|(j, r)| r.map(|r| (j, r))).collect::<Vec<_>>())
        .collect();
    let mut sums = vec![0.0; all.len()];
    for _ in 0..PERMUTATIONS {
        sums.fill(0.0);
        for session in &mut slots {
            for i in (1..session.len()).rev() {
                let j = rng.below(i + 1);
                let value = session[i].1;
                session[i].1 = session[j].1;
                session[j].1 = value;
            }
            for &(j, value) in session.iter() {
                sums[j] += value;
            }
        }
        if variance(&sums, &counts, &eligible) >= observed {
            extreme += 1;
        }
    }
    Some((1 + extreme) as f64 / (PERMUTATIONS + 1) as f64)
}

/// Wall-clock display only; this never participates in inference.
pub fn now_column(tf: Timeframe, now: i64) -> Option<usize> {
    let bucket = intraday::regular_bucket(tf, now)?;
    Some(cash_session::minute_offset(bucket)? as usize / (tf.seconds() / 60) as usize)
}

pub fn minute_label(minute: u32) -> String {
    let minute = intraday::OPEN_MIN + minute;
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn dates(n: usize) -> Vec<NaiveDate> {
        let mut date = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let mut out = Vec::new();
        while out.len() < n {
            if crate::history::calendar::is_session(date) {
                out.push(date);
            }
            date += Duration::days(1);
        }
        out
    }
    fn at(date: NaiveDate, hour: u32, minute: u32) -> i64 {
        tr_core::ny::to_utc(date.and_hms_opt(hour, minute, 0).unwrap())
    }
    fn bar(ts: i64, bp: f64) -> Bar {
        let close = 100.0 * (1.0 + bp / 10_000.0);
        Bar::new(ts, 100.0, close.max(100.0), close.min(100.0), close, 100.0).with_provenance(Provenance::Real)
    }
    fn fixture(n: usize, seed: u64, drift: bool) -> (Vec<Bar>, i64) {
        let days = dates(n + 1);
        let mut rng = SplitMix64(seed);
        let mut bars = Vec::new();
        for date in &days[..n] {
            for (j, ts) in intraday::regular_opens(*date, Timeframe::M30).into_iter().enumerate() {
                let noise = (rng.next() >> 11) as f64 / (1_u64 << 53) as f64 * 10.0 - 5.0;
                bars.push(bar(ts, noise + if drift && j == 1 { 20.0 } else { 0.0 }));
            }
        }
        (bars, at(days[n], 9, 45))
    }
    fn report(bars: &[Bar], now: i64) -> SessionClock {
        build_clock("AAPL", bars, &ClockQuery { timeframe: Timeframe::M30 }, "unit", now, Some(now)).unwrap()
    }

    #[test]
    fn seeded_noise_rarely_colours_and_joint_banner_fires() {
        let mut coloured = 0;
        let mut tested = 0;
        let mut banners = 0;
        for seed in 0..32 {
            let (bars, now) = fixture(80, seed, false);
            let r = report(&bars, now);
            coloured += r.passing;
            tested += r.tested;
            banners += usize::from(!r.joint_passes());
            if !r.joint_passes() {
                assert_eq!(r.passing, 0);
            }
            assert!(r.cells.iter().flatten().filter(|c| c.passes(r.joint_p)).all(|c| c.q.unwrap() <= ALPHA));
        }
        assert!(coloured as f64 / tested as f64 <= 0.10, "{coloured}/{tested}");
        assert!(banners >= 24, "only {banners} noise banners");
    }

    #[test]
    fn planted_ten_oclock_drift_is_found_and_deterministic() {
        let (bars, now) = fixture(80, 42, true);
        let r = report(&bars, now);
        assert!(r.joint_passes(), "{:?}", r.joint_p);
        assert!(r.cells[5][1].passes(r.joint_p));
        assert!(r.cells[..5].iter().all(|row| row[1].passes(r.joint_p)));
        assert_eq!(r, report(&bars, now));
        assert_eq!(r.minutes.len(), 13);
        assert_eq!(minute_label(r.minutes[1]), "10:00");
        assert_eq!(r.tested, 78);
    }

    #[test]
    fn today_never_votes_and_forming_or_stale_bucket_never_prints() {
        let (mut bars, now) = fixture(30, 1, false);
        let prior = report(&bars, now);
        let today = tr_core::ny::civil(now).date();
        bars.push(bar(at(today, 9, 30), 999.0));
        let r = report(&bars, now);
        assert_eq!(r.cells, prior.cells);
        assert_eq!(r.joint_p, prior.joint_p);
        assert!(r.today.iter().all(Option::is_none));
        let closed = report(&bars, at(today, 10, 0));
        assert!(closed.today[0].is_some());
        assert!(closed.today[1].is_none());
        let stale =
            build_clock("AAPL", &bars, &ClockQuery { timeframe: Timeframe::M30 }, "unit", at(today, 11, 0), Some(now))
                .unwrap();
        assert!(stale.today[0].is_none());
        assert!(stale.stale);
        // Even future prints in a payload cannot expose a forming current bucket.
        bars.push(bar(at(today, 10, 0), 123.0));
        bars.push(bar(at(today, 10, 30), 456.0));
        let partial = report(&bars, at(today, 10, 15));
        assert!(partial.today[0].is_some());
        assert!(partial.today[1..].iter().all(Option::is_none));
        assert_eq!(partial.cells, prior.cells);
    }

    #[test]
    fn cached_successors_close_earlier_buckets_but_the_last_print_stays_pending() {
        let date = dates(1)[0];
        let mut bars = vec![bar(at(date, 9, 30), 1.0), bar(at(date, 10, 0), 2.0), bar(at(date, 10, 30), 3.0)];
        for b in &mut bars {
            b.provenance = Provenance::Cached;
        }
        let now = at(date, 12, 0);
        let query = ClockQuery { timeframe: Timeframe::M30 };
        let r = build_clock("AAPL", &bars, &query, "unit", now, None).unwrap();
        assert!(r.today[..2].iter().all(Option::is_some));
        assert!(r.today[2..].iter().all(Option::is_none));
        assert!(r.stale);
        // Advancing wall time cannot admit the frozen final print to inference.
        let r = build_clock("AAPL", &bars, &query, "unit", now + 86400, None).unwrap();
        assert_eq!(r.cells[5][0].n, 1);
        assert_eq!(r.cells[5][1].n, 1);
        assert_eq!(r.cells[5][2].n, 0);
        // A second pending print cannot resolve ambiguity in the closed prefix.
        bars.push(bars[2]);
        let r = build_clock("AAPL", &bars, &query, "unit", now + 86400, None).unwrap();
        assert_eq!(r.cells[5][2].n, 0);
    }

    #[test]
    fn cash_hour_bucket_does_not_close_at_utc_hour_and_final_half_hour_closes() {
        let date = dates(1)[0];
        let query = ClockQuery { timeframe: Timeframe::H1 };
        let bars = vec![bar(at(date, 9, 30), 10.0)];
        let now = at(date, 10, 0);
        let r = build_clock("AAPL", &bars, &query, "unit", now, Some(now)).unwrap();
        assert_eq!(r.today[0], None);
        let now = at(date, 10, 30);
        assert!(build_clock("AAPL", &bars, &query, "unit", now, Some(now)).unwrap().today[0].is_some());
        let bars = vec![bar(at(date, 15, 30), 10.0)];
        let now = at(date, 16, 0);
        let r = build_clock("AAPL", &bars, &query, "unit", now, Some(now)).unwrap();
        assert!(r.today[6].is_some());
    }

    #[test]
    fn duplicate_missing_and_unusable_prints_do_not_vote_or_fill_holes() {
        let (bars, now) = fixture(20, 3, false);
        let full = report(&bars, now);
        let mut bad = bars.clone();
        bad.remove(1); // Missing 10:00 on just one day.
        let mut dup = bad[0];
        dup.ts += 1;
        bad.push(dup); // Same 09:30 bucket, despite distinct timestamps.
        bad[2].open = 0.0;
        let r = report(&bad, now);
        assert_eq!(r.cells[5][0].n, full.cells[5][0].n - 1);
        assert_eq!(r.cells[5][1].n, full.cells[5][1].n - 1);
        assert_eq!(r.cells[5][3].n, full.cells[5][3].n - 1);
        assert_eq!(r.cells[5][2].n, full.cells[5][2].n);
        assert_eq!(r.cells[5][12].n, full.cells[5][12].n);
        let today = tr_core::ny::civil(now).date();
        let mut duplicate_today = bars;
        duplicate_today.extend([bar(at(today, 9, 30), 1.0), bar(at(today, 9, 30), 2.0)]);
        let at_close = at(today, 10, 0);
        assert!(report(&duplicate_today, at_close).today.iter().all(Option::is_none));
    }

    #[test]
    fn early_close_votes_only_on_its_own_cash_grid() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 27).unwrap();
        let bars: Vec<_> = intraday::regular_opens(date, Timeframe::M30).into_iter().map(|ts| bar(ts, 10.0)).collect();
        assert_eq!(bars.len(), 7);
        let r = report(&bars, at(date + Duration::days(3), 10, 0));
        assert_eq!(r.cells[5][6].n, 1);
        assert_eq!(r.cells[5][7].n, 0);
        assert_eq!(r.cells[5][7].mean, None);
        assert!(r.cells[5].iter().all(|c| c.q.is_none()));
        let r = report(&bars, at(date, 13, 0));
        assert!(r.today[..7].iter().all(Option::is_some));
        assert!(r.today[7..].iter().all(Option::is_none));
        assert_eq!(now_column(Timeframe::M30, at(date, 13, 0)), None);
    }

    #[test]
    fn samples_unverified_mixed_empty_and_daily_are_refused() {
        let (bars, now) = fixture(10, 8, false);
        for provenance in [Provenance::Sample, Provenance::Missing] {
            let mut input = bars.clone();
            input[0].provenance = provenance;
            assert!(
                build_clock("AAPL", &input, &ClockQuery { timeframe: Timeframe::M30 }, "unit", now, Some(now)).is_err()
            );
        }
        assert!(build_clock("AAPL", &[], &ClockQuery { timeframe: Timeframe::M30 }, "unit", now, Some(now)).is_err());
        assert!(build_clock("AAPL", &bars, &ClockQuery { timeframe: Timeframe::D1 }, "unit", now, Some(now)).is_err());
        assert!(
            build_clock("BTCUSD", &bars, &ClockQuery { timeframe: Timeframe::M30 }, "unit", now, Some(now)).is_err()
        );
        for token in ["1d", "1w", "4h", "30m 1h"] {
            assert!(parse_query(token, "").is_err());
        }
        assert_eq!(parse_query("", "").unwrap().timeframe, Timeframe::M30);
        assert_eq!(parse_query("1h", "30m").unwrap().timeframe, Timeframe::H1);
    }

    #[test]
    fn zeros_drop_from_sign_test_and_low_n_is_never_filled() {
        let days = dates(10);
        let observations: Vec<_> = days.iter().enumerate().map(|(i, d)| (*d, if i < 9 { 1.0 } else { 0.0 })).collect();
        let c = summarize(&observations);
        assert!((c.p.unwrap() - 0.00390625).abs() < 1e-12);
        assert_eq!(c.n, 10);
        assert_eq!(c.hit_rate, Some(0.9));
        assert_eq!(summarize(&[(days[0], 0.0); 8]).p, Some(1.0));
        let c = ClockCell { n: 7, q: Some(0.001), ..Default::default() };
        assert!(!c.passes(Some(0.001)));
        let c = ClockCell { n: 8, q: Some(0.001), ..Default::default() };
        assert!(!c.passes(Some(0.2)));
        assert!(!c.passes(None));
        assert!(c.passes(Some(0.001)));
    }
}
