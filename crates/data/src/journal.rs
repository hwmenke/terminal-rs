//! Durable local trade journal: setups, pre-trade checklists, outcomes, and date notes.
//!
//! User-authored only. Outcomes are never invented from quotes or bars — sample and
//! unverified history cannot compute a dollar P&L or an R-multiple. Chart dates may
//! still be recorded while looking at sample data; those rows are tagged so the UI
//! can show the SAMPLE banner.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tr_core::Provenance;

/// Cap so a long session cannot accumulate unbounded setups.
pub const MAX_SETUPS: usize = 500;
/// Cap on notes attached to historical chart dates.
pub const MAX_NOTES: usize = 2_000;
/// Checklist rows copied onto a new setup.
pub const MAX_CHECKLIST: usize = 32;
pub const MAX_REASONING: usize = 4_000;
pub const MAX_NOTE: usize = 2_000;
pub const MAX_LABEL: usize = 120;

/// Default pre-trade checklist (Whats-News-style process, not a rating).
pub const DEFAULT_CHECKLIST: &[&str] = &[
    "Daily trend aligned",
    "Weekly trend aligned",
    "Volume confirms",
    "Stop defined",
    "Size from planned $ risk",
    "Not chasing",
    "News / catalyst reviewed",
];

/// One checkbox on a saved setup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecklistItem {
    pub label: String,
    pub checked: bool,
}

impl ChecklistItem {
    pub fn new(label: impl Into<String>, checked: bool) -> Self {
        Self { label: truncate(label.into(), MAX_LABEL), checked }
    }
}

/// Realised result in dollars and multiples of the planned initial risk (1R).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub dollars: f64,
    pub r_multiple: f64,
    pub recorded_at: i64,
    #[serde(default)]
    pub note: String,
}

/// A planned trade: chart date, reasoning, planned dollar risk, and checklist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Setup {
    pub id: String,
    pub symbol: String,
    /// Calendar date of the chart the user reviewed (`YYYY-MM-DD`).
    pub chart_date: String,
    pub reasoning: String,
    /// Planned initial risk in dollars (1R).
    pub planned_risk: f64,
    pub checklist: Vec<ChecklistItem>,
    pub created_at: i64,
    #[serde(default)]
    pub outcome: Option<Outcome>,
    /// Chart series the date was taken from. Sample never computes an outcome.
    #[serde(default)]
    pub chart_provenance: Provenance,
}

impl Setup {
    pub fn open(&self) -> bool {
        self.outcome.is_none()
    }

    pub fn ticker(&self) -> String {
        crate::security(&self.symbol).ticker
    }
}

/// A free-form note attached to one symbol on one historical chart date.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DateNote {
    pub id: String,
    pub symbol: String,
    pub chart_date: String,
    pub text: String,
    pub created_at: i64,
}

impl DateNote {
    pub fn ticker(&self) -> String {
        crate::security(&self.symbol).ticker
    }
}

/// Fields accepted when saving a setup.
#[derive(Debug, Clone, PartialEq)]
pub struct SetupDraft {
    pub symbol: String,
    pub chart_date: String,
    pub reasoning: String,
    pub planned_risk: f64,
    pub checklist: Vec<ChecklistItem>,
    pub chart_provenance: Provenance,
}

impl Default for SetupDraft {
    fn default() -> Self {
        Self {
            symbol: String::new(),
            chart_date: String::new(),
            reasoning: String::new(),
            planned_risk: 250.0,
            checklist: default_checklist(),
            chart_provenance: Provenance::Missing,
        }
    }
}

/// Persisted journal. Lives in `trade_journal.json` under the terminal data directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Store {
    #[serde(default)]
    pub setups: Vec<Setup>,
    #[serde(default)]
    pub notes: Vec<DateNote>,
    /// Labels used for the next new setup. Empty means [`DEFAULT_CHECKLIST`].
    #[serde(default)]
    pub checklist_template: Vec<String>,
}

