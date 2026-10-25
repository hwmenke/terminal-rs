//! Command and settings entry points for export, backup and restore.

use super::{PaneKind, Settings, TerminalApp};
use crate::command::ExportKind;
use crate::portable::{self, apply_layouts, apply_themes, resolve_path, write_document};
use crate::worker::Request;
use tr_data::portable::{self as env, Document, Kind};

impl TerminalApp {
    pub(super) fn current_settings(&self) -> Settings {
        Settings {
            watchlist: self.watchlist.clone(),
            watchlists: Some(self.watchlists.clone()),
            recent_symbols: self.recent_symbols.clone(),
            sidebar: self.sidebar_open,
            active_layout: Some(self.layout_name.clone()),
            layout: None,
            panes: None,
        }
    }

    pub(super) fn run_export(&mut self, kind: ExportKind, path: &str) {
        match kind {
            ExportKind::Auto => match self.focused_export_kind() {
                Some(k) => self.run_export(k, path),
                None => {
                    self.settings_tab = super::settings_view::SettingsTab::Backup;
                    self.open_panel(PaneKind::Settings);
                    self.status = "Settings › Backup — export scans, history, journal, or restore a workspace".into();
                }
            },
            ExportKind::Scan => self.export_scan(path),
            ExportKind::Journal => self.export_journal(path),
            ExportKind::History => self.export_history(path),
            ExportKind::Stats => self.export_stats(path),
            ExportKind::Backtest => self.export_backtest(path),
            ExportKind::Lookalike => self.export_lookalike(path),
            ExportKind::Workspace => self.backup_workspace(path),
        }
    }

    fn focused_export_kind(&self) -> Option<ExportKind> {
        match self.panes.iter().find(|p| p.id == self.focus).map(|p| &p.kind) {
            Some(PaneKind::Research(r)) if r.history => Some(ExportKind::History),
            Some(PaneKind::Research(_)) => Some(ExportKind::Scan),
            Some(PaneKind::Journal(_)) => Some(ExportKind::Journal),
            Some(PaneKind::Stats(_)) => Some(ExportKind::Stats),
            Some(PaneKind::Backtest(_)) => Some(ExportKind::Backtest),
            Some(PaneKind::Lookalike(_)) => Some(ExportKind::Lookalike),
            Some(PaneKind::Settings) => Some(ExportKind::Workspace),
            _ => None,
        }
    }

    fn export_scan(&mut self, path: &str) {
        let pane = self.panes.iter().find(|p| p.id == self.focus).and_then(|p| match &p.kind {
            PaneKind::Research(r) if !r.history => Some(r),
            _ => None,
        });
        let pane = pane.or_else(|| {
            self.panes.iter().find_map(|p| match &p.kind {
                PaneKind::Research(r) if !r.history => Some(r),
                _ => None,
            })
        });
        let Some(pane) = pane else {
            self.status = "EXPORT SCAN needs an open SCAN or UNIV pane with results".into();
            return;
        };
        let doc = if let Some(snapshot) = &pane.results {
            env::scan_document(pane.scan.as_deref().unwrap_or("SCAN"), snapshot, Some(&pane.filter), env::now_rfc3339())
        } else if let Some(snapshot) = &pane.universe {
            env::universe_document(snapshot, env::now_rfc3339())
        } else {
            self.status = "SCAN / UNIV has no results to export yet".into();
            return;
        };
        match doc {
            Ok(doc) => self.finish_write(&doc, path, "Exported scan"),
            Err(e) => self.status = e,
        }
    }

    fn export_stats(&mut self, path: &str) {
        let pane = self.panes.iter().find(|p| p.id == self.focus).and_then(|p| match &p.kind {
            PaneKind::Stats(s) => Some(s),
            _ => None,
        });
        let pane = pane.or_else(|| {
            self.panes.iter().find_map(|p| match &p.kind {
                PaneKind::Stats(s) => Some(s),
                _ => None,
            })
        });
        let Some(pane) = pane else {
            self.status = "EXPORT STATS needs an open STATS pane with results".into();
            return;
        };
        let Some(snapshot) = &pane.snapshot else {
            self.status = "STATS has no results to export yet".into();
            return;
        };
        match env::stats_document(snapshot, env::now_rfc3339()) {
            Ok(doc) => self.finish_write(&doc, path, "Exported statistics"),
            Err(e) => self.status = e,
        }
    }

