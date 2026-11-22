//! Offline daily-vault tests. Small constructed numeric fixtures exercise the mathematics and
//! the refusal paths; they are never runtime data. File-backed tests use unique temp dirs.
use super::daily::*;
use super::*;
use crate::history::calendar::next_session;

struct TestDir(std::path::PathBuf);
impl TestDir {
    fn new(name: &str) -> Self {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "tr-dvault-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn file(&self) -> std::path::PathBuf {
        self.0.join("bars.sqlite3")
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn sessions(start: &str, n: usize) -> Vec<NaiveDate> {
    let mut d: NaiveDate = start.parse().unwrap();
    let mut out = vec![d];
    while out.len() < n {
        d = next_session(d);
        out.push(d);
    }
    out
}
fn bar(d: NaiveDate, base: f64) -> Bar {
    // Provider stamp is the exchange open (14:30Z), not midnight: admission canonicalises the label.
    Bar::new(date_ts(d) + 52_200, base, base + 1., base - 1., base + 0.5, 1000. + base)
        .with_provenance(Provenance::Real)
}
fn series(start: &str, n: usize, base: f64) -> Vec<Bar> {
    sessions(start, n).into_iter().enumerate().map(|(i, d)| bar(d, base + i as f64)).collect()
}
fn ts(s: &str) -> i64 {
    s.parse::<chrono::DateTime<chrono::Utc>>().unwrap().timestamp()
}
fn sid(symbol: &str, source: &str) -> DailySeriesId {
    DailySeriesId { symbol: symbol.into(), source: source.into() }
}
fn fetch(symbol: &str, source: &str, bars: Vec<Bar>, observed: &str, extent: DailyExtent) -> DailyFetch {
    DailyFetch {
        id: sid(symbol, source),
        basis: AdjustmentBasis::ProviderQuoteOhlcvV1,
        close_policy: DailyClosePolicy::for_symbol(symbol),
        fetched_at: ts(observed),
        observed_at: ts(observed),
        upstream_fetched_at: None,
        bars,
        extent,
        full_compare: false,
    }
}
const AAPL: &str = "AAPL US Equity";
const OBS: &str = "2026-11-20T00:00:00Z";
fn stored(v: &BarVault, symbol: &str) -> DailyStored {
    match v.daily_best(symbol).unwrap() {
        DailyRead::Hit(s) => *s,
        DailyRead::Empty => panic!("empty"),
    }
}
fn outcome(r: DailyIngest) -> DailyIngestOutcome {
    match r {
        DailyIngest::Stored(o) => o,
        DailyIngest::Rejected(e) => panic!("rejected: {e}"),
    }
}
fn rows(v: &BarVault, table: &str) -> i64 {
    v.cache.conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0)).unwrap()
}

#[test]
fn daily_v3_migration_preserves_v2_and_rolls_back() {
    let dir = TestDir::new("mig");
    let path = dir.file();
    {
        let conn = Connection::open(&path).unwrap();
        BarCache::init_tables(&conn).unwrap();
        conn.execute_batch(V2_SCHEMA).unwrap();
        conn.execute_batch("INSERT INTO vault_series(id,symbol,tf,source,created_at,bars) VALUES(1,'AAPL US Equity','5m','yahoo',0,1);INSERT INTO vault_bars VALUES(1,1000,1,2,0.5,1.5,10,0,0,1);INSERT INTO bars(symbol,tf,ts,o,h,l,c,v) VALUES('MSFT','5m',5,1,1,1,1,1);PRAGMA user_version=2;").unwrap();
    }
    // Failure injection: a view squats on a v3 table name, so the additive index creation fails.
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE VIEW vault_daily_series AS SELECT 1 AS symbol, 1 AS closed_thru;").unwrap();
    }
    assert!(BarVault::open(&path).is_err());
    {
        let conn = Connection::open(&path).unwrap();
        assert_eq!(conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), 2);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE name='vault_daily_bars'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "rolled back: no partial v3 tables");
        conn.execute_batch("DROP VIEW vault_daily_series").unwrap();
    }
    let v = BarVault::open(&path).unwrap();
    assert!(v.enabled());
    assert_eq!(v.cache.conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), 3);
    assert_eq!(
        v.read(&SeriesId { symbol: AAPL.into(), tf: Timeframe::M5, source: "yahoo".into() }, None, 10).unwrap().len(),
        1
    );
    assert_eq!(v.load("MSFT", Timeframe::M5, 10).unwrap().len(), 1, "display cache preserved");
    assert_eq!(rows(&v, "vault_daily_series"), 0, "nothing promoted into the daily vault");
    drop(v);
    // Reopening v3 is idempotent.
    let v = BarVault::open(&path).unwrap();
    assert!(v.enabled());
    assert_eq!(v.cache.conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), 3);
}