impl Store {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .map(Self::normalized)
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) {
        if path.as_os_str().is_empty() {
            return;
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(path, bytes);
        }
    }

    fn normalized(mut self) -> Self {
        self.setups.truncate(MAX_SETUPS);
        self.notes.truncate(MAX_NOTES);
        self
    }

    pub fn template(&self) -> Vec<String> {
        if self.checklist_template.is_empty() {
            DEFAULT_CHECKLIST.iter().map(|s| (*s).to_string()).collect()
        } else {
            self.checklist_template.iter().take(MAX_CHECKLIST).cloned().collect()
        }
    }

    pub fn blank_checklist(&self) -> Vec<ChecklistItem> {
        self.template().into_iter().map(|label| ChecklistItem::new(label, false)).collect()
    }

    /// Newest-first setups, optionally limited to one market key / ticker.
    pub fn setups_for(&self, filter: &str) -> Vec<&Setup> {
        let key = filter_key(filter);
        self.setups
            .iter()
            .filter(|s| key.as_ref().is_none_or(|k| s.symbol == *k || s.ticker().eq_ignore_ascii_case(filter.trim())))
            .collect()
    }

    pub fn notes_for(&self, filter: &str) -> Vec<&DateNote> {
        let key = filter_key(filter);
        self.notes
            .iter()
            .filter(|n| key.as_ref().is_none_or(|k| n.symbol == *k || n.ticker().eq_ignore_ascii_case(filter.trim())))
            .collect()
    }

    pub fn notes_on(&self, symbol: &str, chart_date: &str) -> Vec<&DateNote> {
        let key = crate::security(symbol).key();
        self.notes.iter().filter(|n| n.symbol == key && n.chart_date == chart_date).collect()
    }

    /// Dates that have a setup or a note for `symbol`, newest first.
    pub fn marked_dates(&self, symbol: &str) -> Vec<String> {
        let key = crate::security(symbol).key();
        let mut dates: Vec<String> = self
            .setups
            .iter()
            .filter(|s| s.symbol == key)
            .map(|s| s.chart_date.clone())
            .chain(self.notes.iter().filter(|n| n.symbol == key).map(|n| n.chart_date.clone()))
            .collect();
        dates.sort();
        dates.dedup();
        dates.reverse();
        dates
    }

    pub fn add_setup(&mut self, draft: SetupDraft, now: i64) -> Result<Setup, String> {
        let symbol = crate::security(&draft.symbol).key();
        if symbol.is_empty() {
            return Err("setup needs a ticker".into());
        }
        let chart_date = parse_chart_date(&draft.chart_date)?;
        let reasoning = draft.reasoning.trim();
        if reasoning.is_empty() {
            return Err("setup needs reasoning".into());
        }
        if reasoning.len() > MAX_REASONING {
            return Err(format!("reasoning is at most {MAX_REASONING} characters"));
        }
        if !draft.planned_risk.is_finite() || draft.planned_risk <= 0.0 {
            return Err("planned risk must be a positive dollar amount (1R)".into());
        }
        if self.setups.len() >= MAX_SETUPS {
            return Err(format!("at most {MAX_SETUPS} journal setups"));
        }
        let checklist: Vec<ChecklistItem> = draft
            .checklist
            .into_iter()
            .filter(|i| !i.label.trim().is_empty())
            .take(MAX_CHECKLIST)
            .map(|i| ChecklistItem::new(i.label, i.checked))
            .collect();
        if checklist.is_empty() {
            return Err("setup needs at least one checklist item".into());
        }
        let setup = Setup {
            id: format!("{symbol}:{chart_date}:{now}"),
            symbol,
            chart_date,
            reasoning: reasoning.to_string(),
            planned_risk: draft.planned_risk,
            checklist,
            created_at: now,
            outcome: None,
            chart_provenance: draft.chart_provenance,
        };
        self.setups.insert(0, setup.clone());
        Ok(setup)
    }

    /// Record a closed result. Dollars and/or R-multiples are accepted; the other is
    /// derived from planned risk. Prices are never read — sample bars cannot invent P&L.
    pub fn record_outcome(
        &mut self,
        id: &str,
        dollars: Option<f64>,
        r_multiple: Option<f64>,
        note: impl Into<String>,
        now: i64,
    ) -> Result<Outcome, String> {
        let setup = self.setups.iter().find(|s| s.id == id).ok_or_else(|| "unknown setup".to_string())?;
        let outcome = outcome_from(setup.planned_risk, dollars, r_multiple, note, now)?;
        let setup = self.setups.iter_mut().find(|s| s.id == id).expect("id just found");
        setup.outcome = Some(outcome.clone());
        Ok(outcome)
    }

    pub fn clear_outcome(&mut self, id: &str) -> bool {
        if let Some(s) = self.setups.iter_mut().find(|s| s.id == id) {
            s.outcome = None;
            true
        } else {
            false
        }
    }

    pub fn remove_setup(&mut self, id: &str) -> bool {
        let before = self.setups.len();
        self.setups.retain(|s| s.id != id);
        before != self.setups.len()
    }

    pub fn add_note(
        &mut self,
        symbol: impl Into<String>,
        chart_date: &str,
        text: impl Into<String>,
        now: i64,
    ) -> Result<DateNote, String> {
        let symbol = crate::security(&symbol.into()).key();
        if symbol.is_empty() {
            return Err("note needs a ticker".into());
        }
        let chart_date = parse_chart_date(chart_date)?;
        let text = text.into();
        let text = text.trim();
        if text.is_empty() {
            return Err("note needs text".into());
        }
        if text.len() > MAX_NOTE {
            return Err(format!("note is at most {MAX_NOTE} characters"));
        }
        if self.notes.len() >= MAX_NOTES {
            return Err(format!("at most {MAX_NOTES} journal notes"));
        }
        let note = DateNote {
            id: format!("note:{symbol}:{chart_date}:{now}"),
            symbol,
            chart_date,
            text: text.to_string(),
            created_at: now,
        };
        self.notes.insert(0, note.clone());
        Ok(note)
    }

    pub fn remove_note(&mut self, id: &str) -> bool {
        let before = self.notes.len();
        self.notes.retain(|n| n.id != id);
        before != self.notes.len()
    }

    pub fn set_template(&mut self, labels: Vec<String>) -> Result<(), String> {
        let labels: Vec<String> = labels
            .into_iter()
            .map(|s| truncate(s.trim().to_string(), MAX_LABEL))
            .filter(|s| !s.is_empty())
            .take(MAX_CHECKLIST)
            .collect();
        if labels.is_empty() {
            return Err("checklist needs at least one item".into());
        }
        self.checklist_template = labels;
        Ok(())
    }

    /// Open (no outcome) count, used as a sidebar badge.
    pub fn open_count(&self) -> usize {
        self.setups.iter().filter(|s| s.open()).count()
    }
}

