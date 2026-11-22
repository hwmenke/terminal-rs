//! Worker-only, whole-series daily source selection. Never reads display caches.
use tr_core::{Bar, Timeframe};

pub(crate) struct Receipt {
    pub bars: Vec<Bar>,
    pub source: String,
    /// Stable id of the accepted source: `archive`, `whats-news` or the native provider id.
    pub provider_id: String,
    pub fallback: Vec<String>,
    pub observed_at: i64,
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
        format!(
            "{} · CLOSED THRU {} · {} bars · observed {}{}",
            self.source,
            end,
            self.bars.len(),
            tr_data::stats::bar_date(self.observed_at),
            if self.fallback.is_empty() {
                String::new()
            } else {
                format!(" · fallback: {}", self.fallback.join("; "))
            }
        )
    }
}

/// Sentence appended to a daily-research failure only when the Whats-News stage failed because
/// the service was unreachable/unconfigured (not for "no history for this ticker"). Panes and the
/// footer test for it with [`error_needs_whats_news`]; nothing else in an error text counts.
pub(crate) const WHATS_NEWS_OFFLINE_NOTE: &str = "Whats-News was unreachable for this request.";

/// Failure of the Whats-News stage that is a service outage (as opposed to the ticker simply
/// having no history there). Raised by the worker's stage-1 fetch and read by [`resolve`].
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