#[test]
fn daily_future_schema_disabled_without_downgrade() {
    let dir = TestDir::new("future");
    let path = dir.file();
    let v = BarVault::open(&path).unwrap();
    v.cache.conn.pragma_update(None, "user_version", 99).unwrap();
    drop(v);
    let mut v = BarVault::open(&path).unwrap();
    assert!(!v.enabled());
    let f = fetch(AAPL, "native:yahoo#quote-ohlcv-v1", series("2026-11-02", 4, 100.), OBS, DailyExtent::Window);
    assert!(v.daily_ingest(&f).is_err());
    assert!(matches!(v.daily_best(AAPL).unwrap(), DailyRead::Empty));
    assert!(v.daily_summaries(None).unwrap().is_empty());
    drop(v);
    let conn = Connection::open(&path).unwrap();
    assert_eq!(conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0)).unwrap(), 99);
}

#[test]
fn daily_roundtrip_restart_preserves_receipt_and_hash() {
    let dir = TestDir::new("roundtrip");
    let path = dir.file();
    let src = "native:yahoo#quote-ohlcv-v1";
    let input = series("2026-11-02", 8, 100.);
    let hash;
    {
        let mut v = BarVault::open(&path).unwrap();
        let o = outcome(v.daily_ingest(&fetch(AAPL, src, input.clone(), OBS, DailyExtent::Window)).unwrap());
        assert_eq!((o.inserted, o.withheld), (8, 0));
        hash = o.content_hash;
    }
    let v = BarVault::open(&path).unwrap();
    let s = stored(&v, AAPL);
    assert_eq!(s.id.source, src);
    assert_eq!(s.basis, AdjustmentBasis::ProviderQuoteOhlcvV1);
    assert_eq!(s.closed_thru, "2026-11-11".parse::<NaiveDate>().unwrap());
    assert_eq!(s.content_hash, hash);
    assert_eq!(s.fetched_at, ts(OBS));
    assert_eq!(s.bars.len(), 8);
    for (stored, orig) in s.bars.iter().zip(&input) {
        assert_eq!(stored.ts, date_ts(chrono::DateTime::from_timestamp(orig.ts, 0).unwrap().date_naive()));
        assert_eq!(
            (stored.open, stored.high, stored.low, stored.close, stored.volume),
            (orig.open, orig.high, orig.low, orig.close, orig.volume)
        );
        assert_eq!(stored.provenance, Provenance::Cached);
    }
    assert_eq!(crate::stable_hash::bars_digest(&s.bars), hash, "streaming hash equals bars_digest");
    // Reading never refreshes the observation clock.
    let again = stored_at_clock(&v);
    assert_eq!(again, ts(OBS));
}
fn stored_at_clock(v: &BarVault) -> i64 {
    v.daily_summaries(None).unwrap()[0].last_fetch_at.unwrap()
}

