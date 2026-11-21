use super::*;
use crate::whats_news::scanner;
use tr_core::Provenance;

const MON_2023_01_02: i64 = 1_672_617_600;
const DAY: i64 = 86_400;

fn real(ts: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
    Bar::new(ts, o, h, l, c, 1000.0).with_provenance(Provenance::Real)
}
fn from_closes(closes: &[f64]) -> Vec<Bar> {
    closes
        .iter()
        .enumerate()
        .map(|(i, c)| real(1_704_067_200 + i as i64 * DAY, *c, c + 1.0, c - 1.0, *c))
        .collect()
}
/// 64 bars: prior 63 from (h, l) pairs, then a current bar closing at `c`.
fn daily_vector(prior: impl Fn(usize) -> (f64, f64), c: f64) -> Vec<Bar> {
    let mut bars: Vec<Bar> = (0..63)
        .map(|i| {
            let (h, l) = prior(i);
            real(1_704_067_200 + i as i64 * DAY, (h + l) / 2.0, h, l, (h + l) / 2.0)
        })
        .collect();
    bars.push(real(1_704_067_200 + 63 * DAY, c, c + 1.0, c - 1.0, c));
    bars
}
/// Mon–Fri bars per week: (high, low, last close). Every bar spans [low, high]; Friday closes at `c`.
fn weekly_fixture(weeks: &[(f64, f64, f64)]) -> Vec<Bar> {
    let mut out = vec![];
    for (k, (h, l, c)) in weeks.iter().enumerate() {
        let mid = (h + l) / 2.0;
        for d in 0..5 {
            let ts = MON_2023_01_02 + (k as i64 * 7 + d) * DAY;
            out.push(real(ts, mid, *h, *l, if d == 4 { *c } else { mid }));
        }
    }
    out
}
fn rising(n: usize) -> Vec<Bar> {
    from_closes(&(0..n).map(|i| 100.0 + i as f64).collect::<Vec<_>>())
}
fn kind(r: &Result<PatternRead, Unavailable>) -> Option<PatternKind> {
    r.as_ref().unwrap().kind
}
fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}
fn values_of(spec: &[(f64, usize)]) -> [Option<f64>; 20] {
    let mut v = [None; 20];
    let mut i = 0;
    for (x, n) in spec {
        for _ in 0..*n {
            v[i] = Some(*x);
            i += 1;
        }
    }
    v
}
fn hist(n: usize) -> Vec<(String, Vec<Bar>)> {
    (0..n)
        .map(|i| {
            let mut x = 12345u64 + i as u64 * 7919;
            let closes: Vec<f64> = (0..300)
                .scan(100.0, |p, _| {
                    x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    *p *= 1.0 + ((x >> 33) as f64 / (1u64 << 31) as f64 - 0.5) * 0.04;
                    Some(*p)
                })
                .collect();
            (format!("S{i:03}"), from_closes(&closes))
        })
        .collect()
}

#[test]
fn rsi_reference_ewm_known_answers() {
    assert!(approx(rsi_reference_ewm(&[10., 13., 12., 13., 14.], 3).unwrap(), 3900.0 / 43.0));
    assert!(approx(rsi_reference_ewm(&[10., 12., 11., 12., 13.], 2).unwrap(), 800.0 / 9.0));
    assert_eq!(rsi_reference_ewm(&[10.; 5], 3), Some(100.0));
    assert_eq!(rsi_reference_ewm(&[14., 13., 12., 11., 10.], 3), Some(0.0));
    assert_eq!(rsi_reference_ewm(&[10., 11., 12., 13.], 3), None, "L = n+1 is undefined");
    assert_eq!(rsi_reference_ewm(&[10., 11., 12., 13., 14.], 0), None);
    let r6 = [10., 13., 12., 13., 14., 15.];
    let (a, b) = (rsi_reference_ewm(&r6, 3).unwrap(), rsi_reference_ewm(&r6[..5], 3).unwrap());
    assert!(approx(a, 10500.0 / 113.0) && approx(b, 3900.0 / 43.0));
    assert!(approx(a - b, 10800.0 / 4859.0));
}