    fn export_backtest(&mut self, path: &str) {
        let pane = self.panes.iter().find(|p| p.id == self.focus).and_then(|p| match &p.kind {
            PaneKind::Backtest(b) => Some(b),
            _ => None,
        });
        let pane = pane.or_else(|| {
            self.panes.iter().find_map(|p| match &p.kind {
                PaneKind::Backtest(b) => Some(b),
                _ => None,
            })
        });
        let Some(pane) = pane else {
            self.status = "EXPORT BACKTEST needs an open BACKTEST pane with results".into();
            return;
        };
        let Some(report) = &pane.report else {
            self.status = "BACKTEST has no results to export yet".into();
            return;
        };
        match env::backtest_document(report, env::now_rfc3339()) {
            Ok(doc) => self.finish_write(&doc, path, "Exported backtest"),
            Err(e) => self.status = e,
        }
    }

    fn export_lookalike(&mut self, path: &str) {
        let pane = self.panes.iter().find(|p| p.id == self.focus).and_then(|p| match &p.kind {
            PaneKind::Lookalike(l) => Some(l),
            _ => None,
        });
        let pane = pane.or_else(|| {
            self.panes.iter().find_map(|p| match &p.kind {
                PaneKind::Lookalike(l) => Some(l),
                _ => None,
            })
        });
        let Some(pane) = pane else {
            self.status = "EXPORT LOOKALIKE needs an open LOOKALIKE pane with results".into();
            return;
        };
        let Some(report) = &pane.report else {
            self.status = "LOOKALIKE has no results to export yet".into();
            return;
        };
        match env::lookalike_document(report, env::now_rfc3339()) {
            Ok(doc) => self.finish_write(&doc, path, "Exported lookalikes"),
            Err(e) => self.status = e,
        }
    }

    fn export_journal(&mut self, path: &str) {
        let doc = env::journal_document(&self.journal, env::now_rfc3339());
        self.finish_write(&doc, path, "Exported journal");
    }

    fn export_history(&mut self, path: &str) {
        let dest = resolve_path((!path.trim().is_empty()).then_some(path), Kind::History, false);
        self.worker.send(Request::ExportHistory { path: dest, symbols: Vec::new() });
        self.status = "Exporting price history…".into();
    }

    pub(super) fn backup_workspace(&mut self, path: &str) {
        self.save_layout();
        self.save_prefs();
        self.save_settings();
        self.persist_journal();
        let settings = self.current_settings();
        let alerts = tr_data::price_alerts::Store::load(&tr_data::price_alerts::store_path(crate::worker::data_dir()));
        match portable::build_workspace(
            &self.prefs,
            &settings,
            &self.store,
            &self.themes,
            &self.journal,
            &alerts,
            &self.palette.recents,
        ) {
            Ok(doc) => self.finish_write(&doc, path, "Backed up workspace"),
            Err(e) => self.status = format!("Backup failed: {e:#}"),
        }
    }

    pub(super) fn restore_portable(&mut self, path: &str) {
        let path = portable::expand_home(path.trim());
        if path.as_os_str().is_empty() {
            self.status = "RESTORE needs a file path".into();
            return;
        }
        let doc = match Document::read(&path) {
            Ok(d) => d,
            Err(e) => {
                self.status = format!("Restore failed: {e}");
                return;
            }
        };
        if let Some(banner) = portable::sample_banner(&doc) {
            self.status = banner.to_string();
        }
        match doc.kind {
            Kind::Workspace => self.restore_workspace(doc),
            Kind::Journal => match env::merge_journal(&mut self.journal, &doc) {
                Ok(n) => {
                    self.persist_journal();
                    self.status = format!("Restored {n} journal rows from {}", path.display());
                }
                Err(e) => self.status = format!("Journal restore: {e}"),
            },
            Kind::History => {
                self.worker.send(Request::ImportHistory { path });
                self.status = "Restoring price history…".into();
            }
            Kind::Scan => {
                self.status =
                    "Scan exports are snapshots and are not restored into SCAN. Open the JSON file instead.".into();
            }
            Kind::Stats => {
                self.status =
                    "Statistics exports are snapshots and are not restored into STATS. Open the JSON file instead."
                        .into();
            }
            Kind::Backtest => {
                self.status =
                    "Backtest exports are snapshots and are not restored into BACKTEST. Open the JSON file instead."
                        .into();
            }
            Kind::Lookalike => {
                self.status =
                    "Lookalike exports are snapshots and are not restored into LOOKALIKE. Open the JSON file instead."
                        .into();
            }
        }
    }