pub(crate) async fn resolve<F, Fut>(key: &str, observed_at: i64, mut fetch: F) -> anyhow::Result<Receipt>
where
    F: FnMut(usize) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<(Vec<Bar>, String)>>,
{
    let mut fallback = Vec::new();
    let mut needs_whats_news = false;
    for (i, name) in ["Local research archive", "Whats-News (HTTP / SQLite)", "Native provider"].iter().enumerate() {
        let result = async {
            let (mut bars, source) = fetch(i).await?;
            tr_data::stats::require_research_bars(&bars, "Daily research").map_err(anyhow::Error::msg)?;
            let n = tr_data::closed::closed_len(Timeframe::D1, &bars, tr_data::security(key).class, Some(observed_at));
            bars.truncate(n);
            anyhow::ensure!(!bars.is_empty(), "no verified closed daily bars");
            Ok::<_, anyhow::Error>((bars, source))
        }
        .await;
        match result {
            Ok((bars, source)) => {
                let provider_id = match i {
                    0 => "archive".to_string(),
                    1 => "whats-news".to_string(),
                    _ => source.trim_end_matches(" (native provider)").to_ascii_lowercase(),
                };
                return Ok(Receipt { bars, source, provider_id, fallback, observed_at });
            }
            Err(e) => {
                needs_whats_news |= i == 1 && e.downcast_ref::<WhatsNewsUnreachable>().is_some();
                fallback.push(format!("{name}: {e:#}"))
            }
        }
    }
    Err(anyhow::Error::new(DailyUnavailable {
        message: format!(
            "Daily research unavailable for {key}. {}. Retry, or open Settings > Whats-News; HIST can download verified daily history.",
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
    #[tokio::test]
    async fn ordered_whole_series_fallback_and_receipts() {
        for accepted in 0..3 {
            let mut calls = Vec::new();
            let receipt = resolve("SPX Index", 1_740_000_000, |stage| {
                calls.push(stage);
                async move {
                    if stage < accepted {
                        anyhow::bail!("source offline");
                    }
                    Ok((bars(Provenance::Real), format!("source {stage}")))
                }
            })
            .await
            .unwrap();
            assert_eq!(calls, (0..=accepted).collect::<Vec<_>>());
            assert_eq!(receipt.bars.len(), 4);
            assert_eq!(receipt.fallback.len(), accepted);
            assert!(receipt.label().contains("CLOSED THRU 2025-01-06"));
        }
    }
    #[tokio::test]
    async fn rejected_inputs_never_spliced_or_trimmed_into_acceptance() {
        let receipt = resolve("AAPL", 1_740_000_000, |stage| async move {
            let mut data = bars(Provenance::Real);
            if stage == 0 {
                data.last_mut().unwrap().provenance = Provenance::Sample;
            }
            if stage == 1 {
                data[1].close = f64::NAN;
            }
            Ok((data, format!("source {stage}")))
        })
        .await
        .unwrap();
        assert_eq!(receipt.source, "source 2");
        assert_eq!(receipt.fallback.len(), 2);
        assert_eq!(receipt.bars.len(), 4);
        assert!(receipt.bars.iter().all(|b| b.provenance == Provenance::Real));
    }
    #[tokio::test]
    async fn exhaustion_names_every_source_and_recovery() {
        let error = resolve("SPX Index", 1_740_000_000, |stage| async move { anyhow::bail!("failure {stage}") })
            .await
            .err()
            .unwrap()
            .to_string();
        for text in [
            "Local research archive",
            "Whats-News",
            "Native provider",
            "failure 0",
            "failure 1",
            "failure 2",
            "Retry",
            "Settings > Whats-News",
        ] {
            assert!(error.contains(text), "{error}");
        }
    }
    #[tokio::test]
    async fn structured_receipt_keeps_success_apart_from_attempts() {
        let receipt = resolve("SPX Index", 1_740_000_000, |stage| async move {
            if stage < 2 {
                anyhow::bail!("no daily history for stage {stage}");
            }
            Ok((bars(Provenance::Real), "yahoo (native provider)".into()))
        })
        .await
        .unwrap();
        let s = receipt.summary();
        assert_eq!((s.provider_id.as_str(), s.provider.as_str(), s.bars), ("yahoo", "Yahoo", 4));
        assert_eq!(s.attempts.len(), 2);
        let chip = s.chip();
        assert!(chip.starts_with("Yahoo · thru Jan 6") && chip.contains("4 bars"), "{chip}");
        assert!(!chip.contains("fallback") && !chip.contains("no daily history"), "{chip}");
        // The complete diagnostic text is preserved for hover, the Inspector and exports.
        assert!(s.detail.contains("fallback:") && s.full().contains("no daily history for stage 1"));
        assert_eq!(s.detail, receipt.label());
        let archive = resolve("AAPL", 1_740_000_000, |_| async { Ok((bars(Provenance::Real), "x".into())) })
            .await
            .unwrap();
        assert_eq!(archive.summary().provider_id, "archive");
    }

    #[tokio::test]
    async fn whats_news_flag_is_structural_not_text() {
        // Whats-News unreachable and every other source failed: flagged, and the text says so.
        let down = resolve("AAPL", 1_740_000_000, |stage| async move {
            if stage == 1 {
                return Err(anyhow::Error::new(WhatsNewsUnreachable("connection refused".into())));
            }
            anyhow::bail!("failure {stage}")
        })
        .await
        .err()
        .unwrap();
        assert!(needs_whats_news(&down) && error_needs_whats_news(&down.to_string()), "{down:#}");
        // Unknown ticker: the same Whats-News words appear in the generic recovery text, but the
        // Whats-News stage merely had no history, so nothing is flagged.
        let unknown = resolve("ZZZZ", 1_740_000_000, |_| async { anyhow::bail!("no daily history for ZZZZ") })
            .await
            .err()
            .unwrap();
        assert!(unknown.to_string().contains("Settings > Whats-News"));
        assert!(!needs_whats_news(&unknown) && !error_needs_whats_news(&unknown.to_string()));
        // Reachability failure covered by a later source is not an error at all.
        let covered = resolve("AAPL", 1_740_000_000, |stage| async move {
            if stage < 2 {
                return Err(anyhow::Error::new(WhatsNewsUnreachable("offline".into())));
            }
            Ok((bars(Provenance::Real), "yahoo (native provider)".into()))
        })
        .await;
        assert!(covered.is_ok());
    }

    #[tokio::test]
    async fn pending_bar_is_withheld() {
        let receipt =
            resolve("AAPL", 1_735_862_400 + 3 * 86_400, |_| async { Ok((bars(Provenance::Real), "native".into())) })
                .await
                .unwrap();
        assert_eq!(receipt.bars.len(), 3);
    }
}