#[test]
fn rsic_reference_ewm_differs_from_wilder_and_flat_reads_reference_value() {
    let wilder = |closes: &[f64], n| scanner::rsi(&from_closes(closes), n);
    let r1 = [10., 13., 12., 13., 14.];
    assert!(approx(wilder(&r1, 3).unwrap(), 1100.0 / 13.0));
    assert!(!approx(wilder(&r1, 3).unwrap(), rsi_reference_ewm(&r1, 3).unwrap()));
    let r2 = [10., 12., 11., 12., 13.];
    assert!(approx(wilder(&r2, 2).unwrap(), rsi_reference_ewm(&r2, 2).unwrap()), "identical at n=2");
    assert_eq!(wilder(&[10.; 5], 3), Some(50.0));
    assert_eq!(rsi_reference_ewm(&[10.; 5], 3), Some(100.0));
    assert_eq!(wilder(&[14., 13., 12., 11., 10.], 3), Some(0.0));
    assert!(wilder(&[10., 11., 12., 13.], 3).is_some(), "Wilder needs only n+1 closes");
}

#[test]
fn rsic_lengths_2_21_same_last_bar() {
    let closes: Vec<f64> = hist(1)[0].1.iter().map(|b| b.close).collect();
    let c = rsi_counter(&closes[100..]).unwrap();
    for i in 0..20 {
        assert_eq!(c.values[i], rsi_reference_ewm(&closes[100..], i + 2), "length {}", i + 2);
    }
    assert_eq!(c.valid, 20);
    assert_eq!(c.closes_used, 200);
}

#[test]
fn rsic_exact_50_enters_both_trend_buckets() {
    let c = RsiCounter::from_values(values_of(&[(50.0, 20)]), 100, false).unwrap();
    assert_eq!((c.up, c.down), (20, 20));
    assert_eq!(c.state, RsicState::UpStrong);
    assert_eq!(c.align, 1.0);
}

#[test]
fn rsic_up_before_down_priority() {
    let c = RsiCounter::from_values(values_of(&[(50.0, 12), (30.0, 8)]), 100, false).unwrap();
    assert_eq!((c.up, c.down), (12, 20));
    assert_eq!(c.state, RsicState::UpTilt, "down is 100% but up is checked first");
}

#[test]
fn rsic_state_priority_and_tiers() {
    let a = values_of(&[(50.0, 4), (60.0, 7), (75.0, 3), (20.0, 2), (10.0, 1), (85.0, 2), (95.0, 1)]);
    let a = RsiCounter::from_values(a, 100, false).unwrap();
    assert_eq!((a.os20, a.os10, a.ob80, a.ob90, a.up, a.down), (1, 0, 3, 1, 14, 4));
    assert_eq!(a.state, RsicState::Up);
    assert!(approx(a.mean, 58.0));
    let state = |spec: &[(f64, usize)]| RsiCounter::from_values(values_of(spec), 100, false).unwrap().state;
    assert_eq!(state(&[(15.0, 12), (85.0, 8)]), RsicState::OsLean, "OS before OB");
    assert_eq!(state(&[(15.0, 10), (85.0, 10)]), RsicState::Mixed);
    assert_eq!(state(&[(15.0, 18), (50.0, 2)]), RsicState::OsExtreme);
    assert_eq!(state(&[(15.0, 14), (50.0, 6)]), RsicState::Oversold);
    assert_eq!(state(&[(85.0, 11), (50.0, 9)]), RsicState::ObLean);
    assert_eq!(state(&[(85.0, 14), (50.0, 6)]), RsicState::Overbought);
    assert_eq!(state(&[(85.0, 18), (50.0, 2)]), RsicState::ObExtreme);
    assert_eq!(state(&[(30.0, 20)]), RsicState::DownStrong);
    assert_eq!(state(&[(30.0, 14), (60.0, 6)]), RsicState::Down);
    assert_eq!(state(&[(30.0, 11), (90.0, 9)]), RsicState::DownTilt);
    // 54.99% never tilts (integer arithmetic): 11 of 20 is 55% exactly, 10 is not.
    assert_eq!(state(&[(60.0, 10), (90.0, 5), (15.0, 5)]), RsicState::Mixed);
    assert_eq!(state(&[(60.0, 11), (90.0, 5), (15.0, 4)]), RsicState::UpTilt);
}

