//! Worker-level Daily Vault tests: offline, in-memory or unique temp dirs, never the user's data.
use super::tests::{VaultProvider, shared, vault_provider};
use super::*;
use tr_data::vault::daily::{DailyExtent, DailyRead};

const SYMBOL: &str = "AAPL US Equity";

/// Constructed numeric fixture: the last `n` US sessions that are closed right now.
fn recent(n: usize) -> Vec<Bar> {
    let mut d = tr_data::history::calendar::last_expected_session(chrono::Utc::now());
    d = tr_data::history::calendar::previous_session(d);
    let mut dates = vec![];
    for _ in 0..n {
        dates.push(d);
        d = tr_data::history::calendar::previous_session(d);
    }
    dates.reverse();
    dates
        .into_iter()
        .enumerate()
        .map(|(i, d)| {
            let base = 100. + i as f64;
            Bar::new(
                d.and_hms_opt(14, 30, 0).unwrap().and_utc().timestamp(),
                base,
                base + 1.,
                base - 1.,
                base + 0.5,
                1000.,
            )
            .with_provenance(tr_core::Provenance::Real)
        })
        .collect()
}
struct Dir(PathBuf);
impl Dir {
    fn new(name: &str) -> Self {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "tr-dvault-w-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn bridge_off(s: &Shared) -> tr_data::whats_news::Bridge {
    let config = tr_data::whats_news::Config { enabled: false, ..Default::default() };
    s.whats_news.send_replace((0, config.clone()));
    config.bridge()
}
fn vault_summaries(s: &Shared) -> Vec<tr_data::vault::daily::DailySummary> {
    s.cache.lock().unwrap().as_ref().unwrap().daily_summaries(None).unwrap()
}
fn failing() -> Arc<VaultProvider> {
    let p = vault_provider(vec![]);
    p.fail.store(true, std::sync::atomic::Ordering::SeqCst);
    p
}

#[tokio::test]
async fn daily_all_sources_down_survives_restart() {
    let dir = Dir::new("restart");
    let path = dir.0.join("bars.sqlite3");
    let input = recent(8);
    {
        // First run: a verified fetch is written through at the native boundary.
        let provider = vault_provider(input.clone());
        let (s, _) = shared(provider, None);
        *s.cache.lock().unwrap() = Some(BarVault::open(&path).unwrap());
        let (bars, source, obs) = native_fetch(&s, SYMBOL, Timeframe::D1, None, 800).await.unwrap();
        assert_eq!((bars.len(), source.as_str()), (8, "yahoo"));
        assert!(matches!(obs.daily, Some(Ok(tr_data::vault::daily::DailyIngest::Stored(ref o))) if o.inserted == 8));
    }
    // Restart: every upstream stub fails; stored real bars come back from the vault alone.
    let provider = failing();
    let (s, _) = shared(provider.clone(), None);
    *s.cache.lock().unwrap() = Some(BarVault::open(&path).unwrap());
    let bridge = bridge_off(&s);
    let receipt = research_daily_receipt(&s, &bridge, SYMBOL).await.unwrap();
    assert_eq!(receipt.stage, crate::daily_research::Stage::Vault);
    let label = receipt.label();
    assert!(label.starts_with("VAULT (native:yahoo#quote-ohlcv-v1) CLOSED THRU "), "{label}");
    assert!(!label.contains("LIVE"));
    assert_eq!(receipt.bars.len(), 8);
    for (got, want) in receipt.bars.iter().zip(&input) {
        assert_eq!(
            (got.open, got.high, got.low, got.close, got.volume),
            (want.open, want.high, want.low, want.close, want.volume)
        );
        assert_eq!(got.provenance, tr_core::Provenance::Cached);
    }
    assert!(label.contains("upstream failed:") && label.contains("Native provider"));
    // research_history keeps structured vault metadata: not live, stored clock.
    let (_, info) = research_history(&s, SYMBOL, Timeframe::D1, 400, "test").await.unwrap();
    assert!(!info.live_ok && info.vault_bars == 8 && info.note.as_deref().unwrap_or("").contains("VAULT FALLBACK"));
    // Reading never advances the stored observation clock.
    let before = vault_summaries(&s)[0].last_fetch_at;
    research_daily_receipt(&s, &bridge, SYMBOL).await.unwrap();
    assert_eq!(vault_summaries(&s)[0].last_fetch_at, before);
}

#[tokio::test]
async fn daily_empty_vault_returns_actionable_error_without_generating() {
    let provider = failing();
    let (s, _) = shared(provider.clone(), Some(BarCache::open_in_memory().unwrap()));
    let bridge = bridge_off(&s);
    let error = research_daily_receipt(&s, &bridge, SYMBOL).await.err().unwrap().to_string();
    for text in [
        "Daily vault: no stored daily bars",
        "Local research archive",
        "Native provider",
        "VAULT DAILY REFRESH",
        "Nothing is substituted",
    ] {
        assert!(error.contains(text), "{error}");
    }
    assert!(
        s.cache.lock().unwrap().as_ref().unwrap().daily_summaries(None).unwrap().iter().all(|x| x.bars == 0),
        "no bars appear from nowhere"
    );
}

#[tokio::test]
async fn daily_every_acquisition_boundary_writes_through() {
    let (s, _) = shared(failing(), Some(BarCache::open_in_memory().unwrap()));
    let bridge = bridge_off(&s);
    // Local research archive via the resolver.
    // The archive stores UTC-midnight dates.
    let archive: Vec<Bar> = recent(6)
        .into_iter()
        .map(|mut b| {
            b.ts -= b.ts.rem_euclid(86_400);
            b
        })
        .collect();
    {
        let a = archive.clone();
        s.history.db(move |db| db.import_verified("AAPL", "Apple", &a)).await.unwrap();
    }
    let r = research_daily_receipt(&s, &bridge, SYMBOL).await.unwrap();
    assert_eq!(r.stage, crate::daily_research::Stage::Archive);
    let rows = vault_summaries(&s);
    let arch = rows.iter().find(|x| x.id.source.starts_with("archive:")).expect("archive series written");
    assert_eq!(arch.bars, 6);
    assert_eq!(
        arch.basis,
        tr_data::vault::daily::AdjustmentBasis::UnknownProviderBasisV1,
        "archive basis is not assumed"
    );
    // Native chart / research path with a different provider: a separate series, never a splice.
    let provider = vault_provider(recent(9));
    let (s2, _) = shared(provider, Some(BarCache::open_in_memory().unwrap()));
    native_fetch(&s2, SYMBOL, Timeframe::D1, None, 800).await.unwrap();
    let rows = vault_summaries(&s2);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].id.source.starts_with("native:yahoo#"));
    // Intraday responses never create daily rows.
    native_fetch(&s2, SYMBOL, Timeframe::M5, None, 100).await.ok();
    assert_eq!(vault_summaries(&s2).len(), 1);
    // The explicit refresh job goes through the same admission path.
    let provider = vault_provider(recent(12));
    let (s3, _) = shared(provider.clone(), Some(BarCache::open_in_memory().unwrap()));
    let report = daily_job(&s3, SYMBOL.into()).await.unwrap();
    assert!(report.contains("+12 new"), "{report}");
    assert_eq!(vault_summaries(&s3)[0].bars, 12);
    assert_eq!(vault_summaries(&s3)[0].coverage, "provider-limited", "native window is never full history");
}

