//! Analysis-period windowing: limits, warm-up exclusion, cache identity.
use super::*;
use std::sync::Arc;
use tr_core::Provenance;

fn day(y: i32, m: u32, d: u32) -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap()
}
fn midnight(d: chrono::NaiveDate) -> i64 {
    d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp()
}
/// Fixture only: verified-tagged daily AAPL bars on real calendar sessions.
fn daily(start: chrono::NaiveDate, n: usize, skip: &[usize]) -> Vec<Bar> {
    let mut out = vec![];
    let mut d = start;
    for k in 0..n {
        if !skip.contains(&k) {
            let c = 100. + 8. * ((k as f64) * 0.11).sin() + (k % 5) as f64 * 0.2 + k as f64 * 0.0005;
            out.push(Bar {
                ts: midnight(d),
                open: c,
                high: c * 1.002,
                low: c * 0.998,
                close: c,
                volume: 1000.,
                provenance: Provenance::Real,
            });
        }
        d = crate::history::calendar::next_session(d);
    }
    out
}
fn input(bars: Vec<Bar>) -> CanvasInput {
    let observed = bars.last().unwrap().ts + 3 * 86_400;
    CanvasInput {
        symbol: "AAPL".into(),
        tf: Timeframe::D1,
        bars: Arc::new(bars),
        source: "fixture".into(),
        observed_at: Some(observed),
        stitch: None,
    }
}
fn spec(rule: &str, period: WhenPeriod) -> WhenSpec {
    WhenSpec::parse(rule, 0.).unwrap().with_period(period)
}

#[test]
fn period_parse_token_roundtrip_and_default() {
    assert_eq!(WhenPeriod::default(), WhenPeriod::Years(10));
    assert_eq!(WhenPeriod::parse("5y").unwrap(), WhenPeriod::Years(5));
    assert_eq!(WhenPeriod::parse("20Y").unwrap(), WhenPeriod::Years(20));
    assert_eq!(WhenPeriod::parse("2015-03-02").unwrap(), WhenPeriod::Since(day(2015, 3, 2)));
    assert!(WhenPeriod::parse("0y").is_err() && WhenPeriod::parse("abc").is_err());
    let c = parse_command("period=5y RSI(14)<30").unwrap();
    assert_eq!(c.period, Some(WhenPeriod::Years(5)));
    assert_eq!(c.rule.as_deref(), Some("RSI(14)<30"));
    assert_eq!(parse_command("since=2020-01-02").unwrap().period, Some(WhenPeriod::Since(day(2020, 1, 2))));
    assert!(parse_command("period=5y period=10y").is_err());
    assert_eq!(WhenPeriod::Years(10).token(), "period=10y");
    // Bare 5Y/10Y/20Y are tickers, never a period.
    for t in ["5Y", "10Y", "20Y"] {
        assert!(parse_command(t).map_or(true, |c| c.period.is_none()), "{t}");
    }
}

#[test]
fn preflight_exact_session_limit() {
    let start = day(2000, 1, 3);
    let mut d = start;
    for _ in 0..MAX_SESSIONS - 1 {
        d = crate::history::calendar::next_session(d);
    }
    assert_eq!(preflight(Timeframe::D1, midnight(start), midnight(d)).unwrap(), (MAX_SESSIONS, MAX_SESSIONS));
    let over = midnight(crate::history::calendar::next_session(d));
    let err = preflight(Timeframe::D1, midnight(start), over).unwrap_err();
    assert!(err.starts_with(PERIOD_LIMIT_PREFIX), "{err}");
}

#[test]
fn preflight_exact_slot_limit_and_early_closes() {
    let s0 = day(2024, 11, 18);
    let (mut d, mut cum, mut days) = (s0, 0usize, vec![]);
    while cum <= MAX_GRID {
        cum += crate::intraday::regular_opens(d, Timeframe::M1).len();
        days.push((d, cum));
        d = crate::history::calendar::next_session(d);
    }
    let open = |d| crate::intraday::regular_opens(d, Timeframe::M1)[0];
    let n = days.len();
    let (ok_day, ok_cum) = days[n - 2];
    assert!(ok_cum <= MAX_GRID && days[n - 1].1 > MAX_GRID);
    assert_eq!(preflight(Timeframe::M1, open(s0), open(ok_day)).unwrap().1, ok_cum);
    let err = preflight(Timeframe::M1, open(s0), open(days[n - 1].0)).unwrap_err();
    assert!(err.starts_with(PERIOD_LIMIT_PREFIX), "{err}");
    // Early close (Black Friday) has fewer slots than a full session; Thanksgiving is closed.
    let (full, early) = [day(2024, 11, 26), day(2024, 11, 29)]
        .map(|d| crate::intraday::regular_opens(d, Timeframe::M5).len())
        .into();
    assert!(early < full);
    let first = crate::intraday::regular_opens(day(2024, 11, 26), Timeframe::M5)[0];
    let last = crate::intraday::regular_opens(day(2024, 11, 29), Timeframe::M5)[0];
    assert_eq!(preflight(Timeframe::M5, first, last).unwrap(), (3, 2 * full + early));
}

