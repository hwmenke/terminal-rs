//! Watchlist-scoped headlines: bounded fetch, cache, dedup, and chart catalysts.
//!
//! News is never pulled for the whole archive. Callers pass watched names or
//! scanner leaders; [`bound_symbols`] drops anything beyond [`MAX_SCOPED_SYMBOLS`].

use std::collections::HashMap;

use chrono::{TimeZone, Utc};
use serde::{Deserialize, Serialize};
use tr_core::{Bar, Timeframe};

use crate::NewsItem;

/// Names fetched for one watchlist / leaders pane (not the archive universe).
pub const MAX_SCOPED_SYMBOLS: usize = 12;
/// Headlines kept per name after a fetch.
pub const MAX_HEADLINES_PER_SYMBOL: usize = 8;
/// Deduplicated headlines shown in one pane.
pub const MAX_HEADLINES: usize = 40;
/// Parallel Yahoo / CoinDesk requests.
pub const FETCH_CONCURRENCY: usize = 4;
/// Serve a cached fetch without hitting the network.
pub const CACHE_TTL_SECS: i64 = 300;
/// Show a stale banner after this, even if the cache is still displayed.
pub const STALE_AFTER_SECS: i64 = 900;
/// Drop idle cache rows so a long session cannot grow without bound.
pub const MAX_CACHE_ENTRIES: usize = 32;

/// Which names a news pane is asking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum NewsScope {
    /// Selected watchlist group (bounded).
    #[default]
    Watchlist,
    /// Top rows of a visible SCAN pane (bounded).
    Leaders,
    /// One market key.
    Symbol,
    /// General market headlines (single query, not the archive).
    Top,
}

impl NewsScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::Watchlist => "Watchlist",
            Self::Leaders => "Scanner leaders",
            Self::Symbol => "Symbol",
            Self::Top => "Top market",
        }
    }
}

/// One name that failed inside an otherwise usable batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewsFailure {
    pub symbol: String,
    pub error: String,
}

/// Deduplicated headlines plus partial-failure / freshness state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewsBatch {
    pub items: Vec<NewsItem>,
    pub failures: Vec<NewsFailure>,
    /// Market keys actually queried (already bounded). Empty for top-market.
    pub queried: Vec<String>,
    /// Unix seconds when this batch was assembled.
    pub fetched_at: i64,
    /// Cache was older than [`STALE_AFTER_SECS`] or a refresh failed and leftovers remain.
    pub stale: bool,
    /// Sample / offline provider: no headlines are invented.
    pub sample: bool,
}

impl NewsBatch {
    pub fn empty(now: i64) -> Self {
        Self {
            items: Vec::new(),
            failures: Vec::new(),
            queried: Vec::new(),
            fetched_at: now,
            stale: false,
            sample: false,
        }
    }

    pub fn sample_isolated(now: i64) -> Self {
        Self { sample: true, ..Self::empty(now) }
    }

    pub fn is_partial(&self) -> bool {
        !self.failures.is_empty() && !self.items.is_empty()
    }

    pub fn is_stale_at(&self, now: i64) -> bool {
        self.stale || is_stale(self.fetched_at, now)
    }

    pub fn age_secs(&self, now: i64) -> i64 {
        (now - self.fetched_at).max(0)
    }
}

/// A headline that falls inside a loaded bar — a chart catalyst.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewsCatalyst {
    pub item: NewsItem,
    pub bar_ts: i64,
}

#[derive(Debug, Clone)]
struct CachedNews {
    items: Vec<NewsItem>,
    fetched_at: i64,
    error: Option<String>,
}

/// In-memory per-name headline cache. Top-market uses [`TOP_CACHE_KEY`].
#[derive(Debug, Default, Clone)]
pub struct NewsCache {
    entries: HashMap<String, CachedNews>,
}

pub const TOP_CACHE_KEY: &str = "*";

impl NewsCache {
    pub fn get(&self, key: &str, now: i64) -> Option<(&[NewsItem], i64, bool)> {
        let hit = self.entries.get(key)?;
        if hit.error.is_some() && hit.items.is_empty() {
            return None;
        }
        Some((&hit.items, hit.fetched_at, is_stale(hit.fetched_at, now)))
    }

    /// Fresh enough to skip the network.
    pub fn fresh(&self, key: &str, now: i64) -> Option<&[NewsItem]> {
        let hit = self.entries.get(key)?;
        if now - hit.fetched_at > CACHE_TTL_SECS || hit.error.is_some() {
            return None;
        }
        Some(&hit.items)
    }

    pub fn store(&mut self, key: impl Into<String>, items: Vec<NewsItem>, fetched_at: i64) {
        self.entries.insert(key.into(), CachedNews { items, fetched_at, error: None });
        self.evict();
    }