#[test]
fn rsic_alignment_formula() {
    let a = values_of(&[(50.0, 4), (60.0, 7), (75.0, 3), (20.0, 2), (10.0, 1), (85.0, 2), (95.0, 1)]);
    let a = RsiCounter::from_values(a, 100, false).unwrap();
    let sd = (9170.0f64 / 19.0).sqrt();
    assert!(approx(a.sd, sd));
    assert!(approx(a.align, 1.0 - sd / 25.0));
    assert!((a.align - 0.1212449).abs() < 1e-6);
    let wide = RsiCounter::from_values(values_of(&[(0.0, 10), (100.0, 10)]), 100, false).unwrap();
    assert_eq!(wide.align, 0.0, "sd/25 caps at 1");
}

#[test]
fn rsic_too_few_lengths() {
    let mut nine = [None; 20];
    nine[..9].fill(Some(50.0));
    assert_eq!(RsiCounter::from_values(nine, 50, false), Err(Unavailable::TooFewLengths { valid: 9 }));
    let mut ten = [None; 20];
    ten[..10].fill(Some(50.0));
    let c = RsiCounter::from_values(ten, 50, false).unwrap();
    assert_eq!(c.valid, 10);
    assert_eq!(c.share(c.up), 1.0);
    assert_eq!(c.state, RsicState::UpStrong);
    assert_eq!(rsi_counter(&[1.0; 23]), Err(Unavailable::Short { have: 23, need: 24 }));
    let up = rsi_counter(&(100..=123).map(f64::from).collect::<Vec<_>>()).unwrap();
    assert_eq!((up.state, up.valid, up.ob90), (RsicState::ObExtreme, 20, 20));
    assert!(up.values.iter().all(|v| *v == Some(100.0)));
    assert_eq!((up.mean, up.align, up.flat), (100.0, 1.0, false));
    let down = rsi_counter(&(100..=123).rev().map(f64::from).collect::<Vec<_>>()).unwrap();
    assert_eq!((down.state, down.os10), (RsicState::OsExtreme, 20));
    assert!(down.values.iter().all(|v| *v == Some(0.0)));
    let flat = rsi_counter(&[50.0; 24]).unwrap();
    assert_eq!((flat.state, flat.flat), (RsicState::ObExtreme, true));
}

#[test]
fn rsic_24_bar_name_admitted_despite_30_bar_scan_validate() {
    let bars = rising(24);
    let row = engine_row("AAA", &bars, EngineParams::default()).unwrap();
    assert!(row.rsic_d.is_ok());
    assert_eq!(row.pattern_d, Err(Unavailable::Short { have: 24, need: 64 }));
    let err = scanner::calculate(&bars).unwrap_err();
    assert!(err.contains("need 30"), "{err}");
    let short = engine_row("AAA", &rising(23), EngineParams::default()).unwrap_err();
    assert!(short.reason.contains("need 24"));
}

#[test]
fn rsic_weekly_failure_keeps_daily() {
    let row = engine_row("AAA", &weekly_fixture(&vec![(101.0, 99.0, 100.0); 14]), EngineParams::default()).unwrap();
    assert!(row.rsic_d.is_ok() && row.pattern_d.is_ok());
    let w = row.weekly.unwrap();
    assert!(matches!(w.pattern, Err(Unavailable::Short { need: 28, .. })));
    assert!(matches!(w.rsic, Err(Unavailable::Short { need: 24, .. })));
}