/// Compute dollars and R from planned risk. Never reads a price series.
pub fn outcome_from(
    planned_risk: f64,
    dollars: Option<f64>,
    r_multiple: Option<f64>,
    note: impl Into<String>,
    now: i64,
) -> Result<Outcome, String> {
    if !planned_risk.is_finite() || planned_risk <= 0.0 {
        return Err("planned risk must be a positive dollar amount (1R)".into());
    }
    let (dollars, r_multiple) = match (dollars, r_multiple) {
        (Some(d), Some(r)) => {
            if !d.is_finite() || !r.is_finite() {
                return Err("outcome must be a finite dollar amount or R-multiple".into());
            }
            let implied = d / planned_risk;
            if (implied - r).abs() > 1e-6 && r.abs() > 1e-9 && (implied - r).abs() / r.abs().max(1e-9) > 1e-4 {
                return Err("dollar P&L and R-multiple disagree; enter one and the other is derived".into());
            }
            (d, implied)
        }
        (Some(d), None) => {
            if !d.is_finite() {
                return Err("dollar P&L must be a finite number".into());
            }
            (d, d / planned_risk)
        }
        (None, Some(r)) => {
            if !r.is_finite() {
                return Err("R-multiple must be a finite number".into());
            }
            (r * planned_risk, r)
        }
        (None, None) => return Err("record a dollar P&L or an R-multiple".into()),
    };
    Ok(Outcome { dollars, r_multiple, recorded_at: now, note: truncate(note.into().trim().to_string(), MAX_NOTE) })
}

/// Parse `YYYY-MM-DD`. Rejects impossible calendar dates.
pub fn parse_chart_date(s: &str) -> Result<String, String> {
    let s = s.trim();
    let err = || format!("chart date must be YYYY-MM-DD, not {s:?}");
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return Err(err());
    }
    let y: i32 = parts[0].parse().map_err(|_| err())?;
    let m: u32 = parts[1].parse().map_err(|_| err())?;
    let d: u32 = parts[2].parse().map_err(|_| err())?;
    chrono::NaiveDate::from_ymd_opt(y, m, d).ok_or_else(err)?;
    Ok(format!("{y:04}-{m:02}-{d:02}"))
}

/// Calendar date (`YYYY-MM-DD`) of a bar timestamp (UTC midnight of that session).
pub fn date_from_ts(ts: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(ts, 0).map(|t| t.date_naive().to_string())
}

pub fn default_checklist() -> Vec<ChecklistItem> {
    DEFAULT_CHECKLIST.iter().map(|s| ChecklistItem::new(*s, false)).collect()
}

fn filter_key(filter: &str) -> Option<String> {
    let t = filter.trim();
    if t.is_empty() {
        return None;
    }
    Some(crate::security(t).key())
}

fn truncate(s: String, max: usize) -> String {
    if s.len() <= max { s } else { s.chars().take(max).collect() }
}