#[test]
fn daily_rejects_tainted_batch_before_closed_trim() {
    let mut v = BarVault::open_in_memory().unwrap();
    let src = "native:yahoo#quote-ohlcv-v1";
    let good = || series("2026-11-02", 4, 100.);
    // The tainted row is in the still-open (trimmed) tail: trimming must not hide it.
    let mut tail_sample = good();
    tail_sample.push(bar("2026-11-30".parse().unwrap(), 50.).with_provenance(Provenance::Sample));
    let mut missing = good();
    missing[1].provenance = Provenance::Missing;
    let mut bad_ohlc = good();
    bad_ohlc[2].high = bad_ohlc[2].low - 1.;
    let mut nan = good();
    nan[0].close = f64::NAN;
    let mut neg_vol = good();
    neg_vol[3].volume = -1.;
    let mut conflict = good();
    let mut dup = conflict[1];
    dup.close += 0.25;
    dup.ts += 60;
    conflict.insert(2, dup);
    let mut descending = good();
    descending.swap(0, 3);
    let mut weekend = good();
    weekend.push(bar("2026-11-07".parse().unwrap(), 9.));
    for (name, bars) in [
        ("sample tail", tail_sample),
        ("missing provenance", missing),
        ("invalid ohlc", bad_ohlc),
        ("nan", nan),
        ("negative volume", neg_vol),
        ("conflicting duplicate", conflict),
        ("not ascending", descending),
        ("weekend label", weekend),
    ] {
        match v.daily_ingest(&fetch(AAPL, src, bars, OBS, DailyExtent::Window)).unwrap() {
            DailyIngest::Rejected(_) => {}
            DailyIngest::Stored(o) => panic!("{name} admitted: {o:?}"),
        }
    }
    assert_eq!(rows(&v, "vault_daily_bars"), 0);
    assert!(matches!(v.daily_best(AAPL).unwrap(), DailyRead::Empty));
    // An exact duplicate is deduplicated deterministically.
    let mut exact = good();
    exact.insert(2, exact[1]);
    let o = outcome(v.daily_ingest(&fetch(AAPL, src, exact, OBS, DailyExtent::Window)).unwrap());
    assert_eq!((o.inserted, o.deduped), (4, 1));
}

#[test]
fn daily_missing_volume_is_not_zero() {
    let mut v = BarVault::open_in_memory().unwrap();
    let mut bars = series("2026-11-02", 4, 100.);
    bars[1].volume = f64::NAN;
    bars[2].volume = 0.;
    v.daily_ingest(&fetch(AAPL, "native:x#quote-ohlcv-v1", bars, OBS, DailyExtent::Window)).unwrap();
    let nulls: i64 =
        v.cache.conn.query_row("SELECT COUNT(*) FROM vault_daily_bars WHERE v IS NULL", [], |r| r.get(0)).unwrap();
    assert_eq!(nulls, 1);
    let s = stored(&v, AAPL);
    assert!(s.bars[1].volume.is_nan());
    assert_eq!(s.bars[2].volume_value(), Some(0.), "a reported zero stays zero");
    let err = crate::stats::require_research_bars(&s.bars, "Daily research").unwrap_err();
    assert!(err.contains("invalid archived OHLCV"), "{err}");
}

fn admit_one(symbol: &str, date: &str, observed: &str) -> Result<Admitted, String> {
    admit(&fetch(
        symbol,
        "native:x#quote-ohlcv-v1",
        vec![bar(date.parse().unwrap(), 100.)],
        observed,
        DailyExtent::Window,
    ))
}

#[test]
fn daily_cash_close_dst_holiday_half_day() {
    assert!(admit_one(AAPL, "2026-11-11", "2026-11-11T21:14:59Z").is_err(), "withheld before cutoff");
    assert_eq!(admit_one(AAPL, "2026-11-11", "2026-11-11T21:15:00Z").unwrap().bars.len(), 1);
    assert!(admit_one(AAPL, "2026-07-06", "2026-07-06T20:14:59Z").is_err());
    assert_eq!(admit_one(AAPL, "2026-07-06", "2026-07-06T20:15:00Z").unwrap().bars.len(), 1);
    assert!(admit_one(AAPL, "2026-11-27", "2026-11-27T18:14:59Z").is_err());
    assert_eq!(admit_one(AAPL, "2026-11-27", "2026-11-27T18:15:00Z").unwrap().bars.len(), 1, "early-close bar");
    // Each row is tested, not only the last: a future-dated row anywhere is withheld.
    let mut b = series("2026-11-02", 3, 100.);
    b.push(bar("2026-11-30".parse().unwrap(), 9.));
    let a = admit(&fetch(AAPL, "native:x#quote-ohlcv-v1", b, "2026-11-11T21:15:00Z", DailyExtent::Window)).unwrap();
    assert_eq!((a.bars.len(), a.withheld), (3, 1));
    // UTC D1 (crypto): November 11 closes at November 12 00:00Z.
    assert!(admit_one("BTCUSD Curncy", "2026-11-11", "2026-11-11T23:59:59Z").is_err());
    assert_eq!(admit_one("BTCUSD Curncy", "2026-11-11", "2026-11-12T00:00:00Z").unwrap().bars.len(), 1);
}