#[tokio::test]
async fn daily_native_plan_ignores_full_archive_series() {
    use tr_data::vault::daily::DailyExtent;
    let (s, _) = shared(vault_provider(recent(12)), Some(BarCache::open_in_memory().unwrap()));
    let key = tr_data::security(SYMBOL).key();
    let at = chrono::Utc::now().timestamp();
    let archive = crate::daily_vault::build_fetch(
        &key,
        crate::daily_vault::archive_source(),
        recent(300),
        at,
        DailyExtent::FullAvailable,
    );
    persist_daily(&s, archive).await.unwrap();
    let k = key.clone();
    let any = daily_op(&s, move |v| v.daily_state(&k)).await.unwrap();
    assert!(any.as_ref().is_some_and(|st| st.last_full_at.is_some()), "archive is stored full");
    let k = key.clone();
    let native = daily_op(&s, move |v| v.daily_native_state(&k)).await.unwrap();
    assert!(native.is_none());
    assert_eq!(crate::daily_vault::plan(native.as_ref(), at), crate::daily_vault::Plan::Full);
}

#[tokio::test]
async fn daily_native_full_limit_ingest_sets_comparison_clock_and_plans_incremental() {
    let (s, _) = shared(vault_provider(recent(300)), Some(BarCache::open_in_memory().unwrap()));
    native_fetch(&s, SYMBOL, Timeframe::D1, None, crate::daily_vault::FULL_LIMIT).await.unwrap();
    let k = tr_data::security(SYMBOL).key();
    let st = daily_op(&s, move |v| v.daily_native_state(&k)).await.unwrap().expect("native state");
    let t = st.last_full_at.expect("comparison timestamp");
    assert!(st.bars >= 250);
    assert_eq!(crate::daily_vault::plan(Some(&st), t + 86_400), crate::daily_vault::Plan::Incremental(60));
    assert_eq!(
        crate::daily_vault::plan(Some(&st), t + crate::daily_vault::FULL_CHECK_SECS),
        crate::daily_vault::Plan::Full
    );
}

