//! Portable JSON documents for scan results, price history, journal entries, and
//! workspace backups. Every file carries source, date and provenance metadata.
//! Sample content is labelled; credentials are stripped from portable JSON.

use std::path::Path;

use chrono::{TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tr_core::{Bar, Provenance};

use crate::history::{Archive, Entry, Status};
use crate::journal::{self, Store as JournalStore};
use crate::whats_news::{Scan, ScanFilter, Snapshot};

/// Envelope format written by this module.
pub const FORMAT: u32 = 1;
/// Filename / banner text when any payload row is sample or unverified.
pub const SAMPLE_EXPORT_LABEL: &str = "WARNING: SAMPLE DATA — labelled sample export; isolate from live research";

const CREDENTIAL_KEYS: &[&str] = &[
    "client_id",
    "consumer_key",
    "user_id",
    "user_secret",
    "api_key",
    "api_secret",
    "apca_api_key_id",
    "apca_api_secret_key",
    "snaptrade",
    "password",
    "passwd",
    "secret",
    "token",
    "access_token",
    "refresh_token",
    "auth_token",
    "authorization",
];

/// What a portable file contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Scan,
    History,
    Journal,
    Workspace,
    Stats,
    Backtest,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scan => "scan",
            Self::History => "history",
            Self::Journal => "journal",
            Self::Workspace => "workspace",
            Self::Stats => "stats",
            Self::Backtest => "backtest",
        }
    }
}

/// Envelope around every export and backup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Document {
    pub format: u32,
    pub kind: Kind,
    pub exported_at: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of_date: Option<String>,
    pub provenance: Provenance,
    pub sample: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_label: Option<String>,
    /// Portable backups never carry SnapTrade / API keys. Always true on write.
    pub credentials_excluded: bool,
    pub payload: Value,
}

impl Document {
    pub fn write(&self, path: &Path) -> Result<(), String> {
        write_json(path, self)
    }

    pub fn read(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        let mut doc: Self = serde_json::from_slice(&bytes).map_err(|e| format!("not a terminal-rs export ({e})"))?;
        if doc.format > FORMAT {
            return Err(format!("export format {} is newer than this build ({FORMAT})", doc.format));
        }
        strip_credentials(&mut doc.payload);
        doc.credentials_excluded = true;
        Ok(doc)
    }

    pub fn filename(&self) -> String {
        default_filename(self.kind, self.sample, &self.exported_at)
    }
}

/// Suggested `terminal-rs-<kind>[-SAMPLE]-<YYYYMMDD>.json` name.
pub fn default_filename(kind: Kind, sample: bool, exported_at: &str) -> String {
    let day = exported_at.get(..10).unwrap_or("export").replace('-', "");
    let sample_tag = if sample { "-SAMPLE" } else { "" };
    format!("terminal-rs-{}{sample_tag}-{day}.json", kind.as_str())
}

pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| format!("serializing export: {e}"))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("writing {}: {e}", path.display()))
}

/// True when a relative path is a credential store (never copied into a backup).
pub fn is_credential_path(path: &Path) -> bool {
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        let lower = s.to_ascii_lowercase();
        lower == "secrets" || lower.contains("snaptrade") || lower.contains("credential")
    })
}

/// Drop objects/keys that look like API keys or broker secrets.
pub fn strip_credentials(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|k, _| !is_credential_key(k));
            for v in map.values_mut() {
                strip_credentials(v);
            }
        }
        Value::Array(items) => {
            for v in items {
                strip_credentials(v);
            }
        }
        _ => {}
    }
}

pub fn is_credential_key(key: &str) -> bool {
    let k = key.trim().to_ascii_lowercase().replace('-', "_");
    CREDENTIAL_KEYS.iter().any(|c| k == *c || k.ends_with(&format!("_{c}")) || k.contains("user_secret"))
}

fn sample_fields(sample: bool) -> (bool, Option<String>) {
    if sample { (true, Some(SAMPLE_EXPORT_LABEL.into())) } else { (false, None) }
}

fn envelope(
    kind: Kind,
    source: impl Into<String>,
    as_of_date: Option<String>,
    provenance: Provenance,
    sample: bool,
    payload: Value,
) -> Document {
    let (sample, sample_label) = sample_fields(sample || provenance == Provenance::Sample);
    Document {
        format: FORMAT,
        kind,
        exported_at: now_rfc3339(),
        source: source.into(),
        as_of_date,
        provenance,
        sample,
        sample_label,
        credentials_excluded: true,
        payload,
    }
}