#[test]
fn daily_non_cash_requires_finality_evidence() {
    for symbol in ["EURUSD Curncy", "CL1 Comdty", "US10Y Govt"] {
        assert_eq!(DailyClosePolicy::for_symbol(symbol), DailyClosePolicy::ConservativeSuccessorV1, "{symbol}");
    }
    assert_eq!(DailyClosePolicy::for_symbol(AAPL), DailyClosePolicy::UsCashV1);
    assert_eq!(DailyClosePolicy::for_symbol("BTCUSD Curncy"), DailyClosePolicy::UtcDailyV1);
    let bars = series("2026-11-09", 2, 1.1); // Mon, Tue
    let f = |obs: &str| fetch("EURUSD Curncy", "native:x#quote-ohlcv-v1", bars.clone(), obs, DailyExtent::Window);
    // Same UTC day as the last row: nothing beyond Monday may be final, and the last row stays pending.
    let a = admit(&f("2026-11-10T12:00:00Z")).unwrap();
    assert_eq!((a.bars.len(), a.withheld), (1, 1));
    // Even days later the final row has no successor evidence, so it stays pending.
    let a = admit(&f("2026-11-20T12:00:00Z")).unwrap();
    assert_eq!((a.bars.len(), a.withheld), (1, 1));
    // A single row can never be proven final.
    assert!(
        admit(&fetch("EURUSD Curncy", "s#v", bars[..1].to_vec(), "2026-11-20T12:00:00Z", DailyExtent::Window)).is_err()
    );
    // A weekend label is not rejected for non-cash: no US-calendar assumption is made.
    let wk = vec![bar("2026-11-07".parse().unwrap(), 5.), bar("2026-11-09".parse().unwrap(), 5.)];
    assert_eq!(
        admit(&fetch("EURUSD Curncy", "s#v", wk, "2026-11-20T12:00:00Z", DailyExtent::Window)).unwrap().bars.len(),
        1
    );
    let mut v = BarVault::open_in_memory().unwrap();
    v.daily_ingest(&f("2026-11-20T12:00:00Z")).unwrap();
    let s = &v.daily_summaries(None).unwrap()[0];
    assert_eq!(s.missing_sessions, None, "unknown calendar is never counted as missing");
}