#[test]
fn history_over_20k_sessions_opens_within_default_period() {
    // ~82 years of daily sessions: far above the 20,000-session grid limit.
    let bars = daily(day(1942, 1, 2), 21_500, &[]);
    assert!(bars.len() > MAX_SESSIONS);
    let inp = input(bars);
    let full = compute(&inp, &spec("close>SMA(20)", WhenPeriod::Years(100)));
    let err = full.err().expect("whole history must be refused, not truncated silently");
    assert!(err.starts_with(PERIOD_LIMIT_PREFIX), "{err}");
    let r = compute(&inp, &WhenSpec::parse("close>SMA(20)", 0.).unwrap()).expect("default 10Y period opens");
    let w = r.window.as_ref().unwrap();
    assert_eq!(w.period, WhenPeriod::Years(10));
    assert_eq!(w.warmup_bars, WARMUP_BARS);
    assert!(r.sessions < 2_700 && r.sessions > 2_400, "{}", r.sessions);
    assert_eq!(r.mask.ts[0], w.scored_from);
    assert!(r.mask.ts.iter().all(|t| *t >= w.scored_from));
    assert_eq!(r.session_rows.len(), r.sessions);
    // 20Y also fits; bootstrap enforces the same limit and still runs.
    let r20 = compute(&inp, &spec("close>SMA(20)", WhenPeriod::Years(20))).unwrap();
    assert!(r20.sessions > r.sessions && r20.sessions < MAX_SESSIONS);
    assert_eq!(r.bootstrap().sessions, r.sessions);
}

#[test]
fn warmup_never_scored_and_period_changes_identity() {
    let inp = input(daily(day(2012, 1, 3), 3_000, &[]));
    let rule = "close>SMA(50)";
    let r5 = compute(&inp, &spec(rule, WhenPeriod::Years(5))).unwrap();
    let w = r5.window.clone().unwrap();
    assert_eq!(w.warmup_bars, WARMUP_BARS);
    // Every scored slot is inside the window; the first slot is already warm.
    assert_eq!(r5.mask.ts[0], w.scored_from);
    assert!(r5.mask.values[0].is_some(), "warm-up supplies SMA(50) at the first scored slot");
    let in_window = inp.bars.iter().filter(|b| b.ts >= w.scored_from).count();
    assert_eq!(r5.mask.ts.len(), in_window);
    // h=1: each in-window slot but the last has an outcome, and no warm-up slot does.
    let h1 = &r5.cells[0];
    assert_eq!(h1.raw_n, in_window - 1);
    // Outcomes after the last scored slot are never invented.
    let total_rule: usize = r5.session_rows.iter().map(|r| r[0][0] + r[0][2]).sum();
    assert_eq!(total_rule, in_window - 1);
    // Identity: different periods hash differently, same period is stable.
    let k = |p| input_key(&inp, &spec(rule, p)).unwrap();
    assert_ne!(k(WhenPeriod::Years(5)), k(WhenPeriod::Years(10)));
    assert_ne!(k(WhenPeriod::Years(5)), k(WhenPeriod::Since(day(2020, 1, 2))));
    assert_eq!(k(WhenPeriod::Years(5)), k(WhenPeriod::Years(5)));
    assert_eq!(r5.key, k(WhenPeriod::Years(5)));
    // A window longer than the history is clipped and disclosed, never padded.
    let all = compute(&inp, &spec(rule, WhenPeriod::Years(50))).unwrap();
    let w = all.window.unwrap();
    assert!(w.clipped && w.warmup_bars == 0 && all.mask.ts.len() == inp.bars.len());
    // A start after the last bar has nothing to score.
    assert!(compute(&inp, &spec(rule, WhenPeriod::Since(day(2090, 1, 2)))).is_err());
}

#[test]
fn intraday_ignores_period_in_key() {
    let mut bars = vec![];
    let mut d = day(2025, 1, 2);
    for _ in 0..45 {
        for ts in crate::intraday::regular_opens(d, Timeframe::M5) {
            bars.push(Bar { ts, open: 100., high: 100.1, low: 99.9, close: 100., volume: 1., provenance: Provenance::Real });
        }
        d = crate::history::calendar::next_session(d);
    }
    let observed = bars.last().unwrap().ts + 3600;
    let inp = CanvasInput {
        symbol: "AAPL".into(),
        tf: Timeframe::M5,
        bars: Arc::new(bars),
        source: "fixture".into(),
        observed_at: Some(observed),
        stitch: None,
    };
    let a = input_key(&inp, &spec("close>SMA(5)", WhenPeriod::Years(5))).unwrap();
    let b = input_key(&inp, &spec("close>SMA(5)", WhenPeriod::Years(20))).unwrap();
    assert_eq!(a, b);
    assert!(compute(&inp, &spec("close>SMA(5)", WhenPeriod::Years(5))).unwrap().window.is_none());
}

#[test]
fn missing_sessions_keep_resets_and_horizon_boundaries() {
    // Sessions 1500 and 2000 are missing inside the 5Y window (about 1,260 sessions).
    let inp = input(daily(day(2016, 1, 4), 2_400, &[2_000, 2_001]));
    let r = compute(&inp, &spec("close>0", WhenPeriod::Years(5))).unwrap();
    let w = r.window.clone().unwrap();
    let present = inp.bars.iter().filter(|b| b.ts >= w.scored_from).count();
    // Expected slots keep the two missing sessions as unavailable mask entries.
    assert_eq!(r.mask.ts.len(), present + 2);
    assert_eq!(r.mask.values.iter().filter(|v| v.is_none()).count() >= 2, true);
    // h=1: outcome needs two present consecutive bars, so the missing run
    // removes (missing, predecessor) and the final bar has no outcome.
    assert_eq!(r.cells[0].raw_n, present - 2);
}
