//! Worker-only, whole-series daily source selection. Never reads display caches.
//! Order: local research archive -> Whats-News -> native provider -> VAULT (stored real bars).
//! One source supplies the whole series; sources are never concatenated.
use chrono::NaiveDate;
use tr_core::{Bar, Timeframe};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    Archive,
    WhatsNews,
    Native,
    /// Stored verified real bars: always last, never written back into itself.
    Vault,
}
impl Stage {
    pub const ORDER: [Stage; 4] = [Stage::Archive, Stage::WhatsNews, Stage::Native, Stage::Vault];
    pub fn name(self) -> &'static str {
        match self {
            Stage::Archive => "Local research archive",
            Stage::WhatsNews => "Whats-News (HTTP / SQLite)",
            Stage::Native => "Native provider",
            Stage::Vault => "Daily vault",
        }
    }
}

/// What the vault stage knew about the stored series (structured, not parsed from prose).
#[derive(Clone, Debug)]
pub(crate) struct StoredMeta {
    pub original_source: String,
    pub basis: &'static str,
    pub fetched_at: i64,
    pub closed_thru: NaiveDate,
    pub generation: u64,
    pub revisions: u32,
    pub warnings: Vec<String>,
}

pub(crate) struct Acquired {
    pub bars: Vec<Bar>,
    pub source: String,
    pub stored: Option<StoredMeta>,
    /// Persistence warnings gathered before validation (e.g. write-through at the native boundary).
    pub warnings: Vec<String>,
}
impl Acquired {
    pub fn upstream(bars: Vec<Bar>, source: impl Into<String>) -> Self {
        Self { bars, source: source.into(), stored: None, warnings: vec![] }
    }
}

pub(crate) struct Receipt {
    pub bars: Vec<Bar>,
    pub source: String,
    /// Stable id of the accepted source: `archive`, `whats-news` or the native provider id.
    pub provider_id: String,
    pub fallback: Vec<String>,
    pub observed_at: i64,
    pub stage: Stage,
    pub stored: Option<StoredMeta>,
    pub warnings: Vec<String>,
}
impl Receipt {
    /// Structured successful-source metadata, kept apart from the failed-attempt chain.
    pub fn summary(&self) -> tr_data::source_receipt::SourceReceipt {
        let mut s = tr_data::source_receipt::SourceReceipt::new(
            &self.provider_id,
            self.bars.len(),
            self.bars.last().map(|b| b.ts),
        );
        s.observed_at = Some(self.observed_at);
        s.attempts = self.fallback.clone();
        s.detail = self.label();
        s
    }
    pub fn label(&self) -> String {
        let end = self
            .bars
            .last()
            .and_then(|b| chrono::DateTime::from_timestamp(b.ts, 0))
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        let tail = |this: &Self| {
            let mut s = String::new();
            for w in &this.warnings {
                s.push_str(&format!(" · {w}"));
            }
            if !this.fallback.is_empty() {
                s.push_str(&format!(
                    " · {}: {}",
                    if this.stored.is_some() { "upstream failed" } else { "fallback" },
                    this.fallback.join("; ")
                ));
            }
            s
        };
        if let Some(m) = &self.stored {
            // Closure comes from the stored evidence, never from retrieval time. Not LIVE.
            let observed = chrono::DateTime::from_timestamp(self.observed_at, 0).map(|d| d.date_naive());
            let stale = observed.map(|o| (o - m.closed_thru).num_days().max(0)).unwrap_or(0);
            return format!(
                "VAULT ({}) CLOSED THRU {} · basis {} · {} bars · saved {} · {}d behind today · {} revisions{}{}",
                m.original_source,
                m.closed_thru,
                m.basis,
                self.bars.len(),
                tr_data::stats::bar_date(m.fetched_at),
                stale,
                m.revisions,
                m.warnings.iter().map(|w| format!(" · {w}")).collect::<String>(),
                tail(self),
            );
        }
        format!(
            "{} · CLOSED THRU {} · {} bars · observed {}{}",
            self.source,
            end,
            self.bars.len(),
            tr_data::stats::bar_date(self.observed_at),
            tail(self)
        )
    }
}

/// Sentence appended to a daily-research failure only when the Whats-News stage failed because
/// the service was unreachable/unconfigured (not for "no history for this ticker"). Panes and the
/// footer test for it with [`error_needs_whats_news`]; nothing else in an error text counts.
pub(crate) const WHATS_NEWS_OFFLINE_NOTE: &str = "Whats-News was unreachable for this request.";

