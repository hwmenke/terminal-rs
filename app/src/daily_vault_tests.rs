//! Coordinator and planning tests. No network, no user data dir; real (millisecond-scale) time
//! is used because the app's tokio build has no paused-clock support.
use crate::daily_vault::*;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tr_data::vault::daily::{DailyIngest, DailyIngestOutcome, DailyState};

fn fast() -> Pacing {
    Pacing { concurrency: 2, spacing: Duration::from_millis(40), backoff: Duration::from_millis(20), attempts: 4 }
}
fn names(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("S{i}")).collect()
}

#[tokio::test]
async fn daily_jobs_bounded_concurrency_paced_starts_and_backoff_retries() {
    let coordinator = Coordinator::default();
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let starts = Arc::new(Mutex::new(Vec::<Instant>::new()));
    let attempts = Arc::new(Mutex::new(std::collections::HashMap::<String, Vec<Instant>>::new()));
    let notes = Arc::new(Mutex::new(Vec::<Progress>::new()));
    let (a, p, st, at, nt) = (active.clone(), peak.clone(), starts.clone(), attempts.clone(), notes.clone());
    let started = Instant::now();
    let result = coordinator
        .run(
            names(8),
            fast(),
            move |pr| nt.lock().unwrap().push(pr),
            move |symbol| {
                let (a, p, st, at) = (a.clone(), p.clone(), st.clone(), at.clone());
                async move {
                    let now = Instant::now();
                    st.lock().unwrap().push(now);
                    let n = {
                        let mut m = at.lock().unwrap();
                        let v = m.entry(symbol.clone()).or_default();
                        v.push(now);
                        v.len()
                    };
                    let cur = a.fetch_add(1, Ordering::SeqCst) + 1;
                    p.fetch_max(cur, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(60)).await;
                    a.fetch_sub(1, Ordering::SeqCst);
                    match (symbol.as_str(), n) {
                        ("S1", 1 | 2) => Err(JobError::Transient { message: "offline".into(), retry_after: None }),
                        ("S2", _) => Err(JobError::Permanent("unsupported".into())),
                        ("S3", _) => Err(JobError::Transient { message: "429".into(), retry_after: None }),
                        ("S4", 1) => Err(JobError::Transient {
                            message: "429".into(),
                            retry_after: Some(Duration::from_millis(90)),
                        }),
                        _ => Ok("ok".into()),
                    }
                }
            },
        )
        .await
        .unwrap();
    assert!(peak.load(Ordering::SeqCst) <= 2, "at most two concurrent requests");
    assert!(peak.load(Ordering::SeqCst) >= 2, "and the pool is actually used");
    let mut all = starts.lock().unwrap().clone();
    all.sort();
    for w in all.windows(2) {
        assert!(w[1] - w[0] >= Duration::from_millis(37), "request starts are paced: {:?}", w[1] - w[0]);
    }
    let attempts = attempts.lock().unwrap();
    assert_eq!(attempts["S1"].len(), 3, "transient retried until success");
    assert_eq!(attempts["S2"].len(), 1, "unsupported is never retried");
    assert_eq!(attempts["S3"].len(), 4, "four attempts then give up");
    // Backoff doubles (20 ms, 40 ms) and Retry-After (90 ms) takes precedence over the 20 ms base.
    let gap = |s: &str, i: usize| attempts[s][i] - attempts[s][i - 1];
    // Each gap = the 60 ms job + the retry delay.
    assert!(gap("S1", 1) >= Duration::from_millis(78), "{:?}", gap("S1", 1));
    assert!(gap("S1", 2) >= Duration::from_millis(98), "{:?}", gap("S1", 2));
    assert!(gap("S4", 1) >= Duration::from_millis(148), "{:?}", gap("S4", 1));
    assert_eq!((result.done, result.failed, result.total, result.running), (6, 2, 8, false));
    assert!(result.log.iter().any(|l| l.contains("S3: FAILED 429 (gave up after 4 attempts)")));
    // Progress is throttled to roughly four per second (plus the first and final states).
    let n = notes.lock().unwrap().len();
    let seconds = started.elapsed().as_secs_f64();
    assert!((n as f64) <= seconds * 4.0 + 3.0, "{n} notifications in {seconds}s");
    assert!(!notes.lock().unwrap().last().unwrap().running);
    assert!(!coordinator.running());
}