/// Scan / universe snapshot with the source string and last-bar dates from the pane.
pub fn scan_document(
    setup: &str,
    snapshot: &Snapshot<Scan>,
    filter: Option<&ScanFilter>,
    exported_at: impl Into<String>,
) -> Result<Document, String> {
    scan_document_at(setup, snapshot, filter, exported_at.into())
}

fn scan_document_at(
    setup: &str,
    snapshot: &Snapshot<Scan>,
    filter: Option<&ScanFilter>,
    exported_at: String,
) -> Result<Document, String> {
    let sample = snapshot.provenance == Provenance::Sample
        || snapshot.data.provenance == Provenance::Sample
        || snapshot.data.results.iter().any(|r| r.provenance == Provenance::Sample);
    if !sample {
        snapshot.provenance.require_verified("export SCAN")?;
        snapshot.data.provenance.require_verified("export SCAN")?;
        for row in &snapshot.data.results {
            row.provenance.require_verified("export SCAN result")?;
        }
    }
    let as_of = snapshot.data.results.iter().filter_map(|r| r.last_date.clone()).max();
    let payload = serde_json::json!({
        "setup": setup,
        "source": snapshot.source,
        "scanned": snapshot.data.scanned,
        "results": snapshot.data.results,
        "skipped": snapshot.data.skipped,
        "filter": filter,
    });
    let (sample, sample_label) = sample_fields(sample);
    Ok(Document {
        format: FORMAT,
        kind: Kind::Scan,
        exported_at,
        source: snapshot.source.clone(),
        as_of_date: as_of,
        provenance: snapshot.provenance.combine(snapshot.data.provenance),
        sample,
        sample_label,
        credentials_excluded: true,
        payload,
    })
}

/// Universe coverage table (UNIV pane) with the same envelope as a scan.
pub fn universe_document(
    snapshot: &Snapshot<Vec<crate::whats_news::Symbol>>,
    exported_at: impl Into<String>,
) -> Result<Document, String> {
    let sample = snapshot.provenance == Provenance::Sample;
    if !sample {
        snapshot.provenance.require_verified("export UNIV")?;
    }
    let as_of = snapshot.data.iter().filter_map(|s| s.last_date.clone()).max();
    let payload = serde_json::json!({
        "setup": "UNIV",
        "source": snapshot.source,
        "symbols": snapshot.data,
    });
    let (sample, sample_label) = sample_fields(sample);
    Ok(Document {
        format: FORMAT,
        kind: Kind::Scan,
        exported_at: exported_at.into(),
        source: snapshot.source.clone(),
        as_of_date: as_of,
        provenance: snapshot.provenance,
        sample,
        sample_label,
        credentials_excluded: true,
        payload,
    })
}

/// Journal setups and date notes. Sample-tagged rows label the whole file.
pub fn journal_document(store: &JournalStore, exported_at: impl Into<String>) -> Document {
    let sample = store.setups.iter().any(|s| s.chart_provenance == Provenance::Sample);
    let as_of =
        store.setups.iter().map(|s| s.chart_date.clone()).chain(store.notes.iter().map(|n| n.chart_date.clone())).max();
    envelope(
        Kind::Journal,
        "trade journal",
        as_of,
        if sample { Provenance::Sample } else { Provenance::Real },
        sample,
        serde_json::to_value(store).unwrap_or(Value::Null),
    )
    .with_exported_at(exported_at.into())
}

impl Document {
    fn with_exported_at(mut self, exported_at: String) -> Self {
        self.exported_at = exported_at;
        self
    }
}

/// Merge a journal export into an existing store (skip duplicate ids).
pub fn merge_journal(into: &mut JournalStore, doc: &Document) -> Result<usize, String> {
    if doc.kind != Kind::Journal {
        return Err(format!("expected a journal export, got {}", doc.kind.as_str()));
    }
    let incoming: JournalStore =
        serde_json::from_value(doc.payload.clone()).map_err(|e| format!("journal payload: {e}"))?;
    let mut added = 0;
    for setup in incoming.setups {
        if !into.setups.iter().any(|s| s.id == setup.id) && into.setups.len() < journal::MAX_SETUPS {
            into.setups.push(setup);
            added += 1;
        }
    }
    for note in incoming.notes {
        if !into.notes.iter().any(|n| n.id == note.id) && into.notes.len() < journal::MAX_NOTES {
            into.notes.push(note);
            added += 1;
        }
    }
    if into.checklist_template.is_empty() && !incoming.checklist_template.is_empty() {
        into.checklist_template = incoming.checklist_template;
    }
    Ok(added)
}