/// Failure of the Whats-News stage that is a service outage (as opposed to the ticker simply
/// having no history there). Raised by the worker's Whats-News fetch and read by [`resolve`].
#[derive(Debug)]
pub(crate) struct WhatsNewsUnreachable(pub String);
impl std::fmt::Display for WhatsNewsUnreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for WhatsNewsUnreachable {}

/// Structured result of an exhausted daily resolution.
#[derive(Debug)]
pub(crate) struct DailyUnavailable {
    pub message: String,
    /// The Whats-News stage failed on reachability, so an outage plausibly caused this result.
    pub needs_whats_news: bool,
}
impl std::fmt::Display for DailyUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)?;
        if self.needs_whats_news {
            write!(f, " {WHATS_NEWS_OFFLINE_NOTE}")?;
        }
        Ok(())
    }
}
impl std::error::Error for DailyUnavailable {}

/// True when this resolver failure was affected by a Whats-News outage.
pub(crate) fn needs_whats_news(error: &anyhow::Error) -> bool {
    error.downcast_ref::<DailyUnavailable>().is_some_and(|e| e.needs_whats_news)
}

/// Pane/footer check on the error text a worker response carried (the text embeds the resolver's
/// structured flag as [`WHATS_NEWS_OFFLINE_NOTE`]; an unknown ticker never contains it).
pub(crate) fn error_needs_whats_news(text: &str) -> bool {
    text.contains(WHATS_NEWS_OFFLINE_NOTE)
}

fn provider_id_for(stage: Stage, source: &str) -> String {
    match stage {
        Stage::Archive => "archive".to_string(),
        Stage::WhatsNews => "whats-news".to_string(),
        Stage::Native => source.trim_end_matches(" (native provider)").to_ascii_lowercase(),
        Stage::Vault => "vault".to_string(),
    }
}

