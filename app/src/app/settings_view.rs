//! Preferences panel (`PREFS`): appearance, theme editor, hotkeys, panel defaults and
//! saved layouts.

use egui::{RichText, Ui};
use tr_chart::scene::ChartType;
use tr_core::Timeframe;

use super::TerminalApp;
use super::workspace::LayoutOp;
use crate::prefs::{Action, Density, MonoFont, UiFont};
use crate::theme::{self, muted, text, ui_font};
use crate::themes::{self, Hex, SYSTEM, Theme};
use crate::widgets;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    Appearance,
    Themes,
    Hotkeys,
    Panels,
    Layouts,
    Brokerage,
    Trading,
    WhatsNews,
    Backup,
}

impl SettingsTab {
    pub(super) const ALL: [SettingsTab; 9] = [
        SettingsTab::Appearance,
        SettingsTab::Themes,
        SettingsTab::Hotkeys,
        SettingsTab::Panels,
        SettingsTab::Layouts,
        SettingsTab::Brokerage,
        SettingsTab::Trading,
        SettingsTab::WhatsNews,
        SettingsTab::Backup,
    ];

    pub(super) fn label(self) -> &'static str {
        match self {
            SettingsTab::Appearance => "Appearance",
            SettingsTab::Themes => "Themes",
            SettingsTab::Hotkeys => "Hotkeys",
            SettingsTab::Panels => "Panels",
            SettingsTab::Layouts => "Layouts",
            SettingsTab::Brokerage => "Brokerage",
            SettingsTab::Trading => "Trading",
            SettingsTab::WhatsNews => "Whats-News",
            SettingsTab::Backup => "Backup",
        }
    }
}

pub enum SettingsCmd {
    Layout(LayoutOp),
    SavePrefs,
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(theme::dp(10.0));
    ui.label(RichText::new(title).font(ui_font(13.5)).color(text()));
    ui.separator();
}

/// A gallery card: one theme, or System's dark/light pair.
enum Card {
    Theme(Theme),
    System(Theme, Theme),
}

/// Paint a miniature of `t` (or a split dark/light card for System) into `r`.
fn theme_swatch(p: &egui::Painter, r: egui::Rect, t: &Theme) {
    let c = &t.colors;
    p.rect_filled(r, 6.0, c.background.0);
    let panel = r.shrink(5.0);
    p.rect_filled(panel, 4.0, c.panel.0);
    let hdr = egui::Rect::from_min_size(panel.min, egui::vec2(panel.width(), 9.0));
    p.rect_filled(hdr, egui::CornerRadius { nw: 4, ne: 4, sw: 0, se: 0 }, c.panel_header.0);
    let y = hdr.bottom() + 9.0;
    p.text(egui::pos2(panel.left() + 5.0, y), egui::Align2::LEFT_CENTER, "AAPL", theme::mono(9.0), c.highlight.0);
    p.text(egui::pos2(panel.left() + 36.0, y), egui::Align2::LEFT_CENTER, "230.15", theme::mono(9.0), c.text.0);
    p.text(egui::pos2(panel.right() - 5.0, y), egui::Align2::RIGHT_CENTER, "+1.2%", theme::mono(9.0), c.up.0);
    // Two candles and a grid line.
    let base = panel.bottom() - 6.0;
    p.hline(panel.x_range().shrink(4.0), base - 9.0, egui::Stroke::new(1.0, c.grid.0));
    for (k, (h, up)) in [(14.0, true), (9.0, false), (17.0, true), (7.0, false)].into_iter().enumerate() {
        let x = panel.left() + 8.0 + k as f32 * 9.0;
        let col = if up { c.candle_up.0 } else { c.candle_down.0 };
        p.rect_filled(egui::Rect::from_min_max(egui::pos2(x, base - h), egui::pos2(x + 5.0, base)), 1.0, col);
    }
    let accent = c.accent.map_or(theme::accent(), |a| a.0);
    p.circle_filled(egui::pos2(panel.right() - 9.0, base - 6.0), 4.0, accent);
}

/// Colour picker row with alpha; returns whether it changed.
fn color_row(ui: &mut Ui, label: &str, hex: &mut Hex) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.add_sized([150.0, 20.0], egui::Label::new(RichText::new(label).font(ui_font(12.5))));
        let mut c = hex.0;
        if egui::color_picker::color_edit_button_srgba(ui, &mut c, egui::color_picker::Alpha::OnlyBlend).changed() {
            *hex = Hex(c);
            changed = true;
        }
        ui.label(RichText::new(hex.format()).font(theme::mono(11.0)).color(muted()));
    });
    changed
}

fn row(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized([140.0, 20.0], egui::Label::new(RichText::new(label).font(ui_font(12.5))));
        add(ui);
    });
}

