//! Workspace backup / restore on top of layout and theme file import/export.
//! SnapTrade credentials and other secrets never enter the portable JSON.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tr_data::portable::{self, Document, Kind, SAMPLE_EXPORT_LABEL, is_credential_path, strip_credentials};

use crate::layouts::{self, LayoutDoc};
use crate::prefs::Prefs;
use crate::themes::{self, Theme};
use crate::worker::data_dir;

/// Custom theme text stored inside a workspace backup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThemeFile {
    pub name: String,
    pub toml: String,
}

/// Personal files that travel with a workspace backup. No `secrets/` directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspacePayload {
    pub prefs: serde_json::Value,
    pub settings: serde_json::Value,
    pub layouts: BTreeMap<String, serde_json::Value>,
    pub themes: Vec<ThemeFile>,
    pub journal: tr_data::journal::Store,
    pub price_alerts: tr_data::price_alerts::Store,
    #[serde(default)]
    pub palette: Vec<crate::palette::Item>,
}

pub fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(rest),
        None => PathBuf::from(p),
    }
}

/// `~/Downloads` when it exists, otherwise `<data dir>/exports`.
pub fn default_export_dir() -> PathBuf {
    let downloads = expand_home("~/Downloads");
    if downloads.is_dir() { downloads } else { data_dir().join("exports") }
}

pub fn default_path(kind: Kind, sample: bool) -> PathBuf {
    default_export_dir().join(portable::default_filename(kind, sample, &portable::now_rfc3339()))
}

pub fn resolve_path(given: Option<&str>, kind: Kind, sample: bool) -> PathBuf {
    match given.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => expand_home(p),
        None => default_path(kind, sample),
    }
}

pub fn write_document(doc: &Document, path: &Path) -> Result<PathBuf> {
    if is_credential_path(path) {
        bail!("refusing to write an export into a credentials path ({})", path.display());
    }
    doc.write(path).map_err(|e| anyhow::anyhow!(e))?;
    Ok(path.to_path_buf())
}

pub fn build_workspace(
    prefs: &Prefs,
    settings: &impl Serialize,
    layouts: &layouts::Store,
    themes: &[Theme],
    journal: &tr_data::journal::Store,
    price_alerts: &tr_data::price_alerts::Store,
    palette: &[crate::palette::Item],
) -> Result<Document> {
    let mut prefs_value = serde_json::to_value(prefs).context("serializing prefs")?;
    strip_credentials(&mut prefs_value);
    let mut settings_value = serde_json::to_value(settings).context("serializing settings")?;
    strip_credentials(&mut settings_value);

    let mut layout_map = BTreeMap::new();
    for name in layouts.list() {
        let Ok(doc) = layouts.load(&name) else { continue };
        let mut value = serde_json::from_slice(&doc.to_json()).context("layout json")?;
        strip_credentials(&mut value);
        layout_map.insert(doc.name, value);
    }

    let theme_files: Vec<ThemeFile> =
        themes.iter().filter(|t| !t.builtin).map(|t| ThemeFile { name: t.name.clone(), toml: t.to_toml() }).collect();

    let sample = journal.setups.iter().any(|s| s.chart_provenance == tr_core::Provenance::Sample);
    let payload = WorkspacePayload {
        prefs: prefs_value,
        settings: settings_value,
        layouts: layout_map,
        themes: theme_files,
        journal: journal.clone(),
        price_alerts: price_alerts.clone(),
        palette: palette.to_vec(),
    };
    let mut value = serde_json::to_value(&payload).context("serializing workspace")?;
    strip_credentials(&mut value);
    Ok(portable::workspace_document("terminal-rs personal workspace", sample, value, portable::now_rfc3339()))
}

pub fn parse_workspace(doc: &Document) -> Result<WorkspacePayload> {
    if doc.kind != Kind::Workspace {
        bail!("expected a workspace backup, got {}", doc.kind.as_str());
    }
    let mut payload = doc.payload.clone();
    strip_credentials(&mut payload);
    serde_json::from_value(payload).context("workspace payload")
}