#[test]
fn daily_sources_and_adjustment_bases_never_splice() {
    let mut v = BarVault::open_in_memory().unwrap();
    let adj = source_id("archive", "yahoo", AdjustmentBasis::YahooAdjustedOhlcRawVolumeV1);
    let native = source_id("native", "yahoo", AdjustmentBasis::ProviderQuoteOhlcvV1);
    let wn = source_id("whats-news", "http", AdjustmentBasis::UnknownProviderBasisV1);
    let mut fa = fetch(AAPL, &adj, series("2026-11-02", 5, 50.), OBS, DailyExtent::Window);
    fa.basis = AdjustmentBasis::YahooAdjustedOhlcRawVolumeV1;
    let fnat = fetch(AAPL, &native, series("2026-11-02", 7, 100.), OBS, DailyExtent::Window);
    let mut fwn = fetch(AAPL, &wn, series("2026-11-02", 6, 100.), OBS, DailyExtent::Window);
    fwn.basis = AdjustmentBasis::UnknownProviderBasisV1;
    for f in [&fa, &fnat, &fwn] {
        assert_eq!(outcome(v.daily_ingest(f).unwrap()).revised, 0, "different sources never compare or revise");
    }
    assert_eq!(v.daily_summaries(Some(AAPL)).unwrap().len(), 3);
    // Whole-series choice: greatest closed-through, then bars; the adjusted series is untouched.
    let best = stored(&v, AAPL);
    assert_eq!(best.id.source, native);
    assert_eq!(best.bars.len(), 7);
    let DailyRead::Hit(a) = v.daily_read(&sid(AAPL, &adj)).unwrap() else { panic!() };
    assert_eq!(a.bars[0].open, 50.);
    assert_eq!(a.basis, AdjustmentBasis::YahooAdjustedOhlcRawVolumeV1);
    // The same source id with a different basis is refused rather than re-baselined.
    let mut wrong = fetch(AAPL, &native, series("2026-11-02", 7, 100.), OBS, DailyExtent::Window);
    wrong.basis = AdjustmentBasis::YahooAdjustedOhlcRawVolumeV1;
    assert!(matches!(v.daily_ingest(&wrong).unwrap(), DailyIngest::Rejected(_)));
    // Equal closure and bars: lexicographic source id decides, never read time.
    let mut tie = fetch(AAPL, "aaa:tie#quote-ohlcv-v1", series("2026-11-02", 7, 7.), OBS, DailyExtent::Window);
    tie.basis = AdjustmentBasis::ProviderQuoteOhlcvV1;
    v.daily_ingest(&tie).unwrap();
    assert_eq!(stored(&v, AAPL).id.source, "aaa:tie#quote-ohlcv-v1");
    // Credentials and URLs never enter a source id.
    let id = source_id("whats-news", "https://host/x?token=SECRET", AdjustmentBasis::UnknownProviderBasisV1);
    assert!(!id.contains("SECRET") && !id.contains("//"), "{id}");
}

#[test]
fn daily_revision_tolerance_known_answers() {
    let rows = [
        (100.0, 100.00005, true),
        (100.0, 100.001, false),
        (1.1, 1.1000005, true),
        (1.1, 1.10001, false),
        (100.0, 50.0, false),
        (0.0, 1e-8, true), // equality boundary
        (0.0, 1.0000001e-8, false),
    ];
    for (a, b, agree) in rows {
        assert_eq!(price_agrees(a, b), agree, "{a} {b}");
        assert_eq!(price_agrees(b, a), agree, "symmetric {a} {b}");
    }
    for (a, b, agree) in [
        (1_000_000.0, 1_000_000.25, true),
        (1_000_000.0, 1_000_001.0, false),
        (0.0, 0.5, true),
        (0.0, 0.5000001, false),
    ] {
        assert_eq!(volume_agrees(Some(a), Some(b)), agree, "{a} {b}");
        assert_eq!(volume_agrees(Some(b), Some(a)), agree);
    }
    assert!(volume_agrees(None, None));
    assert!(!volume_agrees(None, Some(1.)) && !volume_agrees(Some(1.), None));
    // On agreement stored values are kept exactly (no churn from rounding noise).
    let mut v = BarVault::open_in_memory().unwrap();
    let src = "native:t#quote-ohlcv-v1";
    v.daily_ingest(&fetch(AAPL, src, series("2026-11-02", 4, 100.), OBS, DailyExtent::Window)).unwrap();
    let before = stored(&v, AAPL);
    let mut noisy = series("2026-11-02", 4, 100.);
    noisy[1].close += 0.00005;
    let o = outcome(v.daily_ingest(&fetch(AAPL, src, noisy, "2026-11-21T00:00:00Z", DailyExtent::Window)).unwrap());
    assert_eq!((o.confirmed, o.revised, o.generation), (4, 0, 1));
    let after = stored(&v, AAPL);
    assert_eq!(after.content_hash, before.content_hash);
    assert_eq!(after.bars[1].close, before.bars[1].close);
    assert_eq!(after.fetched_at, ts("2026-11-21T00:00:00Z"), "confirmation advances fetch metadata");
}