#[tokio::test]
async fn daily_cancel_stops_new_requests_and_keeps_committed_work() {
    let coordinator = Arc::new(Coordinator::default());
    let committed = Arc::new(Mutex::new(Vec::<String>::new()));
    let c = coordinator.clone();
    let done = committed.clone();
    let run = coordinator.run(
        names(10),
        fast(),
        |_| {},
        move |symbol| {
            let (c, done) = (c.clone(), done.clone());
            async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                if symbol == "S2" {
                    assert!(c.cancel());
                }
                done.lock().unwrap().push(symbol);
                Ok("stored".into())
            }
        },
    );
    let p = run.await.unwrap();
    assert!(p.cancelled && !p.running);
    let n = committed.lock().unwrap().len();
    assert!((3..=5).contains(&n), "committed {n}");
    assert_eq!(p.done, n, "work that committed before the cancel stays counted");
    assert!(!coordinator.cancel(), "nothing running afterwards");
}

#[tokio::test]
async fn daily_second_run_while_active_is_refused() {
    let coordinator = Arc::new(Coordinator::default());
    let c = coordinator.clone();
    let first = tokio::spawn(async move {
        c.run(
            names(2),
            fast(),
            |_| {},
            |_| async {
                tokio::time::sleep(Duration::from_millis(80)).await;
                Ok("ok".to_string())
            },
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(coordinator.run(names(1), fast(), |_| {}, |_| async { Ok("x".to_string()) }).await.is_none());
    assert!(first.await.unwrap().is_some());
}

fn stored(bars: usize, last_fetch: Option<i64>, last_full: Option<i64>) -> DailyState {
    DailyState { bars, last_fetch_at: last_fetch, last_full_at: last_full, closed_thru: None }
}
fn outcome(pending: bool) -> DailyIngest {
    DailyIngest::Stored(DailyIngestOutcome { pending_revision: pending, ..Default::default() })
}

#[test]
fn daily_plan_and_due_follow_overlap_full_check_and_one_run_per_day() {
    let now = 1_800_000_000;
    assert_eq!(plan(None, now), Plan::Full);
    assert_eq!(plan(Some(&stored(100, Some(now), Some(now))), now), Plan::Full, "thin series: full");
    assert_eq!(plan(Some(&stored(5000, Some(now), Some(now - 86_400))), now), Plan::Incremental(INCREMENTAL_BARS));
    assert_eq!(
        plan(Some(&stored(5000, Some(now), Some(now - FULL_CHECK_SECS))), now),
        Plan::Full,
        "30-day full comparison"
    );
    assert_eq!(plan(Some(&stored(5000, Some(now), None)), now), Plan::Full);
    const { assert!(INCREMENTAL_BARS >= 7, "covers the seven-day overlap") };
    // At most one successful automatic run per symbol per UTC day.
    let day = now - now % 86_400;
    assert!(due(None, now));
    assert!(due(Some(&stored(5000, Some(day - 1), None)), now));
    assert!(!due(Some(&stored(5000, Some(day + 5), None)), day + 80_000));
    assert!(due(Some(&stored(5000, Some(day + 5), None)), day + 86_400));
}

#[tokio::test]
async fn daily_backfill_detects_nonadvancing_refetch() {
    // The provider keeps disagreeing with the stored series and never supplies a covering full
    // history: the job must stop with a visible error, not loop and not claim completion.
    let calls = Arc::new(Mutex::new(Vec::<usize>::new()));
    let c = calls.clone();
    let result = refresh_symbol(
        "AAPL US Equity",
        Some(stored(5000, Some(1), Some(1_799_000_000))),
        1_800_000_000,
        move |limit| {
            c.lock().unwrap().push(limit);
            async { Ok(outcome(true)) }
        },
    )
    .await;
    assert_eq!(*calls.lock().unwrap(), vec![INCREMENTAL_BARS, FULL_LIMIT], "one full same-source refetch, then stop");
    match result {
        Err(JobError::Permanent(why)) => assert!(why.contains("revision still pending"), "{why}"),
        other => panic!("{other:?}"),
    }
    // A full run that is still pending is not retried at all.
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    let result = refresh_symbol("AAPL US Equity", None, 1_800_000_000, move |_| {
        n2.fetch_add(1, Ordering::SeqCst);
        async { Ok(outcome(true)) }
    })
    .await;
    assert!(matches!(result, Err(JobError::Permanent(_))));
    assert_eq!(n.load(Ordering::SeqCst), 1);
    // A revision that the full refetch resolves is reported as applied.
    let step = Arc::new(AtomicUsize::new(0));
    let s2 = step.clone();
    let ok =
        refresh_symbol("AAPL US Equity", Some(stored(5000, Some(1), Some(1_799_000_000))), 1_800_000_000, move |_| {
            let first = s2.fetch_add(1, Ordering::SeqCst) == 0;
            async move {
                Ok(if first {
                    outcome(true)
                } else {
                    DailyIngest::Stored(DailyIngestOutcome { replaced: true, ..Default::default() })
                })
            }
        })
        .await
        .unwrap();
    assert!(ok.contains("REVISION APPLIED") && ok.contains("full"), "{ok}");
}

#[tokio::test]
async fn daily_rejected_fetch_is_a_permanent_failure_not_retried() {
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    let r = refresh_symbol("EURUSD Curncy", None, 1_800_000_000, move |_| {
        n2.fetch_add(1, Ordering::SeqCst);
        async { Ok(DailyIngest::Rejected("daily admission refused: invalid OHLC".into())) }
    })
    .await;
    assert!(matches!(r, Err(JobError::Permanent(ref w)) if w.contains("invalid OHLC")));
    assert_eq!(n.load(Ordering::SeqCst), 1);
}

#[test]
fn daily_watchlist_union_canonicalizes_cross_asset_symbols() {
    let listed = [
        "SPX Index",
        "^GSPC",
        "SPY US Equity",
        "SPY",
        "EURUSD Curncy",
        "US10Y Govt",
        "CL1 Comdty",
        "GC1 Comdty",
        "BTCUSD Curncy",
        "  ",
        "AAPL",
    ];
    let keys = configured_symbols(listed);
    for expected in [
        "SPX Index",
        "SPY US Equity",
        "EURUSD Curncy",
        "US10Y Govt",
        "CL1 Comdty",
        "GC1 Comdty",
        "BTCUSD Curncy",
        "AAPL US Equity",
    ] {
        assert!(keys.contains(&expected.to_string()), "{expected} missing from {keys:?}");
    }
    assert_eq!(keys.iter().filter(|k| k.starts_with("SPX")).count(), 1, "^GSPC aliases SPX Index: {keys:?}");
    assert!(!keys.iter().any(|k| k.contains("^GSPC")), "no fabricated ^GSPC entry: {keys:?}");
    assert_eq!(keys.iter().filter(|k| k.starts_with("SPY")).count(), 1);
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted, "deterministic order");
    assert_eq!(configured_symbols(listed.iter().rev().copied()), keys, "input order never matters");
}

#[test]
fn daily_cache_identity_ignores_read_clock() {
    // Native coalescing identity is (canonical symbol, timeframe, range end, limit): there is no
    // clock component, so repeated reads share one key. Source identity is the stable id below.
    let key = |sym: &str| (tr_data::security(sym).key(), tr_core::Timeframe::D1, None::<i64>, FULL_LIMIT);
    assert_eq!(key("AAPL"), key("AAPL US Equity"));
    let a = native_source("yahoo");
    assert_eq!(a, native_source("Yahoo"));
    assert_ne!(a.0, archive_source().0);
    assert_ne!(archive_source().0, whats_news_source("Whats-News HTTP archive: http://h/?token=SECRET").0);
    assert_ne!(
        whats_news_source("Whats-News HTTP archive: http://x").0,
        whats_news_source("Whats-News HTTP / SQLite archive: /p/db").0
    );
    assert!(!whats_news_source("Whats-News HTTP archive: http://h/?token=SECRET").0.contains("SECRET"));
    assert_eq!(native_source("treasury").1, tr_data::vault::daily::AdjustmentBasis::ProviderYieldV1);
    assert_eq!(native_source("some-new-vendor").1, tr_data::vault::daily::AdjustmentBasis::UnknownProviderBasisV1);
    assert_ne!(native_source("yahoo").0, native_source("alpaca:iex").0, "vendors never share a series");
}