/// One symbol's daily series inside a history export.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistorySeries {
    pub symbol: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub latest_date: Option<String>,
    #[serde(default)]
    pub status: String,
    pub source: String,
    pub provenance: Provenance,
    pub bars: Vec<HistoryBar>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryBar {
    pub ts: i64,
    pub date: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub provenance: Provenance,
    pub source: String,
}

impl HistoryBar {
    pub fn from_bar(bar: &Bar, source: &str) -> Self {
        Self {
            ts: bar.ts,
            date: Utc.timestamp_opt(bar.ts, 0).single().map(|t| t.date_naive().to_string()).unwrap_or_default(),
            open: bar.open,
            high: bar.high,
            low: bar.low,
            close: bar.close,
            volume: bar.volume,
            provenance: bar.provenance,
            source: source.to_string(),
        }
    }

    pub fn to_bar(&self) -> Bar {
        Bar::new(self.ts, self.open, self.high, self.low, self.close, self.volume).with_provenance(self.provenance)
    }
}

/// Build a history export from the local Yahoo research archive.
pub fn history_document(
    archive: &Archive,
    symbols: Option<&[String]>,
    exported_at: impl Into<String>,
) -> Result<Document, String> {
    let status =
        archive.status(crate::history::calendar::last_expected_session(Utc::now())).map_err(|e| format!("{e:#}"))?;
    let wanted: Option<Vec<String>> = symbols.filter(|s| !s.is_empty()).map(|s| {
        s.iter()
            .map(|t| crate::history::universe::normalize(t).unwrap_or_else(|_| t.trim().to_ascii_uppercase()))
            .collect()
    });
    let histories = match &wanted {
        Some(list) => archive.histories_for(list).map_err(|e| format!("{e:#}"))?,
        None => archive.histories().map_err(|e| format!("{e:#}"))?,
    };
    let mut series = Vec::new();
    let mut provenance = Provenance::Real;
    let mut sample = false;
    for (symbol, bars) in histories {
        if bars.iter().any(|b| b.provenance == Provenance::Sample) {
            sample = true;
        }
        if bars.iter().any(|b| !b.provenance.is_verified() && b.provenance != Provenance::Sample) {
            return Err(format!("{symbol}: history export refused unverified bars"));
        }
        if !sample {
            Bar::require_verified(&bars, "export price history")?;
        }
        provenance = provenance.combine(Bar::provenance(&bars));
        let entry = status.entries.iter().find(|e| e.symbol == symbol);
        series.push(history_series(&symbol, entry, &bars));
    }
    if wanted.is_none() {
        for entry in &status.entries {
            if !series.iter().any(|s| s.symbol == entry.symbol) {
                series.push(history_series(&entry.symbol, Some(entry), &[]));
            }
        }
    }
    series.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    let as_of = series.iter().filter_map(|s| s.latest_date.clone()).max();
    let payload = serde_json::json!({
        "coverage": coverage_payload(&status),
        "series": series,
    });
    let (sample, sample_label) = sample_fields(sample || provenance == Provenance::Sample);
    Ok(Document {
        format: FORMAT,
        kind: Kind::History,
        exported_at: exported_at.into(),
        source: "Local Yahoo daily research archive".into(),
        as_of_date: as_of,
        provenance,
        sample,
        sample_label,
        credentials_excluded: true,
        payload,
    })
}

fn history_series(symbol: &str, entry: Option<&Entry>, bars: &[Bar]) -> HistorySeries {
    let provenance = if bars.is_empty() { Provenance::Missing } else { Bar::provenance(bars) };
    HistorySeries {
        symbol: symbol.to_string(),
        name: entry.map(|e| e.name.clone()).unwrap_or_default(),
        latest_date: entry.and_then(|e| e.latest.map(|d| d.to_string())).or_else(|| {
            bars.last().and_then(|b| Utc.timestamp_opt(b.ts, 0).single().map(|t| t.date_naive().to_string()))
        }),
        status: entry.map(Entry::status).unwrap_or("MISSING").to_string(),
        source: "yahoo".into(),
        provenance,
        bars: bars.iter().map(|b| HistoryBar::from_bar(b, "yahoo")).collect(),
    }
}