#[test]
fn rsic_acceleration_zero_counts_as_accelerating() {
    let row = engine_row("AAA", &rising(30), EngineParams::default()).unwrap();
    assert_eq!(row.accel, Some(0.0), "both reads are 100 on a straight rise");
    assert!(row.accel.unwrap() >= 0.0);
    // Needs L-k >= n+2.
    let tight = engine_row("AAA", &rising(24), EngineParams { rsi_n: 14, lag: 9 }).unwrap();
    assert_eq!(tight.accel, None);
    let ok = engine_row("AAA", &rising(24), EngineParams { rsi_n: 14, lag: 8 }).unwrap();
    assert_eq!(ok.accel, Some(0.0));
    // R6 through the row path: n=3, k=1 on 24 closes ending ...,14,15.
    let mut closes = vec![10.0; 18];
    closes.extend([10., 13., 12., 13., 14., 15.]);
    let row = engine_row("AAA", &from_closes(&closes), EngineParams { rsi_n: 3, lag: 1 }).unwrap();
    let want = rsi_reference_ewm(&closes, 3).unwrap() - rsi_reference_ewm(&closes[..23], 3).unwrap();
    assert!(approx(row.accel.unwrap(), want));
}

#[test]
fn dw_agreement_and_pullback() {
    use RsicState::*;
    assert_eq!(dw_flags(UpTilt, UpStrong), (Some(Agreement::Up), false));
    assert_eq!(dw_flags(OsLean, UpTilt), (None, true));
    assert_eq!(dw_flags(Up, Mixed), (None, false));
    assert_eq!(dw_flags(DownStrong, DownTilt), (Some(Agreement::Down), false));
    assert_eq!(dw_flags(Oversold, Down), (None, false));
    let row = engine_row("AAA", &rising(30), EngineParams::default()).unwrap();
    assert_eq!((row.agreement, row.pullback), (None, false), "weekly unavailable gives None");
}

#[test]
fn pattern_daily_breakout_breakdown_thirds_priority() {
    let flat = |h, l| move |_| (h, l);
    let r = pattern_daily(&daily_vector(flat(10.0, 5.0), 10.0)).unwrap();
    assert_eq!((r.kind, r.position), (Some(PatternKind::Breakout), Some(1.0)));
    let r = pattern_daily(&daily_vector(flat(10.0, 5.0), 5.0)).unwrap();
    assert_eq!((r.kind, r.position), (Some(PatternKind::Breakdown), Some(0.0)));
    let d3 = |i: usize| if i < 42 { (20.0, 5.0) } else { (8.0, 5.0) };
    let r = pattern_daily(&daily_vector(d3, 9.0)).unwrap();
    assert_eq!(r.kind, Some(PatternKind::FromBottom));
    assert!(approx(r.position.unwrap(), 4.0 / 15.0));
    let r = pattern_daily(&daily_vector(d3, 10.0)).unwrap();
    assert_eq!(r.position, Some(1.0 / 3.0), "5/15 equals 1/3 in IEEE");
    assert_eq!(r.kind, Some(PatternKind::FromBottom));
    assert_eq!(pattern_daily(&daily_vector(d3, 10.5)).unwrap().kind, None);
    let d4 = |i: usize| if i < 42 { (20.0, 5.0) } else { (20.0, 17.0) };
    let r = pattern_daily(&daily_vector(d4, 16.0)).unwrap();
    assert_eq!(r.kind, Some(PatternKind::FromTop));
    assert!(approx(r.position.unwrap(), 11.0 / 15.0));
    let r = pattern_daily(&daily_vector(d4, 15.0)).unwrap();
    assert_eq!(r.position, Some(2.0 / 3.0));
    assert_eq!(r.kind, Some(PatternKind::FromTop));
    // Zero span is unreachable below a break: a flat window is a Breakout with no position.
    let r = pattern_daily(&daily_vector(flat(10.0, 10.0), 10.0)).unwrap();
    assert_eq!((r.kind, r.position), (Some(PatternKind::Breakout), None));
    let short = daily_vector(flat(10.0, 5.0), 10.0);
    assert_eq!(pattern_daily(&short[1..]), Err(Unavailable::Short { have: 63, need: 64 }));
    // Prior windows exclude t: the current bar's own high never moves H.
    let r = pattern_daily(&daily_vector(flat(10.0, 5.0), 10.0)).unwrap();
    assert_eq!((r.hi_long, r.lo_long, r.trigger_ts), (10.0, 5.0, 1_704_067_200 + 63 * DAY));
}