    fn restore_workspace(&mut self, doc: Document) {
        let payload = match portable::parse_workspace(&doc) {
            Ok(p) => p,
            Err(e) => {
                self.status = format!("Restore failed: {e:#}");
                return;
            }
        };
        if let Ok(prefs) = serde_json::from_value::<crate::prefs::Prefs>(payload.prefs.clone()) {
            self.prefs = prefs;
            self.save_prefs();
        }
        if let Ok(settings) = serde_json::from_value::<Settings>(payload.settings.clone()) {
            self.watchlists = settings
                .watchlists
                .clone()
                .unwrap_or_else(|| super::watchlists::Watchlists::migrate(&settings.watchlist));
            self.watchlist = self.watchlists.symbols();
            self.sidebar_open = settings.sidebar;
            self.recent_symbols = super::canonical_recent_symbols(&settings.recent_symbols);
            if let Some(name) = settings.active_layout.clone()
                && self.store.exists(&name)
            {
                self.layout_name = name;
            }
            self.save_settings();
        }
        match apply_layouts(&self.store, &payload.layouts) {
            Ok(_) => self.refresh_layout_names(),
            Err(e) => {
                self.status = format!("Layout restore: {e:#}");
                return;
            }
        }
        if let Err(e) = apply_themes(&self.theme_store, &payload.themes) {
            self.status = format!("Theme restore: {e:#}");
            return;
        }
        self.themes = self.theme_store.load_all();
        self.journal = payload.journal;
        self.persist_journal();
        self.worker.send(Request::ReplacePriceAlerts(payload.price_alerts));
        self.palette.recents = payload.palette;
        if let Some(active) = self.store.list().into_iter().find(|n| n.eq_ignore_ascii_case(&self.layout_name)) {
            if let Ok(layout) = self.store.load(&active) {
                self.apply_doc(layout);
            }
        }
        let sample = if doc.sample { format!(" · {}", env::SAMPLE_EXPORT_LABEL) } else { String::new() };
        self.status = format!("Restored workspace backup{sample}");
    }

    fn finish_write(&mut self, doc: &Document, path: &str, verb: &str) {
        let dest = resolve_path((!path.trim().is_empty()).then_some(path), doc.kind, doc.sample);
        match write_document(doc, &dest) {
            Ok(p) => {
                self.status = if doc.sample {
                    format!("{verb} (SAMPLE) to {} · {}", p.display(), env::SAMPLE_EXPORT_LABEL)
                } else {
                    format!("{verb} to {}", p.display())
                };
            }
            Err(e) => self.status = format!("Export failed: {e:#}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::app::tests::test_app;
    use crate::command::{ExportKind, Function};
    use tr_data::journal::{SetupDraft, default_checklist};

    #[test]
    fn journal_export_restore_and_workspace_backup_exclude_secrets() {
        let mut app = test_app();
        app.journal
            .add_setup(
                SetupDraft {
                    symbol: "AAPL".into(),
                    chart_date: "2026-10-24".into(),
                    reasoning: "breakout".into(),
                    planned_risk: 100.0,
                    checklist: default_checklist(),
                    chart_provenance: tr_core::Provenance::Real,
                },
                1,
            )
            .unwrap();
        let out = app.store.dir.parent().unwrap().join("exports");
        let journal_path = out.join("journal-export.json");
        app.run_export(ExportKind::Journal, journal_path.to_str().unwrap());
        let text = std::fs::read_to_string(&journal_path).unwrap_or_else(|_| panic!("{}", app.status));
        assert!(text.contains("\"kind\": \"journal\""));
        assert!(text.contains("2026-10-24"));
        assert!(text.contains("credentials_excluded"));
        app.journal = Default::default();
        app.restore_portable(journal_path.to_str().unwrap());
        assert_eq!(app.journal.setups.len(), 1);

        let backup_path = out.join("workspace.json");
        app.backup_workspace(backup_path.to_str().unwrap());
        let backup = std::fs::read_to_string(&backup_path).unwrap_or_else(|_| panic!("{}", app.status));
        assert!(backup.contains("\"kind\": \"workspace\""));
        assert!(!backup.contains("user_secret"));
        assert!(!backup.contains("snaptrade"));
        assert!(app.status.contains("Backed up workspace"));
        app.apply(Function::Export(ExportKind::Auto, String::new()));
        assert_eq!(app.settings_tab, super::super::settings_view::SettingsTab::Backup);
    }
}