fn coverage_payload(status: &Status) -> Value {
    serde_json::json!({
        "fresh": status.fresh,
        "stale": status.stale,
        "failed": status.failed,
        "missing": status.missing,
        "symbols": status.entries.len(),
    })
}

/// Restore verified Real bars only. Sample / unverified series are refused.
pub fn import_history(archive: &mut Archive, doc: &Document) -> Result<usize, String> {
    if doc.kind != Kind::History {
        return Err(format!("expected a price-history export, got {}", doc.kind.as_str()));
    }
    if doc.sample || doc.provenance == Provenance::Sample {
        return Err("history restore refused: SAMPLE export cannot enter the research archive".into());
    }
    let series: Vec<HistorySeries> = doc
        .payload
        .get("series")
        .cloned()
        .ok_or_else(|| "history export is missing series".to_string())
        .and_then(|v| serde_json::from_value(v).map_err(|e| format!("history series: {e}")))?;
    let mut imported = 0;
    for row in series {
        if row.provenance == Provenance::Sample || row.bars.iter().any(|b| b.provenance == Provenance::Sample) {
            return Err(format!("{}: SAMPLE bars cannot be restored into the research archive", row.symbol));
        }
        let bars: Vec<Bar> = row.bars.iter().map(HistoryBar::to_bar).collect();
        if bars.is_empty() {
            continue;
        }
        archive.import_verified(&row.symbol, &row.name, &bars).map_err(|e| format!("{}: {e:#}", row.symbol))?;
        imported += 1;
    }
    Ok(imported)
}

/// Strategy backtest snapshot. Sample-tagged reports stay labelled and are not restored.
pub fn backtest_document(
    report: &crate::backtest::BacktestReport,
    exported_at: impl Into<String>,
) -> Result<Document, String> {
    let sample = report.provenance == Provenance::Sample || !report.provenance.is_verified();
    if !sample {
        report.provenance.require_verified("export BACKTEST")?;
    }
    let payload = serde_json::to_value(report).map_err(|e| format!("backtest payload: {e}"))?;
    let (sample, sample_label) = sample_fields(sample || report.provenance == Provenance::Sample);
    Ok(Document {
        format: FORMAT,
        kind: Kind::Backtest,
        exported_at: exported_at.into(),
        source: report.source.clone(),
        as_of_date: Some(report.as_of.clone()).filter(|s| !s.is_empty()),
        provenance: report.provenance,
        sample,
        sample_label,
        credentials_excluded: true,
        payload,
    })
}

/// Statistics / conditional-return snapshot. Sample-tagged reports stay labelled.
pub fn stats_document(
    snapshot: &crate::stats::ResearchSnapshot,
    exported_at: impl Into<String>,
) -> Result<Document, String> {
    let sample = snapshot.stats.provenance == Provenance::Sample
        || snapshot.conditional.provenance == Provenance::Sample
        || !snapshot.stats.provenance.is_verified()
        || !snapshot.conditional.provenance.is_verified();
    if !sample {
        snapshot.stats.provenance.require_verified("export STATS")?;
        snapshot.conditional.provenance.require_verified("export STATS")?;
    }
    let provenance = snapshot.stats.provenance.combine(snapshot.conditional.provenance);
    let payload = serde_json::to_value(snapshot).map_err(|e| format!("stats payload: {e}"))?;
    let (sample, sample_label) = sample_fields(sample || provenance == Provenance::Sample);
    Ok(Document {
        format: FORMAT,
        kind: Kind::Stats,
        exported_at: exported_at.into(),
        source: snapshot.stats.source.clone(),
        as_of_date: Some(snapshot.stats.as_of.clone()).filter(|s| !s.is_empty()),
        provenance,
        sample,
        sample_label,
        credentials_excluded: true,
        payload,
    })
}