fn weekly_of(weeks: &[(f64, f64, f64)]) -> Result<PatternRead, Unavailable> {
    engine_row("AAA", &weekly_fixture(weeks), EngineParams::default()).unwrap().weekly.unwrap().pattern
}
fn year_with_last(base: (f64, f64), last: (f64, f64, f64)) -> Vec<(f64, f64, f64)> {
    let mut w = vec![(base.0, base.1, (base.0 + base.1) / 2.0); 51];
    w.push(last);
    w
}

#[test]
fn pattern_weekly_0999_1001_tolerance() {
    assert_eq!(kind(&weekly_of(&year_with_last((100.0, 80.0), (100.0, 99.0, 99.95)))), Some(PatternKind::Breakout));
    assert_eq!(kind(&weekly_of(&year_with_last((100.0, 80.0), (100.0, 99.0, 99.85)))), None);
    assert_eq!(kind(&weekly_of(&year_with_last((100.0, 80.0), (81.0, 80.0, 80.05)))), Some(PatternKind::Breakdown));
    assert_eq!(kind(&weekly_of(&year_with_last((100.0, 80.0), (81.0, 80.0, 80.15)))), None);
    // Flat year: breakout with no position (H == L).
    let r = weekly_of(&vec![(100.0, 100.0, 100.0); 52]).unwrap();
    assert_eq!((r.kind, r.position), (Some(PatternKind::Breakout), None));
}

#[test]
fn pattern_weekly_thirds_inclusive_windows() {
    let mut w3 = vec![(200.0, 100.0, 150.0); 26];
    w3.extend(vec![(120.0, 100.0, 110.0); 25]);
    w3.push((125.0, 118.0, 124.95));
    let r = weekly_of(&w3).unwrap();
    assert_eq!(r.kind, Some(PatternKind::FromBottom));
    assert!(approx(r.position.unwrap(), 0.2495));
    assert_eq!(r.hi_short, 125.0, "H26 includes the current week");
    let mut w4 = vec![(200.0, 100.0, 150.0); 26];
    w4.extend(vec![(200.0, 180.0, 190.0); 25]);
    w4.push((182.0, 175.0, 175.1));
    let r = weekly_of(&w4).unwrap();
    assert_eq!(r.kind, Some(PatternKind::FromTop));
    assert!(approx(r.position.unwrap(), 0.751));
    assert_eq!(r.lo_short, 175.0);
}

#[test]
fn pattern_weekly_28_to_51_weeks_reports_actual_length() {
    let w = |n: usize| vec![(100.0, 80.0, 90.0); n];
    assert_eq!(weekly_of(&w(27)), Err(Unavailable::Short { have: 27, need: 28 }));
    let r = weekly_of(&w(28)).unwrap();
    assert_eq!((r.bars_used, r.full_year), (28, false));
    let r = weekly_of(&w(51)).unwrap();
    assert_eq!((r.bars_used, r.full_year), (51, false));
    let r = weekly_of(&w(52)).unwrap();
    assert_eq!((r.bars_used, r.full_year), (52, true));
    let mut long = w(80);
    long[0] = (300.0, 80.0, 90.0);
    let r = weekly_of(&long).unwrap();
    assert_eq!((r.bars_used, r.full_year, r.hi_long), (52, true, 100.0));
}

#[test]
fn weekly_trigger_is_last_session_of_week() {
    // 2026-03-02 (Mon) .. 2026-04-02 (Thu); Good Friday 2026-04-03 is a holiday.
    let mon = chrono::NaiveDate::from_ymd_opt(2026, 3, 2).unwrap();
    let ts = |d: chrono::NaiveDate| d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
    let bars: Vec<Bar> = (0..32)
        .map(|i| mon + chrono::Duration::days(i))
        .filter(|d| chrono::Datelike::weekday(d).num_days_from_monday() < 5 && d.to_string() <= "2026-04-02".to_string())
        .map(|d| real(ts(d), 100.0, 101.0, 99.0, 100.0))
        .collect();
    let row = engine_row("AAA", &bars, EngineParams::default()).unwrap();
    let w = row.weekly.unwrap();
    let thu = chrono::NaiveDate::from_ymd_opt(2026, 4, 2).unwrap();
    assert_eq!(w.week_start, ts(chrono::NaiveDate::from_ymd_opt(2026, 3, 30).unwrap()));
    assert_eq!(w.trigger_ts, ts(thu));
    assert_ne!(w.trigger_ts, w.week_start);
    assert_eq!(row.last_ts, ts(thu));
}