fn revised_window() -> Vec<Bar> {
    let mut w = series("2026-11-04", 3, 100.0 + 2.);
    w[0].high += 6.;
    w[0].close += 5.; // material disagreement on 2026-11-04
    w
}

#[test]
fn daily_revision_failure_keeps_old_generation() {
    let mut v = BarVault::open_in_memory().unwrap();
    let src = "native:t#quote-ohlcv-v1";
    let o =
        outcome(v.daily_ingest(&fetch(AAPL, src, series("2026-11-02", 6, 100.), OBS, DailyExtent::Window)).unwrap());
    let old_hash = o.content_hash;
    let p = outcome(v.daily_ingest(&fetch(AAPL, src, revised_window(), OBS, DailyExtent::Window)).unwrap());
    assert!(p.pending_revision && !p.replaced);
    let s = stored(&v, AAPL);
    assert_eq!((s.generation, s.content_hash, s.bars.len()), (1, old_hash, 6));
    assert!(s.warnings.iter().any(|w| w.contains("PENDING REVISION")));
    assert_eq!(v.daily_summaries(None).unwrap()[0].pending_revisions, 1);
    // Injected failure in the middle of the replacement transaction.
    v.cache.conn.execute_batch("CREATE TEMP TRIGGER boom BEFORE INSERT ON vault_daily_bars WHEN NEW.c > 5000 BEGIN SELECT RAISE(ABORT,'boom'); END;").unwrap();
    let mut full = series("2026-11-02", 6, 100.);
    full[0].open = 99.;
    full[0].high = 9000.;
    full[0].close = 5001.;
    assert!(v.daily_ingest(&fetch(AAPL, src, full, OBS, DailyExtent::FullAvailable)).is_err());
    let s = stored(&v, AAPL);
    assert_eq!((s.generation, s.content_hash, s.bars.len()), (1, old_hash, 6), "old active generation intact");
    assert_eq!(v.daily_summaries(None).unwrap()[0].pending_revisions, 1, "pending status preserved");
}