/// Workspace-shaped payload helper used by the app. Credentials are stripped here too.
pub fn workspace_document(
    source: impl Into<String>,
    sample: bool,
    mut payload: Value,
    exported_at: impl Into<String>,
) -> Document {
    strip_credentials(&mut payload);
    let (sample, sample_label) = sample_fields(sample);
    Document {
        format: FORMAT,
        kind: Kind::Workspace,
        exported_at: exported_at.into(),
        source: source.into(),
        as_of_date: None,
        provenance: if sample { Provenance::Sample } else { Provenance::Real },
        sample,
        sample_label,
        credentials_excluded: true,
        payload,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{SetupDraft, default_checklist};
    use crate::whats_news::{ScanRow, Symbol};
    use tr_core::Bar;

    fn bar(date: &str, close: f64) -> Bar {
        let ts = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp();
        Bar::new(ts, close, close, close, close, 1_000.0).with_provenance(Provenance::Real)
    }

    #[test]
    fn sample_scan_export_is_labelled_and_named() {
        let snapshot = Snapshot {
            source: "SAMPLE DATA".into(),
            provenance: Provenance::Sample,
            data: Scan {
                provenance: Provenance::Sample,
                scanned: 1,
                skipped: vec![],
                results: vec![ScanRow {
                    provenance: Provenance::Sample,
                    symbol: "AAPL".into(),
                    last_date: Some("2026-10-24".into()),
                    price: Some(1.0),
                    ..Default::default()
                }],
            },
        };
        let doc = scan_document_at("NEAR_HIGH", &snapshot, None, "2026-10-25T00:00:00Z".into()).unwrap();
        assert!(doc.sample);
        assert_eq!(doc.sample_label.as_deref(), Some(SAMPLE_EXPORT_LABEL));
        assert_eq!(doc.source, "SAMPLE DATA");
        assert_eq!(doc.as_of_date.as_deref(), Some("2026-10-24"));
        assert!(doc.filename().contains("SAMPLE"));
        assert!(doc.filename().starts_with("terminal-rs-scan-SAMPLE-"));
    }

    #[test]
    fn verified_scan_export_keeps_source_and_date() {
        let snapshot = Snapshot {
            source: "Local Yahoo daily research archive".into(),
            provenance: Provenance::Cached,
            data: Scan {
                provenance: Provenance::Real,
                scanned: 1,
                skipped: vec![],
                results: vec![ScanRow {
                    provenance: Provenance::Real,
                    symbol: "MSFT".into(),
                    last_date: Some("2026-10-23".into()),
                    price: Some(400.0),
                    ..Default::default()
                }],
            },
        };
        let doc = scan_document_at("VOL_SURGE", &snapshot, None, "2026-10-25T12:00:00Z".into()).unwrap();
        assert!(!doc.sample);
        assert!(doc.sample_label.is_none());
        assert_eq!(doc.source, "Local Yahoo daily research archive");
        assert_eq!(doc.as_of_date.as_deref(), Some("2026-10-23"));
        assert_eq!(doc.payload["setup"], "VOL_SURGE");
        assert!(!doc.filename().contains("SAMPLE"));
    }

    #[test]
    fn journal_export_labels_sample_setups() {
        let mut store = JournalStore::default();
        store
            .add_setup(
                SetupDraft {
                    symbol: "NVDA".into(),
                    chart_date: "2026-10-01".into(),
                    reasoning: "sample chart review".into(),
                    planned_risk: 100.0,
                    checklist: default_checklist(),
                    chart_provenance: Provenance::Sample,
                },
                1,
            )
            .unwrap();
        let doc = journal_document(&store, "2026-10-25T00:00:00Z");
        assert!(doc.sample);
        assert!(doc.filename().contains("SAMPLE"));
        assert_eq!(doc.kind, Kind::Journal);
        let mut empty = JournalStore::default();
        assert_eq!(merge_journal(&mut empty, &doc).unwrap(), 1);
        assert_eq!(empty.setups[0].chart_provenance, Provenance::Sample);
    }

    #[test]
    fn history_export_round_trip_refuses_sample() {
        let mut archive = Archive::memory().unwrap();
        archive
            .add(&[Symbol { symbol: "AAPL".into(), name: Some("Apple".into()), group_tag: None, last_date: None }])
            .unwrap();
        archive.import_verified("AAPL", "Apple", &[bar("2026-10-22", 10.0), bar("2026-10-23", 11.0)]).unwrap();
        let doc = history_document(&archive, None, "2026-10-25T00:00:00Z").unwrap();
        assert_eq!(doc.kind, Kind::History);
        assert!(!doc.sample);
        assert_eq!(doc.source, "Local Yahoo daily research archive");
        assert_eq!(doc.as_of_date.as_deref(), Some("2026-10-23"));
        assert_eq!(doc.payload["series"][0]["source"], "yahoo");
        assert_eq!(doc.payload["series"][0]["bars"][0]["date"], "2026-10-22");

        let mut sample = doc.clone();
        sample.sample = true;
        sample.provenance = Provenance::Sample;
        assert!(import_history(&mut Archive::memory().unwrap(), &sample).unwrap_err().contains("SAMPLE"));

        let mut other = Archive::memory().unwrap();
        assert_eq!(import_history(&mut other, &doc).unwrap(), 1);
        assert_eq!(other.daily("AAPL").unwrap().len(), 2);
    }

    #[test]
    fn credentials_are_stripped_from_portable_json() {
        let mut payload = serde_json::json!({
            "prefs": { "theme": "Nord", "user_secret": "leak", "client_id": "id" },
            "nested": { "snaptrade": { "consumer_key": "k" }, "ok": 1 },
        });
        strip_credentials(&mut payload);
        assert_eq!(payload["prefs"]["theme"], "Nord");
        assert!(payload["prefs"].get("user_secret").is_none());
        assert!(payload["prefs"].get("client_id").is_none());
        assert!(payload["nested"].get("snaptrade").is_none());
        assert_eq!(payload["nested"]["ok"], 1);
        assert!(is_credential_path(Path::new("secrets/snaptrade.json")));
        assert!(!is_credential_path(Path::new("layouts/Trading.json")));
    }

    #[test]
    fn workspace_envelope_always_excludes_credentials() {
        let doc = workspace_document(
            "personal workspace",
            false,
            serde_json::json!({ "api_key": "x", "layouts": { "Desk": {} } }),
            "2026-10-25T00:00:00Z",
        );
        assert!(doc.credentials_excluded);
        assert!(doc.payload.get("api_key").is_none());
        assert_eq!(doc.kind, Kind::Workspace);
    }

    #[test]
    fn stats_export_keeps_source_as_of_and_provenance() {
        let ts = |d: &str| {
            chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d")
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp()
        };
        let bars: Vec<Bar> = (0..40)
            .map(|i| {
                let c = 100.0 + i as f64;
                Bar::new(ts("2026-01-01") + i * 86_400, c, c, c, c, 1_000.0).with_provenance(Provenance::Real)
            })
            .collect();
        let snap = crate::stats::compute_snapshot(
            "AAPL US Equity",
            "AAPL",
            &bars,
            "RSI(14) < 30",
            &[5],
            "Local Yahoo daily research archive",
        )
        .unwrap();
        let doc = stats_document(&snap, "2026-10-25T00:00:00Z").unwrap();
        assert_eq!(doc.kind, Kind::Stats);
        assert!(!doc.sample);
        assert_eq!(doc.source, "Local Yahoo daily research archive");
        assert_eq!(doc.as_of_date.as_deref(), Some(snap.stats.as_of.as_str()));
        assert_eq!(doc.provenance, Provenance::Real);
        assert_eq!(doc.payload["stats"]["ticker"], "AAPL");
        assert!(doc.filename().starts_with("terminal-rs-stats-"));
    }

    #[test]
    fn backtest_export_keeps_source_as_of_and_settings() {
        let ts = |d: &str| {
            chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d")
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp()
        };
        let bars: Vec<Bar> = (0..80)
            .map(|i| {
                let c = 50.0 + i as f64 * 0.1;
                Bar::new(ts("2024-01-02") + i * 86_400, c, c, c, c, 1_000.0).with_provenance(Provenance::Real)
            })
            .collect();
        let settings =
            crate::backtest::RunSettings::default().with_strategies(vec![crate::backtest::StrategySpec::BuyAndHold]);
        let report = crate::backtest::compute_report(
            "AAPL US Equity",
            "AAPL",
            &bars,
            &settings,
            "Local Yahoo daily research archive",
        )
        .unwrap();
        let doc = backtest_document(&report, "2026-10-25T00:00:00Z").unwrap();
        assert_eq!(doc.kind, Kind::Backtest);
        assert!(!doc.sample);
        assert_eq!(doc.source, "Local Yahoo daily research archive");
        assert_eq!(doc.as_of_date.as_deref(), Some(report.as_of.as_str()));
        assert_eq!(doc.provenance, Provenance::Real);
        assert_eq!(doc.payload["ticker"], "AAPL");
        assert!(doc.filename().starts_with("terminal-rs-backtest-"));
    }
}