impl TerminalApp {
    pub(super) fn settings_panel(&mut self, ui: &mut Ui) -> Vec<SettingsCmd> {
        let mut cmds = Vec::new();
        let labels: Vec<&str> = SettingsTab::ALL.iter().map(|t| t.label()).collect();
        let sel = SettingsTab::ALL.iter().position(|t| *t == self.settings_tab);
        if let Some(i) = widgets::segmented(ui, "settings-tabs", &labels, sel, 24.0) {
            self.settings_tab = SettingsTab::ALL[i];
            self.rebinding = None;
        }
        let width = ui.available_width() - 14.0;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(width);
            ui.spacing_mut().item_spacing.y = theme::dp(6.0);
            match self.settings_tab {
                SettingsTab::Appearance => self.appearance_tab(ui, &mut cmds),
                SettingsTab::Themes => self.themes_tab(ui),
                SettingsTab::Hotkeys => self.hotkeys_tab(ui, &mut cmds),
                SettingsTab::Panels => self.panels_tab(ui, &mut cmds),
                SettingsTab::Layouts => self.layouts_tab(ui, &mut cmds),
                SettingsTab::Brokerage => self.brokerage_tab(ui),
                SettingsTab::Trading => self.trading_tab(ui, &mut cmds),
                SettingsTab::WhatsNews => self.whats_news_tab(ui),
                SettingsTab::Backup => self.backup_tab(ui),
            }
        });
        cmds
    }

    pub(super) fn run_settings_cmds(&mut self, cmds: Vec<SettingsCmd>) {
        for c in cmds {
            match c {
                SettingsCmd::Layout(op) => self.layout_op(op),
                SettingsCmd::SavePrefs => self.save_prefs(),
            }
        }
    }

    fn whats_news_tab(&mut self, ui: &mut Ui) {
        section(ui, "Whats-News");
        ui.label("Read-only daily price history for research, SCAN, UNIV and archive charts.");
        let before = self.prefs.whats_news.clone();
        let config = &mut self.prefs.whats_news;
        ui.checkbox(&mut config.enabled, "Enable Whats-News connection");
        ui.label("SQLite archive path (leave blank to auto-detect)");
        ui.add(
            egui::TextEdit::singleline(&mut config.archive_path).hint_text("~/wn-fix/finance.db").desired_width(500.0),
        );
        ui.horizontal(|ui| {
            if ui.button("Auto-detect archive").clicked() {
                config.archive_path.clear();
            }
            let path = config.archive();
            ui.label(if path.as_os_str().is_empty() {
                "No default archive found".into()
            } else {
                format!("Archive: {}", path.display())
            });
        });
        ui.label("Optional local HTTP URL (leave blank for archive only)");
        ui.add(
            egui::TextEdit::singleline(&mut config.http_url)
                .hint_text(tr_data::whats_news::DEFAULT_URL)
                .desired_width(500.0),
        );
        ui.label("Main app: http://127.0.0.1:8050 · optional data service: http://127.0.0.1:8051");
        ui.checkbox(&mut config.prefer_http, "Prefer HTTP (otherwise prefer the archive file)");
        ui.label(
            "If the preferred source is unavailable, the other configured source is tried. No sample data is used.",
        );
        let test = ui.button("Test connection").clicked();
        if before != self.prefs.whats_news || test {
            self.save_prefs();
            self.apply_whats_news_config();
        }
        ui.separator();
        ui.label(self.whats_news_health.summary());
        ui.label(&self.whats_news_health.archive_validation);
        if !self.whats_news_health.message.is_empty() {
            ui.colored_label(crate::theme::amber(), &self.whats_news_health.message);
        }
        ui.label("Settings apply immediately. Connection checks and recovery run in the background.");
    }

    pub(super) fn apply_whats_news_config(&mut self) {
        self.whats_news_epoch += 1;
        self.whats_news_health = tr_data::whats_news::Health::pending();
        self.worker.send(crate::worker::Request::ConfigureWhatsNews {
            epoch: self.whats_news_epoch,
            config: self.prefs.whats_news.clone(),
        });
        self.refresh_whats_news_panes();
    }

    pub(super) fn refresh_whats_news_panes(&mut self) {
        self.command_catalogue = Default::default();
        let desk: Vec<(crate::dock::PaneId, String)> = self
            .panes
            .iter()
            .filter_map(|p| match &p.kind {
                super::PaneKind::Desk(d) => Some((p.id, d.group.clone())),
                _ => None,
            })
            .collect();
        for pane in &mut self.panes {
            match &mut pane.kind {
                super::PaneKind::Research(r) => r.request(pane.id, &self.worker),
                super::PaneKind::Chart(c) if c.archive => c.load(&self.worker),
                super::PaneKind::Stats(s) => s.request(pane.id, &self.worker),
                super::PaneKind::Backtest(b) => b.request(pane.id, &self.worker),
                super::PaneKind::Lookalike(l) => l.request(pane.id, &self.worker),
                _ => {}
            }
        }
        for (id, group) in desk {
            let symbols = self
                .watchlists
                .group(&group)
                .map(|g| g.members.iter().map(|m| m.symbol.clone()).collect())
                .unwrap_or_default();
            if let Some(super::PaneKind::Desk(d)) = self.panes.iter_mut().find(|p| p.id == id).map(|p| &mut p.kind) {
                d.request(id, &self.worker, symbols);
            }
        }
    }

    fn appearance_tab(&mut self, ui: &mut Ui, cmds: &mut Vec<SettingsCmd>) {
        section(ui, "Theme");
        // A gallery of every theme; System shows the dark/light pair it follows.
        let current = self.prefs.appearance.theme.clone();
        let find = |n: &str| themes::find(&self.themes, n).cloned().unwrap_or_else(themes::terminal_dark);
        let cards: Vec<(String, Card)> = self
            .theme_choices()
            .into_iter()
            .map(|name| {
                let card = if name == SYSTEM {
                    Card::System(find(&self.prefs.appearance.dark_theme), find(&self.prefs.appearance.light_theme))
                } else {
                    Card::Theme(find(&name))
                };
                (name, card)
            })
            .collect();
        let mut pick = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
            for (name, card) in &cards {
                let (r, resp) = ui.allocate_exact_size(egui::vec2(128.0, 92.0), egui::Sense::click());
                let p = ui.painter();
                let sw = egui::Rect::from_min_size(r.min, egui::vec2(r.width(), 68.0));
                match card {
                    Card::Theme(t) => theme_swatch(p, sw, t),
                    Card::System(d, l) => {
                        let (left, right) = sw.split_left_right_at_fraction(0.5);
                        theme_swatch(&p.with_clip_rect(left), sw, d);
                        theme_swatch(&p.with_clip_rect(right), sw, l);
                    }
                }
                let selected = name.eq_ignore_ascii_case(&current);
                let stroke = if selected {
                    egui::Stroke::new(2.5, theme::accent())
                } else if resp.hovered() {
                    egui::Stroke::new(1.5, theme::accent().gamma_multiply(0.6))
                } else {
                    egui::Stroke::new(1.0, theme::border())
                };
                p.rect_stroke(sw, 6.0, stroke, egui::StrokeKind::Outside);
                let custom = matches!(card, Card::Theme(t) if !t.builtin);
                let label = if custom { format!("{name} ✎") } else { name.clone() };
                p.with_clip_rect(r).text(
                    egui::pos2(r.center().x, sw.bottom() + 12.0),
                    egui::Align2::CENTER_CENTER,
                    label,
                    ui_font(12.0),
                    if selected { theme::accent() } else { text() },
                );
                if resp.clicked() {
                    pick = Some(name.clone());
                }
                resp.on_hover_text(if custom { "Custom theme" } else { "Built-in theme" });
            }
        });
        if let Some(name) = pick {
            self.select_theme(&name);
        }
        let names: Vec<String> = self.themes.iter().map(|t| t.name.clone()).collect();
        let mut pair_changed = false;
        let a = &mut self.prefs.appearance;
        for (label, slot) in [("System dark theme", &mut a.dark_theme), ("System light theme", &mut a.light_theme)] {
            row(ui, label, |ui| {
                egui::ComboBox::from_id_salt(label).selected_text(slot.as_str()).show_ui(ui, |ui| {
                    for n in &names {
                        if ui.selectable_label(slot == n, n).clicked() {
                            *slot = n.clone();
                            pair_changed = true;
                        }
                    }
                });
            });
        }
        if pair_changed {
            cmds.push(SettingsCmd::SavePrefs);
        }
        row(ui, "Layouts", |ui| {
            let mut on = self.layout_theme;
            if ui.checkbox(&mut on, format!("Save the theme with layout {:?}", self.layout_name)).changed() {
                self.layout_theme = on;
                self.save_layout();
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Edit Themes…").clicked() {
                self.settings_tab = SettingsTab::Themes;
            }
            ui.label(RichText::new("THEME <name> · THEME NEXT").font(theme::mono(11.0)).color(muted()));
        });

        section(ui, "Window");
        let before = self.prefs.appearance.clone();
        let a = &mut self.prefs.appearance;
        row(ui, "Sidebar position", |ui| {
            use crate::prefs::SidebarSide;
            let sides = [SidebarSide::Left, SidebarSide::Right];
            let sel = sides.iter().position(|s| *s == a.sidebar_side);
            if let Some(i) = widgets::segmented(ui, "sidebar-side", &["Left", "Right"], sel, 22.0) {
                a.sidebar_side = sides[i];
            }
        });
        if cfg!(target_os = "macos") {
            row(ui, "Sidebar vibrancy", |ui| {
                ui.checkbox(&mut a.vibrancy, "Translucent sidebar (applies on next launch)");
            });
        }
        if self.prefs.appearance != before {
            cmds.push(SettingsCmd::SavePrefs);
        }
    }

    /// Live theme editor. Editing a built-in creates a custom copy; custom themes save to
    /// `<data dir>/themes/<name>.toml` shortly after each change.
    fn themes_tab(&mut self, ui: &mut Ui) {
        section(ui, "Theme editor");
        if !self.themes.iter().any(|t| t.name == self.editing_theme) {
            let ti = self.active_theme_index(ui.ctx());
            self.editing_theme = self.themes[ti].name.clone();
        }
        let idx = self.themes.iter().position(|t| t.name == self.editing_theme).unwrap_or(0);
        let names: Vec<(String, bool)> = self.themes.iter().map(|t| (t.name.clone(), t.builtin)).collect();
        let mut op: Option<&'static str> = None;
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("edit-theme").selected_text(self.editing_theme.clone()).show_ui(ui, |ui| {
                for (n, builtin) in &names {
                    let label = if *builtin { n.clone() } else { format!("{n} ✎") };
                    if ui.selectable_label(*n == self.editing_theme, label).clicked() {
                        self.editing_theme = n.clone();
                        self.renaming_theme = None;
                    }
                }
            });
            for (label, key) in [("Use", "use"), ("Duplicate", "dup"), ("Export TOML", "toml"), ("Export JSON", "json")]
            {
                if ui.button(label).clicked() {
                    op = Some(key);
                }
            }
            if !self.themes[idx].builtin {
                if ui.button("Rename").clicked() {
                    self.renaming_theme = Some((self.editing_theme.clone(), self.editing_theme.clone()));
                }
                if ui.button("Delete").clicked() {
                    op = Some("delete");
                }
            }
            if ui.button("Open Folder").clicked() {
                op = Some("folder");
            }
        });
        if let Some((old, buf)) = &mut self.renaming_theme {
            let mut commit = None;
            ui.horizontal(|ui| {
                ui.label("New name");
                let r = ui.add(egui::TextEdit::singleline(buf).desired_width(220.0));
                if ui.button("Rename").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                    commit = Some((old.clone(), buf.trim().to_string()));
                }
                if ui.button("Cancel").clicked() {
                    commit = Some((String::new(), String::new()));
                }
            });
            if let Some((old, new)) = commit {
                self.renaming_theme = None;
                if !old.is_empty() {
                    self.rename_theme(&old, &new);
                }
            }
        }
        if let Some(op) = op {
            self.theme_op(op, idx);
            return;
        }
        let idx = self.themes.iter().position(|t| t.name == self.editing_theme).unwrap_or(0);
        let info = match (&self.themes[idx].builtin, &self.themes[idx].source) {
            (true, _) => "Built-in: your first change creates an editable copy.".to_string(),
            (false, Some(p)) => format!("Custom · {}", p.display()),
            (false, None) => "Custom · not saved yet".to_string(),
        };
        ui.label(RichText::new(info).font(ui_font(11.5)).color(muted()));

        let mut t = self.themes[idx].clone();
        let (pr, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().min(420.0), 110.0), egui::Sense::hover());
        theme_swatch(ui.painter(), pr, &t);

        let mut changed = false;
        section(ui, "Base");
        row(ui, "Light or dark", |ui| {
            let sel = Some(if t.dark { 0 } else { 1 });
            if let Some(i) = widgets::segmented(ui, "theme-base", &["Dark", "Light"], sel, 22.0) {
                t.dark = i == 0;
                changed = true;
            }
        });
        let c = &mut t.colors;
        section(ui, "Surfaces");
        for (label, h) in [
            ("Background", &mut c.background),
            ("Sidebar", &mut c.sidebar),
            ("Panel", &mut c.panel),
            ("Panel header", &mut c.panel_header),
            ("Border", &mut c.border),
            ("Grid", &mut c.grid),
            ("Control", &mut c.control),
            ("Control hover", &mut c.control_hover),
        ] {
            changed |= color_row(ui, label, h);
        }
        section(ui, "Text");
        for (label, h) in [
            ("Text", &mut c.text),
            ("Muted", &mut c.muted),
            ("Highlight (tickers)", &mut c.highlight),
            ("Highlight dim", &mut c.highlight_dim),
            ("Info", &mut c.info),
        ] {
            changed |= color_row(ui, label, h);
        }
        section(ui, "Accent");
        let mut follow = c.accent.is_none();
        if ui.checkbox(&mut follow, "Follow the system accent colour").changed() {
            c.accent = if follow { None } else { Some(Hex(theme::accent())) };
            changed = true;
        }
        if let Some(a) = &mut c.accent {
            changed |= color_row(ui, "Accent", a);
        }
        section(ui, "Market and chart");
        for (label, h) in [
            ("Up / gains", &mut c.up),
            ("Down / losses", &mut c.down),
            ("Candle up", &mut c.candle_up),
            ("Candle down", &mut c.candle_down),
        ] {
            changed |= color_row(ui, label, h);
        }
        for (k, h) in c.series.iter_mut().enumerate() {
            changed |= color_row(ui, &format!("Series {}", k + 1), h);
        }
        section(ui, "Fonts and density");
        row(ui, "Interface font", |ui| {
            let fonts = [UiFont::System, UiFont::Monospace];
            let sel = fonts.iter().position(|f| *f == t.fonts.ui);
            if let Some(i) = widgets::segmented(ui, "theme-ui-font", &["System (SF Pro)", "Monospace"], sel, 22.0) {
                t.fonts.ui = fonts[i];
                changed = true;
            }
        });
        row(ui, "Numbers font", |ui| {
            egui::ComboBox::from_id_salt("theme-mono").selected_text(t.fonts.mono.label()).show_ui(ui, |ui| {
                for f in MonoFont::ALL {
                    let available =
                        f == MonoFont::Bundled || f.paths().iter().any(|p| std::path::Path::new(p).exists());
                    let label =
                        if available { f.label().to_string() } else { format!("{} (not installed)", f.label()) };
                    if ui.add_enabled(available, egui::Button::selectable(t.fonts.mono == f, label)).clicked() {
                        t.fonts.mono = f;
                        changed = true;
                    }
                }
            });
        });
        row(ui, "Text size", |ui| {
            changed |= ui
                .add(
                    egui::Slider::new(&mut t.fonts.scale, 0.8..=1.4)
                        .step_by(0.05)
                        .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                )
                .changed();
        });
        row(ui, "Density", |ui| {
            let labels: Vec<&str> = Density::ALL.iter().map(|d| d.label()).collect();
            let sel = Density::ALL.iter().position(|d| *d == t.density);
            if let Some(i) = widgets::segmented(ui, "theme-density", &labels, sel, 22.0) {
                t.density = Density::ALL[i];
                changed = true;
            }
        });
        if changed {
            let i2 = self.editable_theme(idx);
            let target = &mut self.themes[i2];
            t.name = target.name.clone();
            t.builtin = false;
            t.source = target.source.clone();
            *target = t;
            let name = target.name.clone();
            self.editing_theme = name.clone();
            self.mark_theme_dirty(&name);
        }

        section(ui, "Import");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.theme_io).hint_text("~/Downloads/theme.toml").desired_width(300.0),
            );
            if ui.button("Import").clicked() && !self.theme_io.trim().is_empty() {
                self.theme_op("import", idx);
            }
        });
        ui.label(
            RichText::new(
                "Themes are TOML (or JSON) files; a file may set only the fields it changes. \
                 Export writes to the path above, or ~/Downloads.",
            )
            .font(ui_font(11.5))
            .color(muted()),
        );
    }

    fn rename_theme(&mut self, old: &str, new: &str) {
        if new.is_empty() || new.eq_ignore_ascii_case(SYSTEM) {
            self.status = "Theme names cannot be empty or \"System\"".into();
            return;
        }
        if !new.eq_ignore_ascii_case(old) && self.themes.iter().any(|t| t.name.eq_ignore_ascii_case(new)) {
            self.status = format!("A theme named {new:?} already exists");
            return;
        }
        let Some(i) = self.themes.iter().position(|t| t.name == old && !t.builtin) else { return };
        let old_file = self.themes[i].source.take();
        self.themes[i].name = new.to_string();
        match self.theme_store.save(&self.themes[i]) {
            Ok(path) => {
                if let Some(f) = old_file.filter(|f| *f != path) {
                    let _ = std::fs::remove_file(f);
                }
                self.themes[i].source = Some(path);
            }
            Err(e) => self.status = format!("Rename failed: {e:#}"),
        }
        let a = &mut self.prefs.appearance;
        for slot in [&mut a.theme, &mut a.dark_theme, &mut a.light_theme] {
            if slot == old {
                *slot = new.to_string();
            }
        }
        self.editing_theme = new.to_string();
        self.save_prefs();
        self.status = format!("Renamed theme {old:?} to {new:?}");
    }

    fn theme_op(&mut self, op: &str, idx: usize) {
        let expand = |p: &str| match p.strip_prefix("~/") {
            Some(rest) => std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default().join(rest),
            None => p.into(),
        };
        let name = self.themes[idx].name.clone();
        match op {
            "use" => {
                self.select_theme(&name);
            }
            "dup" => {
                let mut t = self.themes[idx].clone();
                t.name = themes::unique_name(&self.themes, &format!("{name} Copy"));
                t.builtin = false;
                t.source = None;
                let n = t.name.clone();
                self.themes.push(t);
                self.mark_theme_dirty(&n);
                self.editing_theme = n.clone();
                self.status = format!("Duplicated {name:?} as {n:?}");
            }
            "toml" | "json" => {
                let given = self.theme_io.trim();
                let path = if given.is_empty() {
                    let dl = expand("~/Downloads");
                    let dir = if dl.is_dir() { dl } else { self.theme_store.dir.join("exports") };
                    dir.join(format!("{}.{op}", crate::layouts::Store::file_stem(&name)))
                } else {
                    expand(given).with_extension(op)
                };
                self.status = match themes::ThemeStore::export(&self.themes[idx], &path) {
                    Ok(()) => format!("Exported {name:?} to {}", path.display()),
                    Err(e) => format!("Export failed: {e:#}"),
                };
            }
            "delete" => {
                let t = self.themes[idx].clone();
                match self.theme_store.delete(&t) {
                    Ok(()) | Err(_) if t.source.is_none() => {}
                    Err(e) => {
                        self.status = format!("Delete failed: {e:#}");
                        return;
                    }
                    Ok(()) => {}
                }
                self.themes.remove(idx);
                self.dirty_themes.retain(|n| *n != name);
                let a = &mut self.prefs.appearance;
                if a.theme == name {
                    a.theme = SYSTEM.into();
                }
                if a.dark_theme == name {
                    a.dark_theme = "Terminal Dark".into();
                }
                if a.light_theme == name {
                    a.light_theme = "Terminal Light".into();
                }
                self.save_prefs();
                self.editing_theme = self.prefs.appearance.theme.clone();
                self.status = format!("Deleted theme {name:?}");
            }
            "import" => {
                let path = expand(self.theme_io.trim());
                match themes::load_file(&path) {
                    Ok(mut t) => {
                        t.name = themes::unique_name(&self.themes, &t.name);
                        t.source = None;
                        let n = t.name.clone();
                        self.themes.push(t);
                        self.mark_theme_dirty(&n);
                        self.save_dirty_themes(true);
                        self.editing_theme = n.clone();
                        self.status = format!("Imported theme {n:?} from {}", path.display());
                    }
                    Err(e) => self.status = format!("Import failed: {e:#}"),
                }
            }
            "folder" => {
                let _ = std::fs::create_dir_all(&self.theme_store.dir);
                let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
                if let Err(e) = std::process::Command::new(opener).arg(&self.theme_store.dir).spawn() {
                    self.status = format!("Could not open {}: {e}", self.theme_store.dir.display());
                }
            }
            _ => {}
        }
    }

    fn hotkeys_tab(&mut self, ui: &mut Ui, cmds: &mut Vec<SettingsCmd>) {
        section(ui, "Keyboard shortcuts");
        if self.hotkey_table(ui) {
            cmds.push(SettingsCmd::SavePrefs);
        }
    }

    fn panels_tab(&mut self, ui: &mut Ui, cmds: &mut Vec<SettingsCmd>) {
        let before = self.prefs.panels.clone();
        let d = &mut self.prefs.panels;
        section(ui, "New charts");
        row(ui, "Chart type", |ui| {
            let labels: Vec<&str> = ChartType::ALL.iter().map(|c| c.label()).collect();
            let sel = ChartType::ALL.iter().position(|c| *c == d.chart_type);
            if let Some(i) = widgets::segmented(ui, "def-ct", &labels, sel, 22.0) {
                d.chart_type = ChartType::ALL[i];
            }
        });
        row(ui, "Interval", |ui| {
            let labels: Vec<&str> = Timeframe::ALL.iter().map(|t| t.label()).collect();
            let sel = Timeframe::ALL.iter().position(|t| *t == d.timeframe);
            if let Some(i) = widgets::segmented(ui, "def-tf", &labels, sel, 22.0) {
                d.timeframe = Timeframe::ALL[i];
            }
        });
        row(ui, "Show", |ui| {
            ui.checkbox(&mut d.show_volume, "Volume");
            ui.checkbox(&mut d.congress_markers, "Congress trade markers");
            ui.checkbox(&mut d.news_markers, "News catalyst markers");
        });
        section(ui, "New order books");
        row(ui, "Depth (levels)", |ui| {
            let depths = [10usize, 20, 50];
            if let Some(i) = widgets::segmented(
                ui,
                "def-depth",
                &["10", "20", "50"],
                depths.iter().position(|&x| x == d.book_depth),
                22.0,
            ) {
                d.book_depth = depths[i];
            }
        });
        ui.add_space(6.0);
        ui.label(
            RichText::new("Each pane's own settings (chart type, indicators, filters, depth, link group, tab title) are saved with the layout.")
                .font(ui_font(11.5))
                .color(muted()),
        );
        if self.prefs.panels != before {
            cmds.push(SettingsCmd::SavePrefs);
        }
    }

    fn layouts_tab(&mut self, ui: &mut Ui, cmds: &mut Vec<SettingsCmd>) {
        section(ui, "Saved layouts");
        ui.label(
            RichText::new(format!("Stored as JSON in {}", self.store.dir.display())).font(ui_font(11.5)).color(muted()),
        );
        let names = self.layout_names.clone();
        egui::Grid::new("layouts").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
            for (i, name) in names.iter().enumerate() {
                let current = name.eq_ignore_ascii_case(&self.layout_name);
                let hint = self
                    .prefs
                    .hotkeys
                    .chord(
                        Action::ALL
                            .iter()
                            .copied()
                            .filter(|a| a.layout_slot().is_some())
                            .nth(i)
                            .unwrap_or(Action::Help),
                    )
                    .filter(|_| i < 9)
                    .map(|c| format!("   {}", c.display()))
                    .unwrap_or_default();
                let label = RichText::new(format!("{}{name}{hint}", if current { "● " } else { "   " }))
                    .font(ui_font(12.5))
                    .color(if current { theme::accent() } else { text() });
                ui.label(label);
                ui.horizontal(|ui| {
                    if !current && ui.small_button("Switch").clicked() {
                        cmds.push(SettingsCmd::Layout(LayoutOp::Switch(name.clone())));
                    }
                    if ui.small_button("Rename").clicked() {
                        self.renaming_layout = Some((name.clone(), name.clone()));
                        self.sidebar_open = true;
                    }
                    if ui.small_button("Duplicate").clicked() {
                        cmds.push(SettingsCmd::Layout(LayoutOp::Duplicate(name.clone(), None)));
                    }
                    if ui.small_button("Export").clicked() {
                        cmds.push(SettingsCmd::Layout(LayoutOp::Export(name.clone(), None)));
                    }
                    if ui.small_button("Delete").clicked() {
                        cmds.push(SettingsCmd::Layout(LayoutOp::Delete(name.clone())));
                    }
                });
                ui.end_row();
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("New Layout").clicked() {
                cmds.push(SettingsCmd::Layout(LayoutOp::New(None)));
            }
            if ui.button("Open Layouts Folder").clicked() {
                cmds.push(SettingsCmd::Layout(LayoutOp::OpenFolder));
            }
        });
        section(ui, "Import / export");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.io_path).hint_text("~/Downloads/layout.json").desired_width(320.0),
            );
            if ui.button("Import").clicked() && !self.io_path.trim().is_empty() {
                let p = self.io_path.trim();
                let p = match p.strip_prefix("~/") {
                    Some(rest) => std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default().join(rest),
                    None => p.into(),
                };
                cmds.push(SettingsCmd::Layout(LayoutOp::Import(p)));
            }
            if ui.button("Export Current").clicked() {
                let p = self.io_path.trim();
                let path = (!p.is_empty()).then(|| match p.strip_prefix("~/") {
                    Some(rest) => std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default().join(rest),
                    None => p.into(),
                });
                cmds.push(SettingsCmd::Layout(LayoutOp::Export(self.layout_name.clone(), path)));
            }
        });
        ui.label(
            RichText::new(
                "Export writes to the path given, or ~/Downloads. Imported layouts are copied into the layouts folder.",
            )
            .font(ui_font(11.5))
            .color(muted()),
        );
    }

    fn backup_tab(&mut self, ui: &mut Ui) {
        section(ui, "Export and recovery");
        ui.label(
            RichText::new(
                "Export scan results, archived daily prices, the trade journal and statistics with source and date metadata. \
                 Back up or restore personal workspace data (layouts, themes, prefs, watchlists, journal, alerts). \
                 SnapTrade keys and other credentials are never written into a portable file.",
            )
            .font(ui_font(11.5))
            .color(muted()),
        );
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.io_path)
                    .hint_text("~/Downloads/terminal-rs-workspace.json")
                    .desired_width(360.0),
            );
        });
        ui.horizontal_wrapped(|ui| {
            if ui.button("Export scan").clicked() {
                self.run_export(crate::command::ExportKind::Scan, &self.io_path.clone());
            }
            if ui.button("Export journal").clicked() {
                self.run_export(crate::command::ExportKind::Journal, &self.io_path.clone());
            }
            if ui.button("Export price history").clicked() {
                self.run_export(crate::command::ExportKind::History, &self.io_path.clone());
            }
            if ui.button("Export statistics").clicked() {
                self.run_export(crate::command::ExportKind::Stats, &self.io_path.clone());
            }
            if ui.button("Export backtest").clicked() {
                self.run_export(crate::command::ExportKind::Backtest, &self.io_path.clone());
            }
            if ui.button("Export lookalikes").clicked() {
                self.run_export(crate::command::ExportKind::Lookalike, &self.io_path.clone());
            }
        });
        ui.horizontal_wrapped(|ui| {
            if ui.button("Back up workspace").clicked() {
                self.backup_workspace(&self.io_path.clone());
            }
            if ui.button("Restore…").clicked() && !self.io_path.trim().is_empty() {
                self.restore_portable(&self.io_path.clone());
            }
        });
        ui.label(
            RichText::new(
                "Empty path writes to ~/Downloads (or the exports folder). Sample-tagged journal or scan rows \
                 produce a file named *SAMPLE*. RESTORE accepts a workspace backup, a journal export, or a history export. \
                 Scan JSON is a snapshot only.",
            )
            .font(ui_font(11.5))
            .color(muted()),
        );
        ui.add_space(8.0);
        ui.label(
            RichText::new(format!(
                "Commands: EXPORT · EXPORT SCAN · EXPORT JOURNAL · EXPORT HIST · EXPORT STATS · EXPORT BACKTEST · EXPORT LOOKALIKE · BACKUP · RESTORE <path>. Files stay out of {}/secrets.",
                crate::worker::data_dir().display()
            ))
            .font(ui_font(11.5))
            .color(muted()),
        );
    }
}