#[test]
fn daily_revision_commit_is_atomic_and_idempotent() {
    let mut v = BarVault::open_in_memory().unwrap();
    let src = "native:t#quote-ohlcv-v1";
    let old =
        outcome(v.daily_ingest(&fetch(AAPL, src, series("2026-11-02", 6, 100.), OBS, DailyExtent::Window)).unwrap());
    // The same disagreement twice produces one pending revision and one event.
    for _ in 0..2 {
        assert!(
            outcome(v.daily_ingest(&fetch(AAPL, src, revised_window(), OBS, DailyExtent::Window)).unwrap())
                .pending_revision
        );
    }
    assert_eq!(rows(&v, "vault_daily_revisions"), 1);
    let events = |v: &BarVault| -> i64 {
        v.cache
            .conn
            .query_row("SELECT COUNT(*) FROM vault_events WHERE kind='daily_revision_pending'", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(events(&v), 1);
    // Full replacement from the same source: one generation, one applied revision, old hash kept.
    let mut full = series("2026-11-02", 7, 100.);
    full[2].high += 6.;
    full[2].close += 5.;
    let applied = outcome(
        v.daily_ingest(&fetch(AAPL, src, full.clone(), "2026-11-21T00:00:00Z", DailyExtent::FullAvailable)).unwrap(),
    );
    assert!(applied.replaced && applied.generation == 2);
    assert_ne!(applied.content_hash, old.content_hash);
    let revs = v.daily_revisions(None, 10).unwrap();
    assert_eq!(revs.len(), 1);
    assert!(revs[0].applied && revs[0].old_hash == old.content_hash && revs[0].new_hash == applied.content_hash);
    assert_eq!((revs[0].old_generation, revs[0].new_generation), (1, Some(2)));
    let s = stored(&v, AAPL);
    assert_eq!((s.generation, s.revisions, s.bars.len()), (2, 1, 7));
    assert!(s.warnings.is_empty());
    // Replaying the same replacement is a confirmation: no second generation or event.
    let again =
        outcome(v.daily_ingest(&fetch(AAPL, src, full, "2026-11-21T00:00:00Z", DailyExtent::FullAvailable)).unwrap());
    assert!(!again.replaced && again.generation == 2 && again.confirmed == 7);
    assert_eq!(rows(&v, "vault_daily_revisions"), 1);
    // A replacement that omits stored dates is not applied.
    let short = series("2026-11-03", 3, 500.);
    let o =
        outcome(v.daily_ingest(&fetch(AAPL, src, short, "2026-11-21T00:00:00Z", DailyExtent::FullAvailable)).unwrap());
    assert!(o.pending_revision && !o.replaced);
    assert_eq!(stored(&v, AAPL).generation, 2);
}

#[test]
fn daily_two_disagreeing_windows_keep_one_pending_and_full_replacement_applies() {
    let mut v = BarVault::open_in_memory().unwrap();
    let src = "native:t#quote-ohlcv-v1";
    v.daily_ingest(&fetch(AAPL, src, series("2026-11-02", 6, 100.), OBS, DailyExtent::Window)).unwrap();
    // Two different windows disputing the same date (2026-11-04) with different content.
    let mut second = series("2026-11-03", 4, 101.);
    second[1].high += 9.;
    second[1].close += 8.;
    assert!(
        outcome(v.daily_ingest(&fetch(AAPL, src, revised_window(), OBS, DailyExtent::Window)).unwrap())
            .pending_revision
    );
    assert!(outcome(v.daily_ingest(&fetch(AAPL, src, second, OBS, DailyExtent::Window)).unwrap()).pending_revision);
    assert_eq!(rows(&v, "vault_daily_revisions"), 1, "at most one pending row per series");
    let pending_events: i64 = v
        .cache
        .conn
        .query_row("SELECT COUNT(*) FROM vault_events WHERE kind='daily_revision_pending'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(pending_events, 1);
    let mut full = series("2026-11-02", 7, 100.);
    full[2].high += 6.;
    full[2].close += 5.;
    let applied =
        outcome(v.daily_ingest(&fetch(AAPL, src, full, "2026-11-21T00:00:00Z", DailyExtent::FullAvailable)).unwrap());
    assert!(applied.replaced && applied.generation == 2);
    let revs = v.daily_revisions(None, 10).unwrap();
    assert_eq!(revs.len(), 1);
    assert!(revs[0].applied && revs[0].new_hash == applied.content_hash);
    assert_eq!(stored(&v, AAPL).bars.len(), 7);
    assert!(stored(&v, AAPL).warnings.is_empty(), "no pending revision remains");
}

#[test]
fn daily_covering_native_window_applies_pending_revision_and_stays_provider_limited() {
    let mut v = BarVault::open_in_memory().unwrap();
    let src = "native:t#quote-ohlcv-v1";
    v.daily_ingest(&fetch(AAPL, src, series("2026-11-02", 6, 100.), OBS, DailyExtent::Window)).unwrap();
    // Non-covering overlap that disputes 2026-11-04 (omits older stored dates): pending only.
    let mut part = series("2026-11-03", 4, 101.);
    part[1].high += 9.;
    part[1].close += 8.;
    let o = outcome(v.daily_ingest(&fetch(AAPL, src, part, OBS, DailyExtent::Window)).unwrap());
    assert!(o.pending_revision && !o.replaced);
    assert_eq!(stored(&v, AAPL).bars.len(), 6, "old series intact");
    // A larger window covering every stored date applies the revision and appends new dates.
    let mut cover = series("2026-11-02", 8, 100.);
    cover[2].high += 9.;
    cover[2].close += 8.;
    let o = outcome(v.daily_ingest(&fetch(AAPL, src, cover, "2026-11-21T00:00:00Z", DailyExtent::Window)).unwrap());
    assert!(o.replaced && !o.pending_revision && o.generation == 2);
    let s = stored(&v, AAPL);
    assert_eq!((s.generation, s.bars.len()), (2, 8));
    assert!(s.warnings.is_empty());
    let sum = &v.daily_summaries(None).unwrap()[0];
    assert_eq!((sum.coverage.as_str(), sum.pending_revisions), ("provider-limited", 0));
    assert!(v.daily_state(AAPL).unwrap().unwrap().last_full_at.is_none(), "window never claims full");
}

#[test]
fn daily_full_compare_records_comparison_clock_without_full_coverage() {
    let mut v = BarVault::open_in_memory().unwrap();
    let src = "native:t#quote-ohlcv-v1";
    let mut f = fetch(AAPL, src, series("2026-11-02", 6, 100.), OBS, DailyExtent::Window);
    f.full_compare = true;
    v.daily_ingest(&f).unwrap();
    assert!(v.daily_native_state(AAPL).unwrap().unwrap().last_full_at.is_some());
    assert_eq!(v.daily_summaries(None).unwrap()[0].coverage, "provider-limited");
}

#[test]
fn daily_prune_leaves_full_history_untouched() {
    let mut v = BarVault::open_in_memory().unwrap();
    v.daily_ingest(&fetch(AAPL, "native:t#quote-ohlcv-v1", series("2026-01-02", 100, 100.), OBS, DailyExtent::Window))
        .unwrap();
    let before = stored(&v, AAPL);
    assert!(v.prune(ts(OBS)).is_ok());
    assert!(v.drop_series(AAPL, None, ts(OBS)).is_ok(), "intraday DROP scope only");
    let after = stored(&v, AAPL);
    assert_eq!((after.content_hash, after.bars.len()), (before.content_hash, 100));
    assert_eq!(rows(&v, "vault_daily_bars"), 100);
    let msg = v.verify(ts(OBS)).unwrap();
    assert!(msg.contains("1 daily series content-hash ok"), "{msg}");
}

#[test]
fn daily_snapshot_reads_no_ohlc_rows_and_verify_detects_tampering() {
    let mut v = BarVault::open_in_memory().unwrap();
    for i in 0..5 {
        let symbol = format!("S{i} US Equity");
        v.daily_ingest(&fetch(
            &symbol,
            "native:t#quote-ohlcv-v1",
            series("2026-10-01", 20, 10.),
            OBS,
            DailyExtent::Window,
        ))
        .unwrap();
    }
    v.daily_note_failure("EURUSD Curncy", None, ts(OBS), "offline").unwrap();
    DAILY_ROWS_READ.with(|c| c.set(0));
    let snap = v.snapshot(None, ts(OBS)).unwrap();
    assert_eq!(DAILY_ROWS_READ.with(|c| c.get()), 0, "snapshot must not read daily OHLC rows");
    assert_eq!(snap.daily.len(), 6);
    let fx = snap.daily.iter().find(|s| s.id.symbol == "EURUSD Curncy").unwrap();
    assert_eq!((fx.bars, fx.last_error.as_deref()), (0, Some("offline")));
    let ok = snap.daily.iter().find(|s| s.id.symbol == "S0 US Equity").unwrap();
    assert_eq!(
        (ok.bars, ok.missing_sessions, ok.first_date.is_some(), ok.last_date == ok.closed_thru),
        (20, Some(0), true, true)
    );
    // Tampering with a stored row is caught by the content hash and refused on read.
    v.cache.conn.execute("UPDATE vault_daily_bars SET c=c+1 WHERE series_id=1 AND date=(SELECT MIN(date) FROM vault_daily_bars WHERE series_id=1)", []).unwrap();
    assert!(v.daily_best("S0 US Equity").is_err());
    assert!(v.verify(ts(OBS)).unwrap().contains("DAILY CONTENT CHECK FAILED"));
}

#[test]
fn daily_missing_sessions_are_counted_not_filled() {
    let mut v = BarVault::open_in_memory().unwrap();
    let mut b = series("2026-11-02", 8, 100.);
    b.remove(3); // one internal gap
    let o = outcome(v.daily_ingest(&fetch(AAPL, "native:t#quote-ohlcv-v1", b, OBS, DailyExtent::Window)).unwrap());
    assert_eq!(o.inserted, 7);
    assert_eq!(v.daily_summaries(None).unwrap()[0].missing_sessions, Some(1));
    assert_eq!(stored(&v, AAPL).bars.len(), 7, "the gap stays a gap");
}