#[test]
fn nan_volume_admitted_by_views() {
    let mut bars = rising(40);
    for b in &mut bars {
        b.volume = f64::NAN;
    }
    assert!(engine_row("AAA", &bars, EngineParams::default()).is_ok());
    assert!(scanner::calculate(&bars).is_err(), "SCAN still requires finite volume");
    bars[3].volume = f64::INFINITY;
    assert!(engine_row("AAA", &bars, EngineParams::default()).is_err());
    let mut bad = rising(40);
    bad[10].low = -1.0;
    assert!(engine_row("AAA", &bad, EngineParams::default()).is_err());
}

#[test]
fn sample_input_refuses_board_missing_input_marks_invalid() {
    let mut h = hist(3);
    assert!(refuse_sample(&h).is_ok());
    h[1].1[5].provenance = Provenance::Sample;
    assert!(refuse_sample(&h).unwrap_err().contains("sample"));
    let mut h = hist(3);
    for b in &mut h[2].1 {
        b.provenance = Provenance::Missing;
    }
    let board = engine_board(&h, EngineParams::default());
    assert_eq!(board.rows.len(), 2);
    assert_eq!(board.invalid.len(), 1);
    assert_eq!(board.invalid[0].symbol, "S002");
    assert!(board.invalid[0].reason.contains("unverified"));
    assert!(engine_row("S002", &h[2].1, EngineParams::default()).is_err());
}

#[test]
fn engine_board_deterministic_across_thread_counts() {
    let mut h = hist(41);
    h.push(("SHORT".into(), rising(10)));
    h.reverse();
    let base = engine_board_threads(&h, EngineParams::default(), 1);
    assert_eq!(base.rows.len(), 41);
    assert_eq!(base.short.len(), 1);
    assert!(base.rows.windows(2).all(|w| w[0].symbol < w[1].symbol));
    assert_eq!(base.as_of_ts, Some(1_704_067_200 + 299 * DAY));
    for t in [2, 3, 4, 7] {
        assert_eq!(engine_board_threads(&h, EngineParams::default(), t), base, "threads={t}");
    }
}

#[test]
fn board_cache_key_has_no_wall_clock_and_corrected_tail_bar_misses() {
    let h = hist(6);
    let p = EngineParams::default();
    assert_eq!(board_key(&h, p), board_key(&h, p));
    let mut reordered = h.clone();
    reordered.reverse();
    assert_eq!(board_key(&h, p), board_key(&reordered, p), "key is over sorted symbols");
    assert_ne!(board_key(&h, p), board_key(&h, EngineParams { rsi_n: 9, lag: 5 }));
    let cache = BoardCache::default();
    let (a, hit) = cache.get_or_compute(&h, p);
    assert!(!hit);
    let (b, hit) = cache.get_or_compute(&reordered, p);
    assert!(hit && Arc::ptr_eq(&a, &b));
    let mut fixed = h.clone();
    fixed[2].1[250].close += 0.5;
    fixed[2].1[250].high += 0.5;
    let (c, hit) = cache.get_or_compute(&fixed, p);
    assert!(!hit, "a corrected bar inside the consumed tail must miss");
    assert_ne!(a, c);
    assert_eq!(cache.len(), 2);
    // Bars older than the consumed tail never enter the key.
    let mut long = vec![("L".to_string(), rising(700))];
    let k0 = board_key(&long, p);
    long[0].1[10].close += 1.0;
    assert_eq!(k0, board_key(&long, p));
    // LRU cap.
    for n in 0..6u64 {
        let mut x = h.clone();
        x[0].1[299].close += 1.0 + n as f64;
        x[0].1[299].high += 1.0 + n as f64;
        cache.get_or_compute(&x, p);
    }
    assert_eq!(cache.len(), 4);
}