/// Path used by the app (`<data dir>/trade_journal.json`).
pub fn store_path(data_dir: impl AsRef<Path>) -> PathBuf {
    data_dir.as_ref().join("trade_journal.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(symbol: &str, date: &str, risk: f64) -> SetupDraft {
        SetupDraft {
            symbol: symbol.into(),
            chart_date: date.into(),
            reasoning: "breakout above box, volume confirms".into(),
            planned_risk: risk,
            checklist: default_checklist(),
            chart_provenance: Provenance::Real,
        }
    }

    #[test]
    fn save_setup_and_record_outcome_in_dollars_and_r() {
        let mut s = Store::default();
        let setup = s.add_setup(draft("aapl", "2026-10-24", 250.0), 1).unwrap();
        assert_eq!(setup.symbol, "AAPL US Equity");
        assert_eq!(setup.chart_date, "2026-10-24");
        assert_eq!(setup.checklist.len(), DEFAULT_CHECKLIST.len());
        assert!(setup.open());

        let out = s.record_outcome(&setup.id, Some(500.0), None, "held 3 days", 2).unwrap();
        assert!((out.dollars - 500.0).abs() < 1e-9);
        assert!((out.r_multiple - 2.0).abs() < 1e-9);
        assert!(!s.setups[0].open());

        s.clear_outcome(&setup.id);
        let out = s.record_outcome(&setup.id, None, Some(-1.0), "", 3).unwrap();
        assert!((out.dollars + 250.0).abs() < 1e-9);
        assert!((out.r_multiple + 1.0).abs() < 1e-9);
    }

    #[test]
    fn disagreeing_dollar_and_r_are_rejected() {
        let mut s = Store::default();
        let setup = s.add_setup(draft("MSFT", "2026-01-02", 100.0), 1).unwrap();
        let err = s.record_outcome(&setup.id, Some(200.0), Some(-1.0), "", 2).unwrap_err();
        assert!(err.contains("disagree"));
        assert!(s.setups[0].open());
    }

    #[test]
    fn sample_chart_can_be_tagged_but_never_invents_an_outcome() {
        let mut s = Store::default();
        let mut d = draft("NVDA", "2026-10-01", 100.0);
        d.chart_provenance = Provenance::Sample;
        let setup = s.add_setup(d, 1).unwrap();
        assert_eq!(setup.chart_provenance, Provenance::Sample);
        assert!(setup.chart_provenance.require_verified("journal outcome from bars").is_err());
        // Outcomes stay user-entered; there is no price-series helper on Store.
        let out = outcome_from(100.0, Some(-50.0), None, "", 2).unwrap();
        assert!((out.r_multiple + 0.5).abs() < 1e-9);
    }

    #[test]
    fn notes_attach_to_historical_dates() {
        let mut s = Store::default();
        let n = s.add_note("AAPL", "2026-03-15", "gap and go, wait for 20-day", 10).unwrap();
        assert_eq!(n.symbol, "AAPL US Equity");
        assert_eq!(n.chart_date, "2026-03-15");
        assert_eq!(s.notes_on("aapl", "2026-03-15").len(), 1);
        assert_eq!(s.marked_dates("AAPL"), vec!["2026-03-15".to_string()]);
        assert!(s.add_note("AAPL", "2026-13-40", "bad", 11).is_err());
        assert!(s.add_note("AAPL", "2026-03-15", "   ", 12).is_err());
    }

    #[test]
    fn rejects_invalid_setups_and_respects_caps() {
        let mut s = Store::default();
        assert!(s.add_setup(draft("", "2026-10-24", 100.0), 1).is_err());
        assert!(s.add_setup(draft("AAPL", "yesterday", 100.0), 1).is_err());
        let mut no_reason = draft("AAPL", "2026-10-24", 100.0);
        no_reason.reasoning.clear();
        assert!(s.add_setup(no_reason, 1).is_err());
        assert!(s.add_setup(draft("AAPL", "2026-10-24", 0.0), 1).is_err());
        let mut empty_list = draft("AAPL", "2026-10-24", 100.0);
        empty_list.checklist.clear();
        assert!(s.add_setup(empty_list, 1).is_err());
        assert_eq!(parse_chart_date("2026-2-3").unwrap(), "2026-02-03");
    }

    #[test]
    fn json_round_trip_and_filter() {
        let dir = std::env::temp_dir().join(format!("tr-journal-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = store_path(&dir);
        let mut s = Store::default();
        s.add_setup(draft("AAPL", "2026-10-24", 200.0), 1).unwrap();
        s.add_setup(draft("MSFT", "2026-10-23", 150.0), 2).unwrap();
        s.add_note("AAPL", "2026-10-20", "watch the 50", 3).unwrap();
        s.save(&path);
        let loaded = Store::load(&path);
        assert_eq!(loaded.setups.len(), 2);
        assert_eq!(loaded.setups_for("aapl").len(), 1);
        assert_eq!(loaded.notes_for("AAPL").len(), 1);
        assert_eq!(loaded.open_count(), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn template_defaults_and_can_be_replaced() {
        let mut s = Store::default();
        assert_eq!(s.blank_checklist().len(), DEFAULT_CHECKLIST.len());
        s.set_template(vec!["A".into(), "B".into()]).unwrap();
        assert_eq!(s.template(), vec!["A".to_string(), "B".to_string()]);
        assert!(s.set_template(vec!["  ".into()]).is_err());
    }
}