    pub fn store_error(&mut self, key: impl Into<String>, error: String, fetched_at: i64) {
        let key = key.into();
        if let Some(hit) = self.entries.get_mut(&key) {
            hit.error = Some(error);
            if hit.items.is_empty() {
                hit.fetched_at = fetched_at;
            }
        } else {
            self.entries.insert(key, CachedNews { items: Vec::new(), fetched_at, error: Some(error) });
        }
        self.evict();
    }

    fn evict(&mut self) {
        while self.entries.len() > MAX_CACHE_ENTRIES {
            let oldest = self.entries.iter().min_by_key(|(_, v)| v.fetched_at).map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                self.entries.remove(&k);
            } else {
                break;
            }
        }
    }
}

/// Keep at most [`MAX_SCOPED_SYMBOLS`] unique market keys, first-seen order.
pub fn bound_symbols(symbols: impl IntoIterator<Item = impl AsRef<str>>) -> Vec<String> {
    let mut out = Vec::new();
    for s in symbols {
        let key = crate::security(s.as_ref()).key();
        if key.is_empty() || out.iter().any(|x| x == &key) {
            continue;
        }
        out.push(key);
        if out.len() >= MAX_SCOPED_SYMBOLS {
            break;
        }
    }
    out
}

pub fn is_stale(fetched_at: i64, now: i64) -> bool {
    fetched_at > 0 && now - fetched_at > STALE_AFTER_SECS
}

/// Drop duplicate links and near-identical titles, newest first.
pub fn dedup_headlines(mut items: Vec<NewsItem>) -> Vec<NewsItem> {
    items.sort_by_key(|a| std::cmp::Reverse(a.ts));
    let mut seen_link = std::collections::HashSet::new();
    let mut seen_title = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if item.title.trim().is_empty() || item.link.trim().is_empty() {
            continue;
        }
        let link = canonical_link(&item.link);
        let title = normalize_title(&item.title);
        if !seen_link.insert(link) || !seen_title.insert(title) {
            continue;
        }
        out.push(item);
        if out.len() >= MAX_HEADLINES {
            break;
        }
    }
    out
}