#[test]
fn board_cache_lock_not_held_during_compute() {
    let cache = BoardCache::default();
    let h = hist(2);
    let p = EngineParams::default();
    let (_, hit) = cache.get_or_compute_with(&h, p, || {
        assert!(cache.lock_free(), "cache lock must be free while computing");
        engine_board(&h, p)
    });
    assert!(!hit);
    assert!(cache.get_or_compute(&h, p).1);
}

#[test]
fn sort_order_nulls_last_both_directions_symbol_tiebreak() {
    let h: Vec<_> = ["D", "A", "C", "B", "E"].iter().map(|s| (s.to_string(), rising(30))).collect();
    let board = engine_board(&h, EngineParams::default());
    let key = |r: &EngineRow| match r.symbol.as_str() {
        "A" => Some(2.0),
        "B" => None,
        "C" => Some(2.0),
        "D" => Some(f64::NAN),
        _ => Some(1.0),
    };
    let idx: Vec<usize> = (0..board.rows.len()).collect();
    let names = |o: Vec<usize>| o.into_iter().map(|i| board.rows[i].symbol.clone()).collect::<Vec<_>>();
    assert_eq!(names(sort_order(&board.rows, &idx, key, true)), ["A", "C", "E", "B", "D"]);
    assert_eq!(names(sort_order(&board.rows, &idx, key, false)), ["E", "A", "C", "B", "D"]);
    assert_eq!(names(sort_order(&board.rows, &idx[1..], key, true)), ["C", "E", "B", "D"]);
}

#[test]
fn scan_view_spec_parse_canonical_roundtrip() {
    let p = |s: &str| ScanViewSpec::parse(s).unwrap().unwrap();
    assert_eq!(p("VIEW PATTERN").view, ScanView::Pattern);
    assert_eq!(p("view pattern").canonical(), "VIEW PATTERN");
    assert_eq!(p("VIEW RSIC"), p("VIEW RSI-C"));
    assert_eq!(p("view rsi-c").params, EngineParams { rsi_n: 14, lag: 5 });
    assert_eq!(p("VIEW RSI-C").canonical(), "VIEW RSI-C");
    let spec = p("view rsic n=9 k=3");
    assert_eq!((spec.params.rsi_n, spec.params.lag), (9, 3));
    assert_eq!(spec.canonical(), "VIEW RSI-C N=9 K=3");
    assert_eq!(p(&spec.canonical()), spec);
    assert_eq!(p("VIEW RSI-C K=3").canonical(), "VIEW RSI-C K=3");
    assert_eq!(p("VIEW RSI-C N=14 K=5").canonical(), "VIEW RSI-C");
    assert!(ScanViewSpec::parse("NEAR_HIGH").is_none());
    assert!(ScanViewSpec::parse("").is_none());
    assert!(ScanViewSpec::parse("MA VIEW").is_none());
    let err = |s: &str| ScanViewSpec::parse(s).unwrap().unwrap_err();
    assert_eq!(err("VIEW"), "SCAN VIEW needs PATTERN or RSI-C");
    assert_eq!(err("VIEW FOO"), "Unknown view FOO. Available: PATTERN, RSI-C");
    for bad in ["VIEW RSI-C N=1", "VIEW RSI-C N=22", "VIEW RSI-C K=0", "VIEW RSI-C K=21", "VIEW RSI-C N=x", "VIEW RSI-C N=9 N=9", "VIEW RSI-C K=2 K=3", "VIEW RSI-C Z=3", "VIEW RSI-C 9", "VIEW PATTERN N=9", "VIEW PATTERN K=3"] {
        assert!(ScanViewSpec::parse(bad).unwrap().is_err(), "{bad}");
    }
    assert!(ScanViewSpec::parse("VIEW RSI-C N=2 K=1").unwrap().is_ok());
    assert!(ScanViewSpec::parse("VIEW RSI-C N=21 K=20").unwrap().is_ok());
    assert!(p("VIEW PATTERN").describe().contains("63/21-bar daily and 52/26-week extremes, thirds"));
}