pub fn apply_layouts(store: &layouts::Store, layouts: &BTreeMap<String, serde_json::Value>) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for (name, value) in layouts {
        let bytes = serde_json::to_vec(value).with_context(|| format!("layout {name:?}"))?;
        let mut doc = LayoutDoc::from_json(&bytes).with_context(|| format!("layout {name:?}"))?;
        if doc.name.trim().is_empty() {
            doc.name = name.clone();
        }
        store.save(&doc)?;
        names.push(doc.name);
    }
    Ok(names)
}

pub fn apply_themes(theme_store: &themes::ThemeStore, files: &[ThemeFile]) -> Result<usize> {
    let mut n = 0;
    for file in files {
        if file.name.eq_ignore_ascii_case(themes::SYSTEM) {
            continue;
        }
        let path = theme_store.path(&file.name);
        if is_credential_path(&path) {
            continue;
        }
        layouts::write_atomic(&path, file.toml.as_bytes()).with_context(|| format!("theme {}", file.name))?;
        n += 1;
    }
    Ok(n)
}

pub fn sample_banner(doc: &Document) -> Option<&'static str> {
    doc.sample.then_some(SAMPLE_EXPORT_LABEL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prefs::PanelDefaults;
    use tr_data::journal::{SetupDraft, default_checklist};
    use tr_data::portable::Kind;

    fn temp_layouts(tag: &str) -> layouts::Store {
        let dir = std::env::temp_dir().join(format!("tr-backup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        layouts::Store::new(dir)
    }

    #[test]
    fn workspace_backup_round_trip_omits_credentials_and_labels_sample_journal() {
        let store = temp_layouts("ws");
        store.save(&LayoutDoc::default_workspace("Desk", &PanelDefaults::default())).unwrap();
        let mut journal = tr_data::journal::Store::default();
        journal
            .add_setup(
                SetupDraft {
                    symbol: "AAPL".into(),
                    chart_date: "2026-10-24".into(),
                    reasoning: "sample chart".into(),
                    planned_risk: 100.0,
                    checklist: default_checklist(),
                    chart_provenance: tr_core::Provenance::Sample,
                },
                1,
            )
            .unwrap();
        let mut prefs = Prefs::default();
        prefs.appearance.theme = "Nord".into();
        let settings = serde_json::json!({
            "watchlist": ["AAPL US Equity"],
            "client_id": "must-not-travel",
            "user_secret": "must-not-travel"
        });
        let doc =
            build_workspace(&prefs, &settings, &store, &[], &journal, &tr_data::price_alerts::Store::default(), &[])
                .unwrap();
        assert_eq!(doc.kind, Kind::Workspace);
        assert!(doc.credentials_excluded);
        assert!(doc.sample);
        assert!(doc.filename().contains("SAMPLE"));
        assert!(doc.payload.get("client_id").is_none());
        assert!(doc.payload.get("user_secret").is_none());
        assert!(doc.payload["settings"].get("client_id").is_none());
        assert_eq!(doc.payload["settings"]["watchlist"][0], "AAPL US Equity");
        assert!(doc.payload["layouts"].get("Desk").is_some());

        let out = store.dir.join("backup.json");
        write_document(&doc, &out).unwrap();
        let loaded = Document::read(&out).unwrap();
        let payload = parse_workspace(&loaded).unwrap();
        assert_eq!(payload.journal.setups.len(), 1);
        assert_eq!(payload.journal.setups[0].chart_provenance, tr_core::Provenance::Sample);
        let _ = std::fs::remove_dir_all(&store.dir);
    }

    #[test]
    fn default_export_names_include_kind_and_sample() {
        let p = default_path(Kind::Scan, true);
        assert!(p.file_name().unwrap().to_string_lossy().contains("scan-SAMPLE"));
        assert!(!is_credential_path(&p));
    }
}