pub fn normalize_title(title: &str) -> String {
    let mut t = title.trim().to_lowercase();
    for sep in [" - ", " — ", " | "] {
        if let Some((head, tail)) = t.rsplit_once(sep) {
            if tail.len() <= 24 {
                t = head.to_string();
            }
        }
    }
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn canonical_link(link: &str) -> String {
    let trimmed = link.trim();
    let (base, query) = trimmed.split_once('?').unwrap_or((trimmed, ""));
    let base = base.trim_end_matches('/');
    let keep: Vec<&str> = query
        .split('&')
        .filter(|p| {
            let key = p.split('=').next().unwrap_or("").to_ascii_lowercase();
            !key.is_empty() && !key.starts_with("utm_") && key != "s" && key != "ncid"
        })
        .collect();
    if keep.is_empty() {
        base.to_ascii_lowercase()
    } else {
        format!("{}?{}", base.to_ascii_lowercase(), keep.join("&"))
    }
}

/// Headlines whose publication time falls inside a loaded bar.
pub fn align_to_bars(items: &[NewsItem], bars: &[Bar], tf: Timeframe) -> Vec<NewsCatalyst> {
    if bars.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for item in items {
        if item.ts <= 0 {
            continue;
        }
        if let Some(bar) = bar_for_ts(bars, tf, item.ts) {
            out.push(NewsCatalyst { item: item.clone(), bar_ts: bar.ts });
        }
    }
    out
}

fn bar_for_ts(bars: &[Bar], tf: Timeframe, ts: i64) -> Option<&Bar> {
    let bucket = tf.bucket_start(ts);
    if let Some(bar) = bars.iter().find(|b| b.ts == bucket || tf.bucket_start(b.ts) == bucket) {
        return Some(bar);
    }
    let i = bars.partition_point(|b| b.ts <= ts);
    if i == 0 {
        return None;
    }
    let bar = &bars[i - 1];
    let end = bars.get(i).map(|b| b.ts).unwrap_or_else(|| bar.ts.saturating_add(tf.seconds()));
    (ts < end).then_some(bar)
}

pub fn format_pub(ts: i64, now: i64) -> String {
    if ts <= 0 {
        return "—".into();
    }
    let age = (now - ts).max(0);
    if age < 3600 {
        format!("{}m", age / 60)
    } else if age < 86_400 {
        format!("{}h", age / 3600)
    } else {
        Utc.timestamp_opt(ts, 0).single().map(|d| d.format("%b %d").to_string()).unwrap_or_else(|| "—".into())
    }
}

pub fn format_freshness(fetched_at: i64, now: i64) -> String {
    if fetched_at <= 0 {
        return "not yet fetched".into();
    }
    let age = (now - fetched_at).max(0);
    if age < 60 {
        "just now".into()
    } else if age < 3600 {
        format!("{}m ago", age / 60)
    } else if age < 86_400 {
        format!("{}h ago", age / 3600)
    } else {
        format!("{}d ago", age / 86_400)
    }
}

/// Merge per-name results, cap per name, then dedup across the batch.
pub fn assemble_batch(
    queried: Vec<String>,
    per_symbol: impl IntoIterator<Item = (String, Result<Vec<NewsItem>, String>)>,
    fetched_at: i64,
    stale: bool,
) -> NewsBatch {
    let mut items = Vec::new();
    let mut failures = Vec::new();
    for (symbol, result) in per_symbol {
        match result {
            Ok(mut rows) => {
                rows.truncate(MAX_HEADLINES_PER_SYMBOL);
                for item in &mut rows {
                    if item.tickers.is_empty() {
                        let ticker = crate::security(&symbol).ticker;
                        item.tickers.push(ticker);
                    }
                }
                items.extend(rows);
            }
            Err(error) => failures.push(NewsFailure { symbol, error }),
        }
    }
    NewsBatch { items: dedup_headlines(items), failures, queried, fetched_at, stale, sample: false }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tr_core::Provenance;

    fn item(title: &str, link: &str, ts: i64) -> NewsItem {
        NewsItem { title: title.into(), publisher: "T".into(), link: link.into(), ts, tickers: vec![] }
    }

    #[test]
    fn bound_symbols_dedup_and_cap() {
        let mut many: Vec<String> = (0..20).map(|i| format!("S{i} US Equity")).collect();
        many.insert(1, "s0".into());
        let got = bound_symbols(many);
        assert_eq!(got.len(), MAX_SCOPED_SYMBOLS);
        assert_eq!(got[0], "S0 US Equity");
        assert_eq!(got[1], "S1 US Equity");
    }

    #[test]
    fn dedup_drops_same_link_and_title() {
        let items = vec![
            item("Apple earnings beat", "https://Ex.com/a?utm_source=x", 100),
            item("Apple earnings beat - Reuters", "https://ex.com/a/", 90),
            item("Apple earnings beat", "https://other.com/a", 80),
            item("Different", "https://ex.com/b", 70),
        ];
        let out = dedup_headlines(items);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].title, "Apple earnings beat");
        assert_eq!(out[1].title, "Different");
    }

    #[test]
    fn assemble_keeps_partial_failures() {
        let batch = assemble_batch(
            vec!["AAPL US Equity".into(), "MSFT US Equity".into()],
            [
                ("AAPL US Equity".into(), Ok(vec![item("A", "https://a/1", 2)])),
                ("MSFT US Equity".into(), Err("HTTP 429".into())),
            ],
            50,
            false,
        );
        assert!(batch.is_partial());
        assert_eq!(batch.items[0].tickers, vec!["AAPL".to_string()]);
        assert_eq!(batch.failures[0].symbol, "MSFT US Equity");
        assert!(!batch.sample);
    }

    #[test]
    fn cache_ttl_and_eviction() {
        let mut c = NewsCache::default();
        c.store("A", vec![item("A", "https://a/1", 1)], 100);
        assert!(c.fresh("A", 100 + CACHE_TTL_SECS).is_some());
        assert!(c.fresh("A", 100 + CACHE_TTL_SECS + 1).is_none());
        let (_, _, stale) = c.get("A", 100 + STALE_AFTER_SECS + 1).unwrap();
        assert!(stale);
        for i in 0..MAX_CACHE_ENTRIES + 4 {
            c.store(format!("k{i}"), vec![], 200 + i as i64);
        }
        assert!(c.entries.len() <= MAX_CACHE_ENTRIES);
        assert!(c.get("A", 300).is_none());
    }

    #[test]
    fn catalysts_align_when_timestamp_falls_in_bar() {
        let bars = vec![
            Bar::new(1_000, 1.0, 1.0, 1.0, 1.0, 1.0).with_provenance(Provenance::Real),
            Bar::new(1_000 + 86_400, 1.0, 1.0, 1.0, 1.0, 1.0).with_provenance(Provenance::Real),
        ];
        let items = vec![
            item("In first day", "https://a/1", 1_000 + 3_600),
            item("In second day", "https://a/2", 1_000 + 86_400 + 10),
            item("Before range", "https://a/3", 10),
            item("No time", "https://a/4", 0),
        ];
        let hits = align_to_bars(&items, &bars, Timeframe::D1);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].bar_ts, 1_000);
        assert_eq!(hits[1].bar_ts, 1_000 + 86_400);
    }

    #[test]
    fn sample_batch_is_isolated() {
        let b = NewsBatch::sample_isolated(1);
        assert!(b.sample && b.items.is_empty() && b.failures.is_empty());
        assert!(!is_stale(1, 10));
        assert!(is_stale(1, 1 + STALE_AFTER_SECS + 1));
    }
}