#[cfg(test)]
mod connection_tests {
    use super::*;
    use crate::app::{PaneKind, research::ResearchPane};
    use crate::worker::{Request, Response, Worker};
    use tr_data::whats_news::{Health, LinkState, Snapshot};

    #[test]
    fn live_changes_reject_old_results_and_panes_follow_drop_and_recovery() {
        let mut app = crate::app::tests::test_app();
        let (worker, mut requests, out) = Worker::test_channel_with_responses();
        app.worker = worker;
        app.open_panel(PaneKind::Research(Box::new(ResearchPane::new(None))));
        let pane = app.focus;
        while requests.try_recv().is_ok() {}
        app.prefs.whats_news.http_url = "http://localhost:8051".into();
        app.apply_whats_news_config();
        assert!(
            matches!(requests.try_recv(), Ok(Request::ConfigureWhatsNews { epoch: 1, config }) if config.http_url.ends_with("8051"))
        );
        let connected = Health {
            state: LinkState::Http,
            source: "Whats-News HTTP: http://localhost:8051".into(),
            symbol_count: 2,
            archive_validation: "test archive validated".into(),
            latest_date: Some("2026-10-20".into()),
            message: String::new(),
        };
        out.send(Response::WhatsNewsHealth { epoch: 1, health: connected.clone() }).unwrap();
        app.drain();
        let before_drop_generation = match requests.try_recv().unwrap() {
            Request::Research { pane: id, generation, .. } if id == pane => generation,
            _ => panic!("Expected a research request after the connection became healthy"),
        };
        out.send(Response::WhatsNews {
            epoch: 0,
            response: Box::new(Response::Universe {
                pane,
                generation: 0,
                result: Ok(Snapshot {
                    source: "old source".into(),
                    provenance: tr_core::Provenance::Real,
                    data: vec![],
                }),
            }),
        })
        .unwrap();
        app.drain();
        let research = |app: &TerminalApp| -> (bool, Option<String>, bool) {
            let PaneKind::Research(r) = &app.panes.iter().find(|p| p.id == pane).unwrap().kind else { panic!() };
            (r.loading, r.error.clone(), r.universe.is_some())
        };
        assert_eq!(research(&app), (true, None, false));
        let message = "Whats-News isn't running. Start it or switch to the archive file.";
        let dropped = Health { state: LinkState::Disconnected, message: message.into(), ..connected.clone() };
        out.send(Response::WhatsNewsHealth { epoch: 1, health: dropped }).unwrap();
        app.drain();
        assert_eq!(research(&app), (true, None, false));
        // Local archive remains available when Whats-News drops; worker rechecks both.
        assert!(matches!(requests.try_recv(), Ok(Request::Research { pane: id, .. }) if id == pane));
        out.send(Response::WhatsNewsHealth { epoch: 1, health: connected }).unwrap();
        app.drain();
        assert_eq!(research(&app), (true, None, false));
        assert!(matches!(requests.try_recv(), Ok(Request::Research { pane: id, .. }) if id == pane));
        out.send(Response::WhatsNews {
            epoch: 1,
            response: Box::new(Response::Universe {
                pane,
                generation: before_drop_generation,
                result: Err("obsolete request from before the disconnect".into()),
            }),
        })
        .unwrap();
        app.drain();
        assert_eq!(research(&app), (true, None, false));
    }
}
