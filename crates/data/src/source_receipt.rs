//! Structured source receipt for research series: who supplied the bars, through
//! which closed date, how many bars. Attempt diagnostics (failed earlier sources)
//! are kept apart so the pane header can show one line and the Inspector the chain.
//!
//! Identity is `provider_id`; presentation text is derived from the fields and
//! never parsed back.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceReceipt {
    /// Lower-case stable provider/dataset id: `yahoo`, `archive`, `whats-news`, …
    pub provider_id: String,
    /// Short display name: `Yahoo`, `Archive`, `Whats-News`.
    pub provider: String,
    /// Last closed bar (unix seconds).
    pub closed_thru: Option<i64>,
    /// Bars in this series (the series' own count, not an aligned sample size).
    pub bars: usize,
    pub observed_at: Option<i64>,
    /// Earlier sources that failed before this one succeeded. Diagnostics only.
    pub attempts: Vec<String>,
    /// Full legacy receipt text (exports, MathCards, logs).
    pub detail: String,
}

/// `24841` → `24,841`.
pub fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Display name for a provider id: first segment before `:`/space, capitalised.
pub fn provider_display(id: &str) -> String {
    let head = id.split([':', ' ', '(']).next().unwrap_or(id);
    let mut c = head.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

impl SourceReceipt {
    pub fn new(provider_id: &str, bars: usize, closed_thru: Option<i64>) -> Self {
        let id = provider_id.trim().to_ascii_lowercase();
        let provider = match id.as_str() {
            "whats-news" => "Whats-News".to_string(),
            _ => provider_display(&id),
        };
        Self { provider_id: id, provider, bars, closed_thru, ..Default::default() }
    }
    pub fn thru(&self) -> Option<String> {
        let ts = self.closed_thru?;
        let d = chrono::DateTime::from_timestamp(ts, 0)?;
        let obs = self.observed_at.and_then(|t| chrono::DateTime::from_timestamp(t, 0));
        let year = obs.is_none_or(|o| o.format("%Y").to_string() != d.format("%Y").to_string());
        Some(d.format(if year { "%b %-d, %Y" } else { "%b %-d" }).to_string())
    }
    /// One line: `Yahoo · thru Nov 20 · 24,841 bars`.
    pub fn chip(&self) -> String {
        self.chip_with(0)
    }
    /// `Yahoo · thru Nov 20 · 24,841 bars · +1 series`; the count is this series' own.
    pub fn chip_with(&self, extra_series: usize) -> String {
        let mut parts = vec![if self.provider.is_empty() { "Source".to_string() } else { self.provider.clone() }];
        if let Some(t) = self.thru() {
            parts.push(format!("thru {t}"));
        }
        if self.bars > 0 {
            parts.push(format!("{} bars", thousands(self.bars)));
        }
        if extra_series > 0 {
            parts.push(format!("+{extra_series} series"));
        }
        parts.join(" · ")
    }
    /// Full chain for hover/Inspector: the detail text plus each failed attempt on its own line.
    pub fn full(&self) -> String {
        let mut out = if self.detail.is_empty() { self.chip() } else { self.detail.clone() };
        for a in &self.attempts {
            if !out.contains(a.as_str()) {
                out.push_str("\nattempt failed: ");
                out.push_str(a);
            }
        }
        out
    }
    pub fn same_provider(&self, other: &Self) -> bool {
        self.provider_id == other.provider_id
    }
}

/// Provider ids of the receipts compared pairwise: `Some(true)` when they differ,
/// `None` when either side lacks structure (never guessed from text).
pub fn providers_differ(a: Option<&SourceReceipt>, b: Option<&SourceReceipt>) -> Option<bool> {
    let (a, b) = (a?, b?);
    (!a.provider_id.is_empty() && !b.provider_id.is_empty()).then(|| !a.same_provider(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ts(y: i32, m: u32, d: u32) -> i64 {
        chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp()
    }
    #[test]
    fn chip_is_one_compact_line() {
        let mut r = SourceReceipt::new("Yahoo", 24_841, Some(ts(2026, 11, 20)));
        r.observed_at = Some(ts(2026, 11, 21));
        r.attempts = vec!["Local research archive: no history".into(), "Whats-News: offline".into()];
        assert_eq!(r.chip(), "Yahoo · thru Nov 20 · 24,841 bars");
        assert_eq!(r.chip_with(1), "Yahoo · thru Nov 20 · 24,841 bars · +1 series");
        assert!(!r.chip().contains("fallback") && !r.chip().contains("archive"));
        assert!(r.full().contains("attempt failed: Whats-News: offline"));
        r.observed_at = Some(ts(2027, 1, 3));
        assert_eq!(r.chip(), "Yahoo · thru Nov 20, 2026 · 24,841 bars");
    }
    #[test]
    fn provider_identity_is_structural() {
        let a = SourceReceipt::new("yahoo", 24_841, Some(ts(2026, 11, 20)));
        let b = SourceReceipt::new("Yahoo", 9_000, Some(ts(2026, 11, 19)));
        let c = SourceReceipt::new("whats-news", 24_841, Some(ts(2026, 11, 20)));
        assert_eq!(providers_differ(Some(&a), Some(&b)), Some(false));
        assert_eq!(providers_differ(Some(&a), Some(&c)), Some(true));
        assert_eq!(providers_differ(Some(&a), None), None);
        assert_eq!(c.provider, "Whats-News");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