/// `fetch` acquires a whole series for a stage; `persist` is told about each validated upstream
/// response (before the closed trim) and returns a warning if storing it failed. The upstream
/// result is returned even when persistence fails.
pub(crate) async fn resolve<F, Fut, P, PFut>(
    key: &str,
    observed_at: i64,
    mut fetch: F,
    mut persist: P,
) -> anyhow::Result<Receipt>
where
    F: FnMut(Stage) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Acquired>>,
    P: FnMut(Stage, Vec<tr_core::Bar>, String) -> PFut,
    PFut: std::future::Future<Output = Option<String>>,
{
    let mut fallback = Vec::new();
    let mut needs_whats_news = false;
    for stage in Stage::ORDER {
        let result = async {
            let got = fetch(stage).await?;
            // Whole-response validation first: trimming can never hide a tainted row.
            tr_data::stats::require_research_bars(&got.bars, "Daily research").map_err(anyhow::Error::msg)?;
            let mut warnings = got.warnings;
            let mut bars = got.bars;
            if stage != Stage::Vault {
                if let Some(w) = persist(stage, bars.clone(), got.source.clone()).await {
                    warnings.push(w);
                }
                let n =
                    tr_data::closed::closed_len(Timeframe::D1, &bars, tr_data::security(key).class, Some(observed_at));
                bars.truncate(n);
            }
            anyhow::ensure!(!bars.is_empty(), "no verified closed daily bars");
            Ok::<_, anyhow::Error>((bars, got.source, got.stored, warnings))
        }
        .await;
        match result {
            Ok((bars, source, stored, warnings)) => {
                let provider_id = provider_id_for(stage, &source);
                return Ok(Receipt { bars, source, provider_id, fallback, observed_at, stage, stored, warnings });
            }
            Err(e) => {
                needs_whats_news |= stage == Stage::WhatsNews && e.downcast_ref::<WhatsNewsUnreachable>().is_some();
                fallback.push(format!("{}: {e:#}", stage.name()));
            }
        }
    }
    Err(anyhow::Error::new(DailyUnavailable {
        message: format!(
            "Daily research unavailable for {key}. {}. Retry, or open Settings > Whats-News; HIST can download verified daily history; VAULT DAILY REFRESH stores it for outages. Nothing is substituted.",
            fallback.join("; ")
        ),
        needs_whats_news,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tr_core::Provenance;
    fn bars(provenance: Provenance) -> Vec<Bar> {
        (0..4)
            .map(|i| Bar::new(1_735_862_400 + i * 86_400, 100., 101., 99., 100., 10.).with_provenance(provenance))
            .collect()
    }
    fn meta() -> StoredMeta {
        StoredMeta {
            original_source: "native:yahoo#quote-ohlcv-v1".into(),
            basis: "provider quote OHLCV",
            fetched_at: 1_736_000_000,
            closed_thru: "2025-01-06".parse().unwrap(),
            generation: 1,
            revisions: 0,
            warnings: vec![],
        }
    }
    async fn no_persist(_: Stage, _: Vec<Bar>, _: String) -> Option<String> {
        None
    }
    #[tokio::test]
    async fn daily_resolver_archive_whats_native_vault_order() {
        for (accepted, stage) in Stage::ORDER.into_iter().enumerate() {
            let mut calls = Vec::new();
            let receipt = resolve(
                "SPX Index",
                1_740_000_000,
                |s| {
                    calls.push(s);
                    async move {
                        if s != stage {
                            anyhow::bail!("source offline");
                        }
                        let mut a = Acquired::upstream(bars(Provenance::Real), format!("source {s:?}"));
                        if s == Stage::Vault {
                            a.bars = bars(Provenance::Cached);
                            a.stored = Some(meta());
                        }
                        Ok(a)
                    }
                },
                no_persist,
            )
            .await
            .unwrap();
            assert_eq!(calls, Stage::ORDER[..=accepted].to_vec(), "whole-series stages stop at the first success");
            assert_eq!(receipt.stage, stage);
            assert_eq!(receipt.bars.len(), 4);
            assert_eq!(receipt.fallback.len(), accepted);
            assert!(receipt.label().contains("CLOSED THRU 2025-01-06"));
            if stage == Stage::Vault {
                assert!(
                    receipt.label().starts_with("VAULT (native:yahoo#quote-ohlcv-v1) CLOSED THRU 2025-01-06"),
                    "{}",
                    receipt.label()
                );
                assert!(!receipt.label().contains("LIVE"));
                assert!(receipt.label().contains("upstream failed:"));
            }
        }
    }
    #[tokio::test]
    async fn rejected_inputs_never_spliced_or_trimmed_into_acceptance() {
        let receipt = resolve(
            "AAPL",
            1_740_000_000,
            |stage| async move {
                let mut data = bars(Provenance::Real);
                if stage == Stage::Archive {
                    data.last_mut().unwrap().provenance = Provenance::Sample;
                }
                if stage == Stage::WhatsNews {
                    data[1].close = f64::NAN;
                }
                Ok(Acquired::upstream(data, format!("source {stage:?}")))
            },
            no_persist,
        )
        .await
        .unwrap();
        assert_eq!(receipt.source, "source Native");
        assert_eq!(receipt.fallback.len(), 2);
        assert_eq!(receipt.bars.len(), 4);
        assert!(receipt.bars.iter().all(|b| b.provenance == Provenance::Real));
    }
    #[tokio::test]
    async fn daily_empty_vault_returns_actionable_error() {
        let error =
            resolve("SPX Index", 1_740_000_000, |stage| async move { anyhow::bail!("failure {stage:?}") }, no_persist)
                .await
                .err()
                .unwrap()
                .to_string();
        for text in [
            "Local research archive",
            "Whats-News",
            "Native provider",
            "Daily vault",
            "failure Archive",
            "failure WhatsNews",
            "failure Native",
            "failure Vault",
            "Retry",
            "Settings > Whats-News",
            "VAULT DAILY REFRESH",
            "Nothing is substituted",
        ] {
            assert!(error.contains(text), "{error}");
        }
    }
    #[tokio::test]
    async fn structured_receipt_keeps_success_apart_from_attempts() {
        let receipt = resolve(
            "SPX Index",
            1_740_000_000,
            |stage| async move {
                if stage != Stage::Native {
                    anyhow::bail!("no daily history for stage {stage:?}");
                }
                Ok(Acquired::upstream(bars(Provenance::Real), "yahoo (native provider)"))
            },
            no_persist,
        )
        .await
        .unwrap();
        let s = receipt.summary();
        assert_eq!((s.provider_id.as_str(), s.provider.as_str(), s.bars), ("yahoo", "Yahoo", 4));
        assert_eq!(s.attempts.len(), 2);
        let chip = s.chip();
        assert!(chip.starts_with("Yahoo · thru Jan 6") && chip.contains("4 bars"), "{chip}");
        assert!(!chip.contains("fallback") && !chip.contains("no daily history"), "{chip}");
        // The complete diagnostic text is preserved for hover, the Inspector and exports.
        assert!(s.detail.contains("fallback:") && s.full().contains("no daily history for stage WhatsNews"));
        assert_eq!(s.detail, receipt.label());
        let archive = resolve(
            "AAPL",
            1_740_000_000,
            |_| async { Ok(Acquired::upstream(bars(Provenance::Real), "x")) },
            no_persist,
        )
        .await
        .unwrap();
        assert_eq!(archive.summary().provider_id, "archive");
    }

    #[tokio::test]
    async fn whats_news_flag_is_structural_not_text() {
        // Whats-News unreachable and every other source failed: flagged, and the text says so.
        let down = resolve(
            "AAPL",
            1_740_000_000,
            |stage| async move {
                if stage == Stage::WhatsNews {
                    return Err(anyhow::Error::new(WhatsNewsUnreachable("connection refused".into())));
                }
                anyhow::bail!("failure {stage:?}")
            },
            no_persist,
        )
        .await
        .err()
        .unwrap();
        assert!(needs_whats_news(&down) && error_needs_whats_news(&down.to_string()), "{down:#}");
        // Unknown ticker: the same Whats-News words appear in the generic recovery text, but the
        // Whats-News stage merely had no history, so nothing is flagged.
        let unknown =
            resolve("ZZZZ", 1_740_000_000, |_| async { anyhow::bail!("no daily history for ZZZZ") }, no_persist)
                .await
                .err()
                .unwrap();
        assert!(unknown.to_string().contains("Settings > Whats-News"));
        assert!(!needs_whats_news(&unknown) && !error_needs_whats_news(&unknown.to_string()));
        // Reachability failure covered by a later source is not an error at all.
        let covered = resolve(
            "AAPL",
            1_740_000_000,
            |stage| async move {
                if stage == Stage::Archive || stage == Stage::WhatsNews {
                    return Err(anyhow::Error::new(WhatsNewsUnreachable("offline".into())));
                }
                Ok(Acquired::upstream(bars(Provenance::Real), "yahoo (native provider)"))
            },
            no_persist,
        )
        .await;
        assert!(covered.is_ok());
    }

    #[tokio::test]
    async fn pending_bar_is_withheld() {
        let receipt = resolve(
            "AAPL",
            1_735_862_400 + 3 * 86_400,
            |_| async { Ok(Acquired::upstream(bars(Provenance::Real), "native")) },
            no_persist,
        )
        .await
        .unwrap();
        assert_eq!(receipt.bars.len(), 3);
    }
    #[tokio::test]
    async fn persistence_failure_is_a_warning_not_a_lost_result() {
        let receipt = resolve(
            "AAPL",
            1_740_000_000,
            |_| async { Ok(Acquired::upstream(bars(Provenance::Real), "native")) },
            |stage, bars: Vec<Bar>, _: String| {
                let n = bars.len();
                async move { Some(format!("VAULT WRITE FAILED: disk full ({stage:?}, {n} bars)")) }
            },
        )
        .await
        .unwrap();
        assert_eq!(receipt.stage, Stage::Archive);
        assert_eq!(receipt.bars.len(), 4);
        assert!(receipt.label().contains("VAULT WRITE FAILED: disk full (Archive, 4 bars)"), "{}", receipt.label());
        assert!(!receipt.label().contains("saved to vault"));
    }
    #[tokio::test]
    async fn vault_stage_is_not_persisted_back_and_is_not_trimmed() {
        let persisted = std::cell::RefCell::new(vec![]);
        let receipt = resolve(
            "EURUSD Curncy",
            1_735_862_400,
            |stage| async move {
                if stage != Stage::Vault {
                    anyhow::bail!("down");
                }
                let mut a = Acquired::upstream(bars(Provenance::Cached), "stored");
                a.stored = Some(meta());
                Ok(a)
            },
            |stage, _: Vec<Bar>, _: String| {
                persisted.borrow_mut().push(stage);
                async { None }
            },
        )
        .await
        .unwrap();
        assert_eq!(receipt.bars.len(), 4, "stored closure is trusted; retrieval clock does not trim");
        assert!(persisted.borrow().is_empty());
    }
}