#[tokio::test]
async fn daily_write_failure_does_not_discard_upstream_success() {
    // No vault at all: persistence fails, the verified upstream result is still returned.
    let provider = vault_provider(recent(6));
    let (s, _) = shared(provider, None);
    let bridge = bridge_off(&s);
    let r = research_daily_receipt(&s, &bridge, SYMBOL).await.unwrap();
    assert_eq!(r.stage, crate::daily_research::Stage::Native);
    assert_eq!(r.bars.len(), 6);
    let label = r.label();
    assert!(label.contains("VAULT WRITE FAILED: vault unavailable"), "{label}");
    assert!(!label.contains("saved"));
}

#[tokio::test]
async fn daily_sample_mode_never_opens_vault_or_fetches() {
    let sample = Arc::new(VaultProvider {
        bars: Default::default(),
        calls: Default::default(),
        fail: Default::default(),
        sample: true,
    });
    let (s, rx) = shared(sample.clone(), Some(BarCache::open_in_memory().unwrap()));
    let bridge = bridge_off(&s);
    // Resolver, native boundary and every manual command refuse; nothing is written.
    assert!(research_daily_receipt(&s, &bridge, SYMBOL).await.is_err());
    for req in [
        Request::VaultDailyRefresh { symbols: None },
        Request::VaultDailyRefresh { symbols: Some(vec![SYMBOL.into()]) },
        Request::ConfigureDailyVault { symbols: vec![SYMBOL.into()], auto: true },
    ] {
        handle(&s, req).await;
    }
    assert_eq!(sample.calls.load(std::sync::atomic::Ordering::SeqCst), 0, "zero provider calls in sample mode");
    assert!(daily_op(&s, |v| v.daily_summaries(None)).await.is_err());
    assert!(s.cache.lock().unwrap().as_ref().unwrap().daily_summaries(None).unwrap().is_empty());
    let msgs: Vec<String> =
        rx.try_iter().filter_map(|r| if let Response::Status(t) = r { Some(t) } else { None }).collect();
    assert!(msgs.iter().any(|m| m.contains("VAULT OFF")), "{msgs:?}");
}

#[tokio::test]
async fn daily_refresh_request_stores_real_bars_and_reports_progress() {
    let provider = vault_provider(recent(10));
    let (s, rx) = shared(provider, Some(BarCache::open_in_memory().unwrap()));
    s.daily.configure(vec![SYMBOL.into()], false);
    handle(&s, Request::VaultDailyRefresh { symbols: None }).await;
    assert_eq!(vault_summaries(&s)[0].bars, 10);
    let all: Vec<Response> = rx.try_iter().collect();
    assert!(all.iter().any(|r| matches!(r, Response::VaultDailyProgress(p) if !p.running && p.done == 1)));
    assert!(all.iter().any(|r| matches!(r, Response::Status(t) if t.contains("1 refreshed"))));
    let DailyRead::Hit(h) = s.cache.lock().unwrap().as_ref().unwrap().daily_best(SYMBOL).unwrap() else { panic!() };
    assert_eq!(h.bars.len(), 10);
}
