//! Top-level terminal: command line, docked panes, saved layouts, hotkeys and status bar.
//!
//! Submodules: `chrome` (toolbar, sidebar, status bar), `workspace` (dock rendering,
//! drag and drop, pop-out windows, link groups, saved layouts), `panels` (per-kind pane
//! content) and `settings_view` (preferences UI).

mod alerts;
mod backtest;
mod backup;
mod broker;
mod chrome;
mod command_bar;
mod demo;
mod desk;
mod hotkeys_view;
mod journal;
mod options_view;
mod palette_view;
mod panels;
mod portfolio_view;
mod research;
mod settings_view;
mod settings_writer;
mod stats;
mod theming;
mod trading;
mod watchlists;
mod workspace;

pub use broker::PosSort;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use egui::{Color32, Event, Key, Modifiers, Rect, Ui, UiBuilder, pos2, vec2};
use serde::{Deserialize, Serialize};
use tr_chart::drawings::Tool;
use tr_congress::Trade;
use tr_congress::db::{Alert, PoliticianSummary};
use tr_data::{OrderBook, Quote, SearchHit, YieldCurves};

use crate::anim;
use crate::chart_view::ChartState;
use crate::command::{self, Command, Function};
use crate::congress_view::{Action, FeedFilter};
use crate::dock::{Node, PaneId};
use crate::layout::Layout;
use crate::layouts::{LayoutDoc, Link, PaneSettings, Store, WindowGeometry};
use crate::markets_view;
use crate::prefs::{self, Prefs};
use crate::theme;
use crate::worker::{Profile, Request, Response, Worker, data_dir};

use chrome::{SIDEBAR_W, STATUS_H, SidebarCmd};
use settings_view::SettingsTab;

pub enum PaneKind {
    Chart(Box<ChartState>),
    Feed {
        filter: FeedFilter,
    },
    Ticker {
        symbol: String,
        trades: Vec<Trade>,
        loading: bool,
    },
    Profile {
        query: String,
        data: Option<Box<Profile>>,
        loading: bool,
    },
    Politicians,
    Alerts,
    Research(Box<research::ResearchPane>),
    Watchlist,
    Help,
    Wei,
    Curve,
    Search {
        query: String,
    },
    News(Box<NewsPane>),
    OrderBook(Box<BookPane>),
    Options(Box<OptionsPane>),
    /// Brokerage accounts through SnapTrade (`PORT`, `ACCT`, `POS`, `ORD`, `ACTV`).
    Portfolio(Box<broker::PortfolioPane>),
    /// Watchlist-scoped PM Desk (`DESK` / `PM`): leaders, group performance, position size.
    Desk(Box<desk::DeskPane>),
    /// Trade journal / checklist (`JOURNAL`).
    Journal(Box<journal::JournalPane>),
    /// Historical risk/return and conditional forward returns (`STATS` / `COND`).
    Stats(Box<stats::StatsPane>),
    /// Strategy vs buy-and-hold paper backtest (`BACKTEST` / `BT`).
    Backtest(Box<backtest::BacktestPane>),
    Settings,
}

pub struct OptionsPane {
    pub symbol: String,
    /// Selected expiration (`None` = nearest).
    pub expiration: Option<i64>,
    pub chain: Option<Box<tr_data::OptionChain>>,
    /// Greeks per call and put, aligned with the chain's contracts.
    pub call_greeks: Vec<Option<tr_core::options::Greeks>>,
    pub put_greeks: Vec<Option<tr_core::options::Greeks>>,
    /// Risk-free rate used for the greeks.
    pub rate: f64,
    pub error: Option<String>,
    pub loading: bool,
    pub fetched: Option<Instant>,
    /// Strikes each side of the money (0 = all).
    pub strikes: usize,
    pub show_greeks: bool,
}

impl OptionsPane {
    pub fn new(symbol: &str, strikes: usize, show_greeks: bool) -> Self {
        Self {
            symbol: symbol.into(),
            expiration: None,
            chain: None,
            call_greeks: Vec::new(),
            put_greeks: Vec::new(),
            rate: 0.04,
            error: None,
            loading: true,
            fetched: None,
            strikes,
            show_greeks,
        }
    }

    /// Store a chain and derive greeks from each contract's implied volatility.
    pub fn set_chain(&mut self, chain: Box<tr_data::OptionChain>, rate: f64, now: i64) {
        // Expiry at the 16:00 New York close (≈ 20:00 UTC) of the expiration date.
        let t = ((chain.expiration + 20 * 3600 - now) as f64 / (365.0 * 86_400.0)).max(0.5 / 365.0);
        let g = |call: bool, c: &tr_data::OptionContract| {
            tr_core::options::black_scholes(call, chain.spot, c.strike, t, rate, chain.dividend_yield, c.iv?)
                .map(|(_, g)| g)
        };
        self.call_greeks = chain.calls.iter().map(|c| g(true, c)).collect();
        self.put_greeks = chain.puts.iter().map(|c| g(false, c)).collect();
        self.rate = rate;
        if self.expiration.is_none() || !chain.expirations.contains(&chain.expiration) {
            self.expiration = Some(chain.expiration);
        }
        self.chain = Some(chain);
        self.error = None;
        self.loading = false;
    }
}

pub struct NewsPane {
    pub scope: tr_data::news::NewsScope,
    /// Set when [`NewsScope::Symbol`] (and kept when switching away so Symbol can restore).
    pub symbol: Option<String>,
    pub batch: Option<tr_data::news::NewsBatch>,
    pub loading: bool,
    pub fetched: Option<Instant>,
}

impl NewsPane {
    pub fn new(symbol: Option<String>) -> Self {
        match symbol {
            Some(s) => Self::for_symbol(s),
            None => Self::watchlist(),
        }
    }

    pub fn watchlist() -> Self {
        Self::with_scope(tr_data::news::NewsScope::Watchlist, None)
    }

    pub fn leaders() -> Self {
        Self::with_scope(tr_data::news::NewsScope::Leaders, None)
    }

    pub fn top() -> Self {
        Self::with_scope(tr_data::news::NewsScope::Top, None)
    }

    pub fn for_symbol(symbol: impl Into<String>) -> Self {
        Self::with_scope(tr_data::news::NewsScope::Symbol, Some(symbol.into()))
    }

    fn with_scope(scope: tr_data::news::NewsScope, symbol: Option<String>) -> Self {
        Self { scope, symbol, batch: None, loading: true, fetched: None }
    }

    pub fn accept(&mut self, batch: tr_data::news::NewsBatch) {
        self.loading = false;
        if batch.items.is_empty()
            && !batch.failures.is_empty()
            && self.batch.as_ref().is_some_and(|b| !b.items.is_empty())
        {
            if let Some(prev) = &mut self.batch {
                prev.stale = true;
                prev.failures = batch.failures;
            }
            return;
        }
        self.batch = Some(batch);
    }
}

pub struct BookPane {
    pub symbol: String,
    pub depth: usize,
    /// Price bucket; 0 = raw levels.
    pub group: f64,
    pub book: Option<OrderBook>,
    pub error: Option<String>,
    pub fetched: Option<Instant>,
}

impl BookPane {
    pub fn new(symbol: &str, depth: usize, group: f64) -> Self {
        Self { symbol: symbol.into(), depth, group, book: None, error: None, fetched: None }
    }
}

impl PaneKind {
    /// Price history has its own panel identity, so opening research keeps job progress visible.
    fn panel_type(&self) -> (std::mem::Discriminant<Self>, bool) {
        (std::mem::discriminant(self), matches!(self,Self::Research(r) if r.history))
    }

    /// Short tab title.
    fn tab_title(&self) -> String {
        let short = |s: &str| tr_data::security(s).ticker;
        match self {
            PaneKind::Chart(c) => short(&c.symbol),
            PaneKind::Feed { .. } => "Congress".into(),
            PaneKind::Ticker { symbol, .. } => format!("{} Congress", short(symbol)),
            PaneKind::Profile { query, data, .. } => {
                data.as_ref().map_or_else(|| query.clone(), |p| p.politician.name.clone())
            }
            PaneKind::Politicians => "Politicians".into(),
            PaneKind::Alerts => "Alerts".into(),
            PaneKind::Research(r) => {
                if r.history {
                    "Price history".into()
                } else {
                    r.scan.as_ref().map(|s| format!("Scan: {s}")).unwrap_or_else(|| "Archive universe".into())
                }
            }
            PaneKind::Watchlist => "Watchlist".into(),
            PaneKind::Help => "Help".into(),
            PaneKind::Wei => "World Markets".into(),
            PaneKind::Curve => "Yield Curve".into(),
            PaneKind::Search { query } if query.is_empty() => "Search".into(),
            PaneKind::Search { query } => format!("Search: {query}"),
            PaneKind::News(n) => match n.scope {
                tr_data::news::NewsScope::Symbol => {
                    n.symbol.as_deref().map(|s| format!("{} News", short(s))).unwrap_or_else(|| "News".into())
                }
                tr_data::news::NewsScope::Watchlist => "Watchlist News".into(),
                tr_data::news::NewsScope::Leaders => "Leaders News".into(),
                tr_data::news::NewsScope::Top => "Top News".into(),
            },
            PaneKind::OrderBook(b) => format!("{} Depth", short(&b.symbol)),
            PaneKind::Options(o) => format!("{} Options", short(&o.symbol)),
            PaneKind::Portfolio(p) => p.title(),
            PaneKind::Desk(d) => {
                if d.group.is_empty() {
                    "PM Desk".into()
                } else {
                    format!("Desk: {}", d.group)
                }
            }
            PaneKind::Journal(j) => {
                if j.filter.is_empty() {
                    "Journal".into()
                } else {
                    format!("{} Journal", tr_data::security(&j.filter).ticker)
                }
            }
            PaneKind::Stats(s) => {
                if s.symbol.trim().is_empty() {
                    "Stats".into()
                } else {
                    format!("{} Stats", tr_data::security(&s.symbol).ticker)
                }
            }
            PaneKind::Backtest(b) => {
                if b.symbol.trim().is_empty() {
                    "Backtest".into()
                } else {
                    format!("{} Backtest", tr_data::security(&b.symbol).ticker)
                }
            }
            PaneKind::Settings => "Settings".into(),
        }
    }

    /// The Bloomberg-style function code shown on the tab.
    fn code(&self) -> &'static str {
        match self {
            PaneKind::Chart(_) => "GP",
            PaneKind::Feed { .. } | PaneKind::Ticker { .. } => "CONG",
            PaneKind::Profile { .. } | PaneKind::Politicians => "POL",
            PaneKind::Alerts => "ALRT",
            PaneKind::Research(r) => {
                if r.history {
                    "HIST"
                } else if r.scan.is_some() {
                    "SCAN"
                } else {
                    "UNIV"
                }
            }
            PaneKind::Watchlist => "WL",
            PaneKind::Help => "HELP",
            PaneKind::Wei => "WEI",
            PaneKind::Curve => "GC",
            PaneKind::Search { .. } => "SECF",
            PaneKind::News(_) => "CN",
            PaneKind::OrderBook(_) => "OB",
            PaneKind::Options(_) => "OMON",
            PaneKind::Portfolio(p) => p.view.code(),
            PaneKind::Desk(_) => "DESK",
            PaneKind::Journal(_) => "JRNL",
            PaneKind::Stats(_) => "STATS",
            PaneKind::Backtest(_) => "BT",
            PaneKind::Settings => "PDF",
        }
    }

    /// The security this pane follows, for symbol-driven panels.
    fn symbol(&self) -> Option<&str> {
        match self {
            PaneKind::Chart(c) => Some(&c.symbol),
            PaneKind::Ticker { symbol, .. } => Some(symbol),
            PaneKind::News(n) if n.scope == tr_data::news::NewsScope::Symbol => n.symbol.as_deref(),
            PaneKind::News(_) => None,
            PaneKind::OrderBook(b) => Some(&b.symbol),
            PaneKind::Options(o) => Some(&o.symbol),
            _ => None,
        }
    }

    fn follows_symbol(&self) -> bool {
        matches!(self, PaneKind::Chart(_) | PaneKind::Ticker { .. } | PaneKind::OrderBook(_) | PaneKind::Options(_))
            || matches!(self, PaneKind::News(n) if n.scope == tr_data::news::NewsScope::Symbol)
    }

    fn settings(&self) -> PaneSettings {
        match self {
            PaneKind::Chart(c) => PaneSettings::Chart {
                archive: c.archive,
                watch_group: c.watch_group.clone(),
                symbol: c.symbol.clone(),
                tf: c.tf,
                chart_type: c.chart_type,
                log: c.log,
                indicators: c.indicators.clone(),
                drawings: c.drawings.clone(),
                compare: c.compare.clone(),
                show_volume: c.show_volume,
                show_congress: c.show_congress,
                show_news: c.show_news,
                show_journal: c.show_journal,
                magnet: c.magnet,
                show_box: c.show_box,
                show_rs: c.show_rs,
                show_volume_ctx: c.show_volume_ctx,
            },
            PaneKind::Feed { filter } => {
                PaneSettings::Feed((*filter != FeedFilter::default()).then(|| (*filter).into()))
            }
            PaneKind::Ticker { symbol, .. } => PaneSettings::Ticker(symbol.clone()),
            PaneKind::Profile { query, .. } => PaneSettings::Profile(query.clone()),
            PaneKind::Politicians => PaneSettings::Politicians,
            PaneKind::Alerts => PaneSettings::Alerts,
            PaneKind::Research(r) => {
                if r.history {
                    PaneSettings::PriceHistory
                } else {
                    PaneSettings::Research(r.scan.clone())
                }
            }
            PaneKind::Watchlist => PaneSettings::Watchlist,
            PaneKind::Help => PaneSettings::Help,
            PaneKind::Wei => PaneSettings::Wei,
            PaneKind::Curve => PaneSettings::Curve,
            PaneKind::Search { query } => PaneSettings::Search(query.clone()),
            PaneKind::News(n) => PaneSettings::News(crate::layouts::NewsSettings::from_pane(n)),
            PaneKind::OrderBook(b) => {
                PaneSettings::OrderBook { symbol: b.symbol.clone(), depth: b.depth, group: b.group }
            }
            PaneKind::Settings => PaneSettings::Settings,
            PaneKind::Options(o) => {
                PaneSettings::Options { symbol: o.symbol.clone(), strikes: o.strikes, greeks: o.show_greeks }
            }
            PaneKind::Portfolio(p) => PaneSettings::Portfolio {
                view: p.view,
                account: p.account.clone(),
                symbol: p.symbol.clone(),
                sort: p.sort,
                open_only: p.open_only,
            },
            PaneKind::Desk(d) => PaneSettings::Desk { group: d.group.clone(), selected: d.selected.clone() },
            PaneKind::Journal(j) => PaneSettings::Journal { filter: j.filter.clone() },
            PaneKind::Stats(s) => PaneSettings::Stats {
                symbol: s.symbol.clone(),
                condition: s.condition.clone(),
                conditional: s.tab == stats::StatsTab::Conditional,
            },
            PaneKind::Backtest(b) => PaneSettings::Backtest {
                symbol: b.symbol.clone(),
                spec: b.strategies.clone(),
                holdout: b.holdout.clone(),
                commission: b.commission.clone(),
                slippage: b.slippage.clone(),
                shorts: b.allow_short,
            },
        }
    }

    fn from_settings(p: PaneSettings) -> PaneKind {
        match p {
            PaneSettings::Chart {
                archive,
                watch_group,
                symbol,
                tf,
                chart_type,
                log,
                indicators,
                drawings,
                compare,
                show_volume,
                show_congress,
                show_news,
                show_journal,
                magnet,
                show_box,
                show_rs,
                show_volume_ctx,
            } => {
                let mut c = ChartState::new(0, &symbol, tf);
                c.archive = archive;
                c.watch_group = watch_group;
                c.chart_type = chart_type;
                c.log = log;
                c.indicators = indicators;
                c.drawings = drawings;
                c.compare = compare;
                c.show_volume = show_volume;
                c.show_congress = show_congress;
                c.show_news = show_news;
                c.show_journal = show_journal;
                c.magnet = magnet;
                c.show_box = show_box;
                c.show_rs = show_rs;
                c.show_volume_ctx = show_volume_ctx;
                PaneKind::Chart(Box::new(c))
            }
            PaneSettings::Feed(f) => PaneKind::Feed { filter: f.map(Into::into).unwrap_or_default() },
            PaneSettings::Ticker(s) => PaneKind::Ticker { symbol: s, trades: Vec::new(), loading: true },
            PaneSettings::Profile(q) => PaneKind::Profile { query: q, data: None, loading: true },
            PaneSettings::Politicians => PaneKind::Politicians,
            PaneSettings::Alerts => PaneKind::Alerts,
            PaneSettings::PriceHistory => PaneKind::Research(Box::new(research::ResearchPane::price_history())),
            PaneSettings::Research(scan) => PaneKind::Research(Box::new(research::ResearchPane::new(scan))),
            PaneSettings::Watchlist => PaneKind::Watchlist,
            PaneSettings::Help => PaneKind::Help,
            PaneSettings::Wei => PaneKind::Wei,
            PaneSettings::Curve => PaneKind::Curve,
            PaneSettings::Search(q) => PaneKind::Search { query: q },
            PaneSettings::News(s) => PaneKind::News(Box::new(s.into_pane())),
            PaneSettings::OrderBook { symbol, depth, group } => {
                PaneKind::OrderBook(Box::new(BookPane::new(&symbol, depth, group)))
            }
            PaneSettings::Settings => PaneKind::Settings,
            PaneSettings::Options { symbol, strikes, greeks } => {
                PaneKind::Options(Box::new(OptionsPane::new(&symbol, strikes, greeks)))
            }
            PaneSettings::Portfolio { view, account, symbol, sort, open_only } => {
                PaneKind::Portfolio(Box::new(broker::PortfolioPane { view, account, symbol, sort, open_only }))
            }
            PaneSettings::Desk { group, selected } => {
                let mut d = desk::DeskPane::new(group);
                d.selected = selected;
                PaneKind::Desk(Box::new(d))
            }
            PaneSettings::Journal { filter } => PaneKind::Journal(Box::new(journal::JournalPane::new(filter))),
            PaneSettings::Stats { symbol, condition, conditional } => {
                PaneKind::Stats(Box::new(stats::StatsPane::new(symbol, condition, conditional)))
            }
            PaneSettings::Backtest { symbol, spec, holdout, commission, slippage, shorts } => {
                let mut pane = backtest::BacktestPane::new(symbol, spec);
                pane.holdout = holdout;
                pane.commission = commission;
                pane.slippage = slippage;
                pane.allow_short = shorts;
                PaneKind::Backtest(Box::new(pane))
            }
        }
    }
}

pub struct Pane {
    pub id: PaneId,
    pub kind: PaneKind,
    pub link: Option<Link>,
    /// User-chosen tab title.
    pub title: Option<String>,
    /// Set while popped out into its own window.
    pub window: Option<WindowGeometry>,
}

impl Pane {
    fn display_title(&self) -> String {
        self.title.clone().unwrap_or_else(|| self.kind.tab_title())
    }
}

/// App-wide state persisted in `settings.json` (layouts live in their own files).
#[derive(Clone, Serialize, Deserialize)]
struct Settings {
    #[serde(default)]
    watchlist: Vec<String>,
    #[serde(default)]
    watchlists: Option<watchlists::Watchlists>,
    #[serde(default = "default_true")]
    sidebar: bool,
    /// Name of the layout to restore on launch.
    #[serde(default)]
    active_layout: Option<String>,
    #[serde(default)]
    recent_symbols: Vec<String>,
    // Pre-docking settings files stored a preset and a pane list here.
    #[serde(default, skip_serializing)]
    layout: Option<Layout>,
    #[serde(default, skip_serializing)]
    panes: Option<Vec<serde_json::Value>>,
}

fn default_true() -> bool {
    true
}

const DEFAULT_WATCHLIST: [&str; 11] = [
    "SPX Index",
    "AAPL US Equity",
    "NVDA US Equity",
    "SPY US Equity",
    "BTCUSD Curncy",
    "ETHUSD Curncy",
    "EURUSD Curncy",
    "USDJPY Curncy",
    "US10Y Govt",
    "CL1 Comdty",
    "GC1 Comdty",
];

/// Convert a pre-docking settings file into a layout document.
fn migrate_legacy(s: &Settings, defaults: &prefs::PanelDefaults) -> Option<LayoutDoc> {
    let panes: Vec<PaneSettings> = s
        .panes
        .as_ref()?
        .iter()
        .filter_map(|v| {
            // Unit variants were stored as bare strings ("Feed"); wrap them as {"Feed": null}.
            let v = match v {
                serde_json::Value::String(name) => serde_json::json!({ name.as_str(): null }),
                other => other.clone(),
            };
            serde_json::from_value(v).ok()
        })
        .collect();
    if panes.is_empty() {
        return None;
    }
    let _ = defaults;
    Some(LayoutDoc::preset("Default", s.layout.unwrap_or(Layout::Grid2x2), panes))
}

/// How long an alert toast stays up.
const TOAST_SECS: u64 = 10;

/// A pane drag from its tab or header: pending until the pointer moves past a small
/// threshold (so clicks still select tabs), then active until release.
#[derive(Debug, Clone, Copy)]
pub struct Drag {
    pub pane: PaneId,
    pub origin: egui::Pos2,
    pub active: bool,
}

pub struct TerminalApp {
    worker: Worker,
    provider: String,
    panes: Vec<Pane>,
    dock: Node,
    focus: PaneId,
    next_id: PaneId,
    last_opened: Option<PaneId>,
    /// Preset last applied, for the toolbar picker (cleared by manual rearranging).
    preset: Option<Layout>,
    drag: Option<Drag>,
    dock_motion: workspace::DockMotion,
    /// Tab being renamed inline: pane and edit buffer.
    renaming_tab: Option<(PaneId, String)>,

    store: Store,
    layout_name: String,
    layout_names: Vec<String>,
    last_saved: Vec<u8>,
    /// Layout being renamed in the sidebar: old name and edit buffer.
    renaming_layout: Option<(String, String)>,

    prefs: Prefs,
    /// Built-in and custom themes, and the theme last applied to egui.
    themes: Vec<crate::themes::Theme>,
    theme_store: crate::themes::ThemeStore,
    applied_theme: Option<crate::themes::Theme>,
    /// Theme selected in the editor.
    editing_theme: String,
    /// Custom themes edited but not yet written (saved once editing pauses).
    dirty_themes: Vec<String>,
    themes_changed_at: Option<Instant>,
    renaming_theme: Option<(String, String)>,
    theme_io: String,
    /// The OS appearance, for System theme resolution in commands.
    system_theme: Option<egui::Theme>,
    /// The active layout carries its theme.
    layout_theme: bool,
    rebinding: Option<prefs::Action>,
    hotkey_filter: String,
    settings_tab: SettingsTab,
    whats_news_epoch: u64,
    whats_news_health: tr_data::whats_news::Health,
    io_path: String,

    curves: Option<Box<YieldCurves>>,
    last_wei: Option<Instant>,
    last_curve: Option<Instant>,
    wei_updated: Option<String>,
    /// Latest network search results and the query they answer.
    net_hits: (String, Vec<SearchHit>),
    /// Query typed in the command line awaiting a debounced network search.
    search_pending: Option<(String, Instant)>,
    suggestion: Option<usize>,
    command_catalogue: command_bar::Catalogue,
    recent_symbols: Vec<String>,

    cmd: String,
    cmd_history: Vec<String>,
    cmd_hist_pos: Option<usize>,
    focus_cmd: bool,

    status: String,
    toasts: Vec<(String, Instant)>,

    latest: Vec<Trade>,
    politicians: Vec<PoliticianSummary>,
    pol_search: String,
    alerts: Vec<Alert>,
    price_alerts: tr_data::price_alerts::AlertBatch,
    alert_draft: alerts::AlertDraft,
    last_price_alerts: Option<Instant>,
    journal: tr_data::journal::Store,
    congress_status: String,
    /// When the last Congress ingest finished.
    congress_checked: Option<chrono::DateTime<chrono::Local>>,

    watchlist: Vec<String>,
    watchlists: watchlists::Watchlists,
    watch_ui: watchlists::UiState,
    settings_writer: Option<settings_writer::Writer>,
    quotes: HashMap<String, Quote>,
    last_quotes: Option<Instant>,
    last_feed: Option<Instant>,
    last_save: Instant,
    started: Instant,
    smoke_done: bool,
    /// Frame-time readout (F12): smoothed CPU time of `ui()` and interval between frames.
    show_perf: bool,
    perf_cpu_ms: f64,
    perf_dt_ms: f64,
    last_frame: Option<Instant>,

    sidebar_open: bool,
    /// System accent colour (macOS) or the default blue.
    system_accent: Color32,
    last_accent_poll: Option<Instant>,
    window_styled: bool,
    updater_started: bool,
    /// The window is see-through behind the sidebar (macOS vibrancy).
    vibrancy: bool,
    /// Measured macOS title bar geometry (polled; changes with full screen).
    chrome: Option<tr_macos::Chrome>,
    last_chrome_poll: Option<Instant>,
    /// Brokerage integration (SnapTrade credentials and the loaded portfolio).
    broker: broker::BrokerState,
    /// Order ticket and trading dialogs.
    trading: trading::TradingState,
    /// The paper-trading demo account (selected by default).
    demo: demo::DemoState,
    /// Full command palette (separate from the toolbar input).
    palette: crate::palette::Palette,
}

/// Resolve a run of words (`BTCUSD CRNCY AAPL US EQUITY MSFT`) into market keys.
fn keys_from_words(words: &[String]) -> Vec<String> {
    let tokens: Vec<&str> = words.iter().map(String::as_str).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        match tr_core::Security::resolve_tokens(&tokens[i..]) {
            Some((sec, used)) => {
                out.push(sec.key());
                i += used;
            }
            None => i += 1,
        }
    }
    out
}

/// Canonicalise persisted recents, including old malformed FX/crypto keys.
fn canonical_recent_symbols(symbols: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for symbol in symbols {
        if let Some(sec) = tr_core::Security::parse(symbol) {
            let key = sec.key();
            if !out.contains(&key) {
                out.push(key);
            }
        }
        if out.len() == 30 {
            break;
        }
    }
    out
}

fn settings_path() -> std::path::PathBuf {
    data_dir().join("settings.json")
}

fn prefs_path() -> std::path::PathBuf {
    data_dir().join("prefs.json")
}

impl TerminalApp {
    pub fn new(ctx: &egui::Context, worker: Worker, provider: String) -> Self {
        let system_accent =
            tr_macos::accent_color().map_or(theme::DEFAULT_ACCENT, |[r, g, b]| Color32::from_rgb(r, g, b));
        let mut prefs = Prefs::load(&prefs_path());
        let theme_store = crate::themes::ThemeStore::new(data_dir().join("themes"));
        let mut themes = theme_store.load_all();
        // Appearance overrides from older versions become a custom theme file.
        if let Some(mut t) = prefs.migrated_theme.take() {
            t.name = crate::themes::unique_name(&themes, &t.name);
            prefs.appearance.theme = t.name.clone();
            match theme_store.save(&t) {
                Ok(path) => t.source = Some(path),
                Err(e) => tracing::warn!("saving migrated theme: {e:#}"),
            }
            themes.push(t);
            let _ = prefs.save(&prefs_path());
        }
        if let Ok(v) = std::env::var("TR_THEME") {
            prefs.appearance.theme = match v.to_ascii_lowercase().as_str() {
                "light" => prefs.appearance.light_theme.clone(),
                "dark" => prefs.appearance.dark_theme.clone(),
                _ => crate::themes::find(&themes, &v).map_or(prefs.appearance.theme.clone(), |t| t.name.clone()),
            };
        }
        let settings: Option<Settings> =
            std::fs::read(settings_path()).ok().and_then(|b| serde_json::from_slice(&b).ok());
        let broker = broker::BrokerState::load(provider.eq_ignore_ascii_case("sample"));
        let (demo, demo_note) = demo::DemoState::load(demo::demo_path());
        let mut app = Self {
            broker,
            trading: Default::default(),
            demo,
            worker,
            provider,
            panes: Vec::new(),
            dock: Node::default(),
            focus: 0,
            next_id: 1,
            last_opened: None,
            preset: None,
            drag: None,
            dock_motion: Default::default(),
            renaming_tab: None,
            store: Store::new(data_dir().join("layouts")),
            layout_name: "Default".into(),
            layout_names: Vec::new(),
            last_saved: Vec::new(),
            renaming_layout: None,
            editing_theme: prefs.appearance.theme.clone(),
            themes,
            theme_store,
            applied_theme: None,
            dirty_themes: Vec::new(),
            themes_changed_at: None,
            renaming_theme: None,
            theme_io: String::new(),
            system_theme: ctx.system_theme(),
            layout_theme: false,
            prefs,
            rebinding: None,
            hotkey_filter: String::new(),
            settings_tab: SettingsTab::Appearance,
            whats_news_epoch: 0,
            whats_news_health: tr_data::whats_news::Health::pending(),
            io_path: String::new(),
            curves: None,
            last_wei: None,
            last_curve: None,
            wei_updated: None,
            net_hits: (String::new(), Vec::new()),
            search_pending: None,
            suggestion: None,
            command_catalogue: Default::default(),
            recent_symbols: settings.as_ref().map(|s| canonical_recent_symbols(&s.recent_symbols)).unwrap_or_default(),
            cmd: String::new(),
            cmd_history: Vec::new(),
            cmd_hist_pos: None,
            focus_cmd: true,
            status: "Ready. Type a ticker and press <GO> (Enter). F1 for help.".into(),
            toasts: Vec::new(),
            latest: Vec::new(),
            politicians: Vec::new(),
            pol_search: String::new(),
            alerts: Vec::new(),
            price_alerts: tr_data::price_alerts::AlertBatch::empty(0),
            alert_draft: alerts::AlertDraft::default(),
            last_price_alerts: None,
            journal: tr_data::journal::Store::load(&tr_data::journal::store_path(data_dir())),
            congress_status: String::new(),
            congress_checked: None,
            watchlist: DEFAULT_WATCHLIST.map(String::from).to_vec(),
            watchlists: watchlists::Watchlists::migrate(&DEFAULT_WATCHLIST.map(String::from)),
            watch_ui: Default::default(),
            settings_writer: None,
            quotes: HashMap::new(),
            last_quotes: None,
            last_feed: None,
            last_save: Instant::now(),
            started: Instant::now(),
            smoke_done: false,
            show_perf: std::env::var_os("TR_FPS").is_some(),
            perf_cpu_ms: 0.0,
            perf_dt_ms: 0.0,
            last_frame: None,
            sidebar_open: true,
            system_accent,
            last_accent_poll: None,
            window_styled: false,
            updater_started: false,
            vibrancy: false,
            chrome: None,
            last_chrome_poll: None,
            palette: crate::palette::Palette::load(&data_dir().join("palette.json")),
        };
        let mut doc = None;
        if let Some(s) = &settings {
            app.watchlists = s.watchlists.clone().unwrap_or_else(|| watchlists::Watchlists::migrate(&s.watchlist));
            app.watchlist = app.watchlists.symbols();
            app.sidebar_open = s.sidebar;
            if let Some(name) = &s.active_layout {
                match app.store.load(name) {
                    Ok(d) => doc = Some(d),
                    Err(e) => tracing::warn!("restoring layout {name:?}: {e:#}"),
                }
            }
            if doc.is_none() {
                doc = migrate_legacy(s, &app.prefs.panels);
            }
        }
        let doc = doc
            .or_else(|| app.store.list().first().and_then(|n| app.store.load(n).ok()))
            .unwrap_or_else(|| LayoutDoc::default_workspace("Default", &app.prefs.panels));
        app.worker
            .send(Request::ConfigureWhatsNews { epoch: app.whats_news_epoch, config: app.prefs.whats_news.clone() });
        app.apply_doc(doc);
        if !app.store.exists(&app.layout_name) {
            app.save_layout();
        }
        app.refresh_layout_names();
        app.worker.send(Request::Latest { limit: 2_000 });
        app.worker.send(Request::SearchPoliticians(String::new()));
        app.worker.send(Request::Alerts);
        if let Some(note) = demo_note {
            app.trade_toast(note, false);
        }
        app
    }

    fn save_settings(&mut self) {
        let s = Settings {
            watchlist: self.watchlist.clone(),
            watchlists: Some(self.watchlists.clone()),
            recent_symbols: self.recent_symbols.clone(),
            sidebar: self.sidebar_open,
            active_layout: Some(self.layout_name.clone()),
            layout: None,
            panes: None,
        };
        match serde_json::to_vec_pretty(&s) {
            Ok(json) => {
                let path = self.store.dir.parent().expect("layouts parent").join("settings.json");
                self.settings_writer.get_or_insert_with(|| settings_writer::Writer::new(path)).save(json);
            }
            Err(e) => self.status = format!("Could not serialize settings: {e}"),
        }
    }

    fn save_prefs(&mut self) {
        if let Err(e) = self.prefs.save(&prefs_path()) {
            self.status = format!("Could not save preferences: {e}");
        }
    }

    fn run_actions(&mut self, actions: Vec<Action>) {
        for a in actions {
            match a {
                Action::OpenArchiveChart(s) => self.open_list_symbol(&s, true, None),
                Action::WatchChart(group, symbol, archive) => self.open_list_symbol(&symbol, archive, Some(group)),
                Action::OpenChart(s) => self.open_security(&s),
                Action::OpenTicker(s) => {
                    self.open_panel(PaneKind::Ticker { symbol: s, trades: Vec::new(), loading: true })
                }
                Action::OpenProfile(q) => self.open_panel(PaneKind::Profile { query: q, data: None, loading: true }),
                Action::AddToWatchlist(s) => {
                    self.watch_ui.picker = Some(self.member_for_save(&s));
                    self.watch_ui.picker_index = self.watchlists.index(&self.watchlists.last_used).unwrap_or(0);
                }
                Action::LinkedSymbol(link, s) => self.broadcast(link, &s, None),
                Action::Command(line) => match command::parse(&line) {
                    Some(cmd) => self.dispatch(cmd),
                    None => tracing::warn!("internal command did not parse: {line}"),
                },
                Action::Track(q, on) => {
                    self.worker.send(Request::Track { query: q.clone(), on });
                    // Refresh any open profile so the star updates.
                    for p in &self.panes {
                        if let PaneKind::Profile { query, .. } = &p.kind {
                            self.worker.send(Request::Profile { pane: p.id, query: query.clone() });
                        }
                    }
                }
            }
        }
    }

    fn execute(&mut self, input: &str) {
        if input.len() > command::MAX_INPUT_BYTES {
            self.status = "Command too long (maximum 4096 bytes)".into();
            return;
        }
        let Some(cmd) = command::parse(input) else {
            self.status = format!("Unknown command: {input}  (HELP lists functions)");
            return;
        };
        self.cmd_history.push(input.trim().to_string());
        self.dispatch(cmd);
    }

    fn dispatch(&mut self, cmd: Command) {
        match cmd {
            Command::Symbol(s) => self.open_security(&s),
            Command::SymbolFunction(s, Function::Congress) => {
                self.open_panel(PaneKind::Ticker { symbol: s.clone(), trades: Vec::new(), loading: true });
                self.status = format!("{s}: congressional trades");
            }
            Command::SymbolFunction(s, Function::News) => {
                self.open_panel(PaneKind::News(Box::new(NewsPane::for_symbol(s.clone()))));
                self.status = format!("{s}: news");
            }
            Command::SymbolFunction(s, Function::OrderBook(depth)) => {
                let depth = depth.unwrap_or(self.prefs.panels.book_depth).clamp(5, 200);
                self.open_panel(PaneKind::OrderBook(Box::new(BookPane::new(&s, depth, 0.0))));
                self.status = format!("{s}: order book");
            }
            Command::SymbolFunction(s, Function::Options) => {
                self.open_panel(PaneKind::Options(Box::new(OptionsPane::new(&s, 12, true))));
                self.status = format!("{s}: option chain");
            }
            Command::SymbolFunction(s, Function::Portfolio(view, arg)) => self.portfolio_command(view, &arg, Some(s)),
            Command::SymbolFunction(s, Function::Trade(mut t)) => {
                t.symbol = Some(s);
                self.open_ticket(t, None);
            }
            Command::SymbolFunction(s, f @ (Function::NewPanel(_) | Function::Split { .. })) => {
                self.apply_with_symbol(f, Some(s));
            }
            Command::SymbolFunction(s, Function::Review) => self.open_review(Some(&s)),
            Command::SymbolFunction(s, Function::Desk) => self.open_desk(Some(&s)),
            Command::SymbolFunction(s, Function::Journal(extra)) => self.open_journal(Some(&s), &extra),
            Command::SymbolFunction(s, Function::Stats { condition, conditional }) => {
                self.open_stats(Some(&s), &condition, conditional)
            }
            Command::SymbolFunction(s, Function::Backtest { spec }) => self.open_backtest(Some(&s), &spec),
            Command::SymbolFunction(s, Function::JournalNote { date, text }) => {
                self.add_journal_note(Some(&s), &date, &text)
            }
            Command::SymbolFunction(s, Function::SetPriceAlert(kind)) => self.set_price_alert(Some(s), kind),
            Command::SymbolFunction(s, Function::ClearPriceAlert(_)) => self.clear_price_alert(&s),
            Command::SymbolFunction(s, f) => {
                self.open_security(&s);
                self.apply(f);
            }
            Command::Function(f) => self.apply(f),
        }
    }

    fn apply(&mut self, f: Function) {
        self.apply_with_symbol(f, None);
    }

    fn apply_with_symbol(&mut self, f: Function, symbol: Option<String>) {
        match f {
            Function::Timeframe(tf) => self.with_chart(|c, w| c.set_timeframe(tf, w)),
            Function::ChartType(t) => self.with_chart(|c, _| c.chart_type = t),
            Function::Indicator(spec) => self.with_chart(|c, w| c.toggle_indicator(spec, w)),
            Function::IndicatorPicker(q) => self.palette.open_scoped(crate::palette::Kind::Indicator, &q),
            Function::ClearIndicators => self.with_chart(|c, _| c.clear_indicators()),
            Function::Compare(s) => self.with_chart(|c, w| c.set_compare(s, w)),
            Function::LogScale => self.with_chart(|c, _| c.log = !c.log),
            Function::Range(r) => self.with_chart(|c, w| c.set_range(r, w)),
            Function::Draw(t) => self.with_chart(|c, _| c.start_tool(t)),
            Function::ClearDrawings => self.with_chart(|c, _| c.drawings.clear()),
            Function::Magnet => self.with_chart(|c, _| c.magnet = !c.magnet),
            Function::Congress => self.open_panel(PaneKind::Feed { filter: FeedFilter::default() }),
            Function::Politician(q) if q.trim().is_empty() => self.open_panel(PaneKind::Politicians),
            Function::Politician(q) => self.open_panel(PaneKind::Profile { query: q, data: None, loading: true }),
            Function::Track(q) => self.worker.send(Request::Track { query: q, on: true }),
            Function::Untrack(q) => self.worker.send(Request::Track { query: q, on: false }),
            Function::Alerts => {
                self.worker.send(Request::Alerts);
                self.worker.send(Request::PriceAlerts);
                self.worker.send(Request::EvaluatePriceAlerts);
                self.open_panel(PaneKind::Alerts);
            }
            Function::SetPriceAlert(kind) => self.set_price_alert(symbol, kind),
            Function::ClearPriceAlert(q) => {
                let q = if q.trim().is_empty() {
                    symbol.unwrap_or_else(|| self.focused_symbol().unwrap_or_default())
                } else {
                    q
                };
                self.clear_price_alert(&q);
            }
            Function::Refresh => self.worker.send(Request::RefreshCongress),
            Function::History(command) => {
                if !command.is_empty() {
                    self.worker.send(Request::HistoryTask(command));
                }
                self.open_panel(PaneKind::Research(Box::new(research::ResearchPane::price_history())));
            }
            Function::Universe => self.open_panel(PaneKind::Research(Box::new(research::ResearchPane::new(None)))),
            Function::Scan(name) => {
                self.open_panel(PaneKind::Research(Box::new(research::ResearchPane::new(Some(name)))))
            }
            Function::Watchlist => self.open_panel(PaneKind::Watchlist),
            Function::WatchCommand(line) => self.watch_command(&line),
            Function::Layout(arg) => self.layout_command(&arg),
            Function::Layouts => {
                self.settings_tab = SettingsTab::Layouts;
                self.open_panel(PaneKind::Settings);
                self.status = format!("Layouts: {}", self.layout_names.join(" · "));
            }
            Function::NewPanel(spec) => {
                let kind = self.panel_from_spec(&spec, symbol.as_deref());
                match kind {
                    Some(kind) => {
                        let target = self.focused_visible();
                        let id = self.new_pane(kind);
                        self.dock_pane(id, target, crate::dock::Zone::Right);
                    }
                    None => {
                        self.status = format!(
                            "Unknown panel {spec:?}; try CHART, NEWS, BOOK, WEI, GC, CONG, WL, DESK, JOURNAL, STATS, BACKTEST, PORT"
                        )
                    }
                }
            }
            Function::Split { down, panel } => match self.panel_from_spec(&panel, symbol.as_deref()) {
                Some(kind) => {
                    let target = self.focused_visible();
                    let id = self.new_pane(kind);
                    let zone = if down { crate::dock::Zone::Bottom } else { crate::dock::Zone::Right };
                    self.dock_pane(id, target, zone);
                }
                None => self.status = format!("Unknown panel {panel:?}"),
            },
            Function::ClosePane => self.close_pane(self.focus),
            Function::PopOut => self.pop_out(self.focus),
            Function::Link(link) => {
                let focus = self.focus;
                self.set_link(focus, link);
            }
            Function::News => self.open_panel(PaneKind::News(Box::new(NewsPane::new(None)))),
            Function::TopNews => self.open_panel(PaneKind::News(Box::new(NewsPane::top()))),
            Function::OrderBook(depth) => {
                let sym = self.focused_symbol().unwrap_or_else(|| "BTCUSD Curncy".into());
                let depth = depth.unwrap_or(self.prefs.panels.book_depth).clamp(5, 200);
                self.open_panel(PaneKind::OrderBook(Box::new(BookPane::new(&sym, depth, 0.0))));
            }
            Function::Settings => self.open_panel(PaneKind::Settings),
            Function::Update => {
                if !tr_macos::updater::check_for_updates() {
                    let url = format!("{}/latest", tr_macos::updater::RELEASES);
                    self.status = match broker::open_external(&url) {
                        Ok(()) => "Opened the latest release on GitHub".into(),
                        Err(e) => format!("Could not open {url}: {e}"),
                    };
                }
            }
            Function::Options => {
                let sym = self.optionable_symbol();
                self.open_panel(PaneKind::Options(Box::new(OptionsPane::new(&sym, 12, true))));
            }
            Function::Sidebar(arg) => {
                let side = &mut self.prefs.appearance.sidebar_side;
                match arg.as_str() {
                    "LEFT" => *side = prefs::SidebarSide::Left,
                    "RIGHT" => *side = prefs::SidebarSide::Right,
                    "SWAP" | "FLIP" | "OTHER" => *side = side.other(),
                    "SHOW" | "ON" => self.sidebar_open = true,
                    "HIDE" | "OFF" => self.sidebar_open = false,
                    _ => self.sidebar_open = !self.sidebar_open,
                }
                self.save_prefs();
            }
            Function::Theme(t) => {
                let system = self.system_theme;
                self.theme_command(&t, system);
            }
            Function::Title(t) => {
                let focus = self.focus;
                if let Some(p) = self.pane_mut(focus) {
                    p.title = (!t.trim().is_empty()).then(|| t.trim().to_string());
                }
            }
            Function::Help => self.open_panel(PaneKind::Help),
            Function::Wei => {
                self.last_wei = None;
                self.open_panel(PaneKind::Wei);
            }
            Function::Curve => {
                self.worker.send(Request::YieldCurves);
                self.last_curve = Some(Instant::now());
                self.open_panel(PaneKind::Curve);
            }
            Function::Search(q) => {
                if !q.trim().is_empty() {
                    self.worker.send(Request::Search(q.clone()));
                }
                self.open_panel(PaneKind::Search { query: q });
            }
            Function::Portfolio(view, arg) => self.portfolio_command(view, &arg, None),
            Function::Trade(t) => self.open_ticket(t, None),
            Function::AccountMode(live) => {
                let mode = if live { prefs::AccountMode::Live } else { prefs::AccountMode::Demo };
                if mode == self.prefs.trading.account_mode {
                    self.status = format!("Already using the {} account", if live { "LIVE" } else { "DEMO" });
                }
                self.set_account_mode(mode, false);
            }
            Function::Review => self.open_review(symbol.as_deref()),
            Function::Desk => self.open_desk(symbol.as_deref()),
            Function::Journal(extra) => self.open_journal(symbol.as_deref(), &extra),
            Function::Stats { condition, conditional } => self.open_stats(symbol.as_deref(), &condition, conditional),
            Function::Backtest { spec } => self.open_backtest(symbol.as_deref(), &spec),
            Function::JournalNote { date, text } => self.add_journal_note(symbol.as_deref(), &date, &text),
            Function::Latest => self.with_chart(|c, _| c.jump_latest()),
            Function::ReviewOverlay(which) => self.with_chart(|c, _| match which {
                command::ReviewOverlay::BoxLevels => c.show_box = !c.show_box,
                command::ReviewOverlay::Strength => c.show_rs = !c.show_rs,
                command::ReviewOverlay::Volume => c.show_volume_ctx = !c.show_volume_ctx,
            }),
            Function::Export(kind, path) => self.run_export(kind, &path),
            Function::Backup(path) => self.backup_workspace(&path),
            Function::Restore(path) => self.restore_portable(&path),
        }
    }

    // ---- worker responses ----------------------------------------------------------------

    fn chart_mut(&mut self, pane: PaneId) -> Option<&mut ChartState> {
        match self.panes.iter_mut().find(|p| p.id == pane).map(|p| &mut p.kind) {
            Some(PaneKind::Chart(c)) => Some(c),
            _ => None,
        }
    }

    fn drain(&mut self) {
        while let Ok(r) = self.worker.rx.try_recv() {
            let r = match r {
                Response::WhatsNews { epoch, response } if epoch == self.whats_news_epoch => *response,
                Response::WhatsNews { .. } => continue,
                other => other,
            };
            match r {
                Response::WhatsNews { .. } => unreachable!(),
                Response::WhatsNewsHealth { epoch, health } => {
                    if epoch != self.whats_news_epoch {
                        continue;
                    }
                    let changed = self.whats_news_health.state != health.state
                        || self.whats_news_health.source != health.source
                        || self.whats_news_health.message != health.message;
                    self.whats_news_health = health;
                    if changed {
                        self.refresh_whats_news_panes();
                    }
                }
                Response::HistoryStatus(result) => {
                    for pane in &mut self.panes {
                        if let PaneKind::Research(r) = &mut pane.kind
                            && r.history
                        {
                            r.accept_history(result.clone());
                        }
                    }
                }
                Response::HistoryChanged => {
                    self.command_catalogue = Default::default();
                    let desk: Vec<(crate::dock::PaneId, String)> = self
                        .panes
                        .iter()
                        .filter_map(|p| match &p.kind {
                            PaneKind::Desk(d) => Some((p.id, d.group.clone())),
                            _ => None,
                        })
                        .collect();
                    for pane in &mut self.panes {
                        match &mut pane.kind {
                            PaneKind::Research(r) => r.request(pane.id, &self.worker),
                            PaneKind::Chart(c) if c.archive => c.load(&self.worker),
                            PaneKind::Stats(s) => s.request(pane.id, &self.worker),
                            PaneKind::Backtest(b) => b.request(pane.id, &self.worker),
                            _ => {}
                        }
                    }
                    for (id, group) in desk {
                        let symbols = self
                            .watchlists
                            .group(&group)
                            .map(|g| g.members.iter().map(|m| m.symbol.clone()).collect())
                            .unwrap_or_default();
                        if let Some(PaneKind::Desk(d)) = self.panes.iter_mut().find(|p| p.id == id).map(|p| &mut p.kind)
                        {
                            d.request(id, &self.worker, symbols);
                        }
                    }
                }
                Response::AutocompleteUniverse(result) => self.command_catalogue.accept(result),
                Response::Universe { pane, generation, result } => {
                    if let Some(PaneKind::Research(r)) =
                        self.panes.iter_mut().find(|p| p.id == pane).map(|p| &mut p.kind)
                        && r.generation == generation
                    {
                        r.accept_universe(result);
                    }
                }
                Response::Scan { pane, generation, result } => {
                    if let Some(PaneKind::Research(r)) =
                        self.panes.iter_mut().find(|p| p.id == pane).map(|p| &mut p.kind)
                        && r.generation == generation
                    {
                        r.accept_scan(result);
                    }
                }
                Response::Desk { pane, generation, result } => {
                    if let Some(PaneKind::Desk(d)) = self.panes.iter_mut().find(|p| p.id == pane).map(|p| &mut p.kind) {
                        d.accept(generation, result);
                    }
                }
                Response::Stats { pane, generation, result } => {
                    if let Some(PaneKind::Stats(s)) = self.panes.iter_mut().find(|p| p.id == pane).map(|p| &mut p.kind)
                    {
                        s.accept(generation, result);
                    }
                }
                Response::Backtest { pane, generation, result } => {
                    if let Some(PaneKind::Backtest(b)) =
                        self.panes.iter_mut().find(|p| p.id == pane).map(|p| &mut p.kind)
                    {
                        b.accept(generation, result);
                    }
                }
                Response::History {
                    pane,
                    generation,
                    symbol,
                    tf,
                    bars,
                    gpu,
                    prepended,
                    from_cache,
                    exhausted,
                    live,
                } => {
                    let worker = &self.worker;
                    if let Some(PaneKind::Chart(c)) = self.panes.iter_mut().find(|p| p.id == pane).map(|p| &mut p.kind)
                        && c.symbol == symbol
                        && c.tf == tf
                    {
                        c.on_history(generation, bars, gpu, prepended, from_cache, exhausted, live, worker);
                    }
                }
                Response::HistoryError { pane, generation, error } => {
                    // Errors while paging just mean "no older history"; only report failed loads.
                    let empty =
                        self.chart_mut(pane).map(|c| c.on_history_error(generation, error.clone()) && c.is_empty());
                    if empty == Some(true) {
                        self.status = format!("data error: {error}");
                    }
                }
                Response::Indicators { pane, generation, built } => {
                    if let Some(c) = self.chart_mut(pane) {
                        c.on_indicators(generation, built);
                    }
                }
                Response::Compare { pane, generation, symbol, line } => {
                    if let Some(c) = self.chart_mut(pane) {
                        c.on_compare(generation, &symbol, line);
                    }
                }
                Response::Markers { pane, symbol, trades } => {
                    if let Some(c) = self.chart_mut(pane) {
                        c.on_markers(&symbol, trades);
                    }
                }
                Response::Quotes(q) => {
                    if q.len() > 20 {
                        self.wei_updated = Some(format!("updated {}", chrono::Local::now().format("%H:%M:%S")));
                    }
                    let keys: Vec<String> = q.iter().map(|x| x.symbol.clone()).collect();
                    for x in q {
                        self.quotes.insert(x.symbol.clone(), x);
                    }
                    self.demo_on_quotes(&keys);
                }
                Response::Latest(t) => self.latest = t,
                Response::TickerTrades { pane, symbol, trades } => {
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id == pane)
                        && let PaneKind::Ticker { symbol: s, trades: t, loading } = &mut p.kind
                        && *s == symbol
                    {
                        *t = trades;
                        *loading = false;
                    }
                }
                Response::Profile { pane, profile, query } => {
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id == pane)
                        && let PaneKind::Profile { query: q, data, loading } = &mut p.kind
                        && *q == query
                    {
                        *data = profile;
                        *loading = false;
                    }
                }
                Response::Options { pane, symbol, chain } => {
                    // Greeks use the 3-month T-bill yield when the curve is loaded.
                    let rate = self.curves.as_ref().and_then(|c| c.current.get("US3M")).map_or(0.04, |y| y / 100.0);
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id == pane)
                        && let PaneKind::Options(o) = &mut p.kind
                        && o.symbol == symbol
                    {
                        match chain {
                            Ok(c) => o.set_chain(c, rate, chrono::Utc::now().timestamp()),
                            Err(e) => {
                                o.error = Some(e);
                                o.loading = false;
                            }
                        }
                    }
                }
                Response::News { pane, symbols, batch } => {
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id == pane) {
                        match &mut p.kind {
                            PaneKind::News(n) => n.accept(batch.clone()),
                            PaneKind::Chart(c) => c.on_news(&batch),
                            _ => {}
                        }
                    }
                    for p in &mut self.panes {
                        if p.id == pane {
                            continue;
                        }
                        if let PaneKind::Chart(c) = &mut p.kind {
                            let ours = symbols.as_ref().is_some_and(|s| s.iter().any(|k| k == &c.symbol))
                                || batch.queried.iter().any(|k| k == &c.symbol);
                            if ours {
                                c.on_news(&batch);
                            }
                        }
                    }
                }
                Response::OrderBook { pane, symbol, book } => {
                    if let Some(p) = self.panes.iter_mut().find(|p| p.id == pane)
                        && let PaneKind::OrderBook(b) = &mut p.kind
                        && b.symbol == symbol
                    {
                        match book {
                            Ok(book) => {
                                b.book = Some(book);
                                b.error = None;
                            }
                            Err(e) => b.error = Some(e),
                        }
                    }
                }
                Response::Portfolio(result) => {
                    let result = self.keep_simulated_trades(result);
                    self.on_portfolio(result)
                }
                Response::BrokerRegistered(result) => self.on_broker_registered(result),
                Response::BrokerPortal(result) => self.on_broker_portal(result),
                Response::BrokerDisconnected(result) => self.on_broker_disconnected(result),
                Response::Politicians(list) => self.politicians = list,
                Response::Alerts(a) => self.alerts = a,
                Response::NewAlerts(new) => {
                    for a in new {
                        tracing::info!("ALERT: {}", a.message);
                        self.toasts.push((a.message, Instant::now()));
                    }
                    self.worker.send(Request::Alerts);
                }
                Response::PriceAlertFeed(batch) => {
                    self.price_alerts = batch;
                    self.sync_chart_alerts();
                }
                Response::Portable(result) => {
                    self.status = match result {
                        Ok(s) => s,
                        Err(e) => format!("Export / restore: {e}"),
                    };
                }
                Response::NewPriceAlerts(new) => {
                    for a in new {
                        tracing::info!("ALERT: {}", a.message);
                        self.toasts.push((a.message, Instant::now()));
                    }
                    self.worker.send(Request::PriceAlerts);
                }
                Response::Status(s) => self.status = s,
                Response::Curves(c) => self.curves = Some(c),
                Response::SearchHits { query, hits } => self.net_hits = (query, hits),
                Response::Trade(r) => self.on_trade(*r),
                Response::CongressRefreshed(summary) => {
                    self.congress_status = summary.to_string();
                    self.congress_checked = Some(chrono::Local::now());
                    self.status = format!("Congress disclosures updated: {summary}");
                    self.refresh_congress_views();
                }
            }
        }
    }

    fn set_price_alert(&mut self, symbol: Option<String>, kind: tr_data::price_alerts::AlertKind) {
        let Some(symbol) = symbol.or_else(|| self.focused_symbol()) else {
            self.status = "ALERT needs a ticker (AAPL ALERT 150 or AAPL ALERT SETUP NEAR_HIGH)".into();
            return;
        };
        let symbol = tr_data::security(&symbol).key();
        self.worker.send(Request::AddPriceAlert { symbol, kind });
        self.worker.send(Request::Alerts);
        self.open_panel(PaneKind::Alerts);
    }

    fn clear_price_alert(&mut self, query: &str) {
        if query.trim().is_empty() {
            self.status = "UNALERT needs a ticker or rule".into();
            return;
        }
        self.worker.send(Request::RemovePriceAlert { query: query.to_string() });
        self.open_panel(PaneKind::Alerts);
    }

    fn sync_chart_alerts(&mut self) {
        for pane in &mut self.panes {
            if let PaneKind::Chart(c) = &mut pane.kind {
                c.set_alert_rules(
                    self.price_alerts.rules.iter().filter(|r| r.enabled && r.symbol == c.symbol).cloned().collect(),
                );
            }
        }
    }

    fn unread_alerts(&self) -> usize {
        self.alerts.iter().filter(|a| !a.seen).count() + self.price_alerts.unread()
    }

    /// Freshness line for the Congress feed: last check, newest filing, next check.
    fn congress_freshness(&self) -> String {
        let newest = self.latest.iter().map(|t| t.filed_date).max();
        let minutes = std::env::var("TR_CONGRESS_REFRESH_MIN")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(crate::worker::CONGRESS_REFRESH_MIN as i64);
        let mut parts = Vec::new();
        match self.congress_checked {
            Some(t) => {
                parts.push(format!("checked {}", t.format("%H:%M")));
                if minutes > 0 {
                    parts.push(format!("next {}", (t + chrono::Duration::minutes(minutes)).format("%H:%M")));
                }
            }
            None => parts.push("checking Senate eFD + House Clerk…".into()),
        }
        if let Some(d) = newest {
            parts.push(format!("newest filing {d}"));
        }
        if !self.congress_status.is_empty() {
            parts.push(self.congress_status.clone());
        }
        parts.join(" · ")
    }

    fn refresh_congress_views(&mut self) {
        self.worker.send(Request::Latest { limit: 2_000 });
        self.worker.send(Request::SearchPoliticians(String::new()));
        for p in &self.panes {
            match &p.kind {
                PaneKind::Ticker { symbol, .. } => {
                    self.worker.send(Request::TickerTrades { pane: p.id, symbol: symbol.clone() })
                }
                PaneKind::Chart(c) => self.worker.send(Request::Markers { pane: p.id, symbol: c.symbol.clone() }),
                _ => {}
            }
        }
        self.last_feed = Some(Instant::now());
    }

    // ---- input ---------------------------------------------------------------------------

    /// Global and chart hotkeys. Runs before the command line so it can claim keys; when
    /// the command line holds text, keys belong to it.
    fn hotkeys(&mut self, ctx: &egui::Context) {
        let presses: Vec<(Key, Modifiers)> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    Event::Key { key, pressed: true, repeat: false, modifiers, .. } if !prefs::is_modifier(*key) => {
                        Some((*key, *modifiers))
                    }
                    _ => None,
                })
                .collect()
        });
        // Settings is capturing a new binding: the next key press is it.
        if let Some(action) = self.rebinding {
            if let Some(&(key, m)) = presses.first() {
                ctx.input_mut(|i| i.consume_key(m, key));
                self.rebinding = None;
                if key != Key::Escape {
                    let chord = prefs::Chord::from_event(key, m);
                    let displaced = self.prefs.hotkeys.bind(action, chord);
                    self.status = match displaced {
                        Some(d) => {
                            format!("{} bound to {} (removed from {})", action.label(), chord.display(), d.label())
                        }
                        None => format!("{} bound to {}", action.label(), chord.display()),
                    };
                    self.save_prefs();
                }
            }
            return;
        }
        let focus_chord = presses.iter().find(|(k, m)| *k == Key::K && m.ctrl && !m.alt && !m.shift);
        let focus_text = ctx.input(|i| i.events.iter().any(|e| matches!(e, Event::Text(t) if t == "/" || t == ":")))
            && !ctx.text_edit_focused();
        if focus_chord.is_some() || focus_text {
            ctx.input_mut(|i| {
                if let Some(&(key, m)) = focus_chord {
                    i.consume_key(m, key);
                }
                i.events.retain(|e| !matches!(e, Event::Text(t) if t == "/" || t == ":"));
            });
            self.palette.open = false;
            self.cmd.clear();
            self.command_changed();
            self.focus_cmd = true;
            ctx.memory_mut(|m| m.request_focus(egui::Id::new("command-line")));
            return;
        }
        // The full palette remains available from text fields; Ctrl+K is reserved for the bar.
        if let Some(&(key, m)) = presses.iter().find(|(k, m)| {
            (*k == Key::K && m.shift && (m.command || m.ctrl) && !m.alt)
                || self.prefs.hotkeys.action_for(*k, *m) == Some(prefs::Action::CommandPalette)
        }) {
            ctx.input_mut(|i| i.consume_key(m, key));
            self.palette.toggle();
            return;
        }
        // While it is open, the palette owns the keyboard.
        if self.palette.open
            || self.watch_ui.picker.is_some()
            || self.watch_ui.delete.is_some()
            || self.watch_ui.edit.is_some()
        {
            return;
        }
        let cmd_id = egui::Id::new("command-line");
        // egui processes focus navigation at begin_pass, before app hotkeys. A newly
        // focused TextEdit may not have installed its filter yet; keep its first Tab,
        // arrows and Escape in the command bar instead of moving to another widget.
        let navigation = presses
            .iter()
            .any(|(k, m)| !m.any() && matches!(k, Key::Tab | Key::Escape | Key::ArrowUp | Key::ArrowDown));
        let owns_navigation =
            ctx.memory(|m| m.has_focus(cmd_id) || (m.focused().is_none() && m.had_focus_last_frame(cmd_id)));
        if navigation && owns_navigation {
            ctx.memory_mut(|m| {
                if !m.has_focus(cmd_id) {
                    m.request_focus(cmd_id);
                }
                m.move_focus(egui::FocusDirection::None);
            });
        }
        let focused = ctx.memory(|m| m.focused());
        let cmd_focused = focused == Some(cmd_id);
        let other_text_focused = ctx.text_edit_focused() && !cmd_focused;
        if other_text_focused {
            return;
        }
        if !cmd_focused && !self.focus_cmd && self.cmd.is_empty() && self.workflow_keys(ctx) {
            return;
        }
        // Chorded bindings (⌘, Ctrl, Alt) work even while the command line has text.
        let typing = ctx.input(|i| i.events.iter().any(|e| matches!(e, Event::Text(t) if !t.is_empty())));
        let mut swallow_text = false;
        for &(key, m) in &presses {
            let Some(action) = self.prefs.hotkeys.action_for(key, m) else { continue };
            let chorded = m.command || m.ctrl || m.alt;
            if !chorded && (!self.cmd.is_empty() || (cmd_focused && key == Key::Tab)) {
                continue;
            }
            // ⇧B / ⇧S type letters: never while the command line is being typed into.
            let letter = !chorded && Self::types_text(key);
            if letter && (cmd_focused || typing) {
                continue;
            }
            if !self.hotkey_applies(action) {
                continue;
            }
            ctx.input_mut(|i| i.consume_key(m, key));
            swallow_text |= letter;
            self.run_hotkey(action);
        }
        if !cmd_focused && !swallow_text {
            // Type-anywhere: capture keystrokes until the command line has focus.
            // Judge modifiers per key event: the Alt release can arrive in the same frame
            // as Alt+T, so the frame-level modifier state is unreliable.
            let typed: String = ctx.input(|i| {
                let mut chord = false;
                let mut out = String::new();
                for e in &i.events {
                    match e {
                        Event::Key { pressed: true, modifiers, .. } => {
                            chord = modifiers.alt || modifiers.ctrl || modifiers.command
                        }
                        Event::Text(t) if !chord => out.push_str(t),
                        _ => {}
                    }
                }
                out
            });
            if !typed.is_empty() {
                let starts = !self.cmd.is_empty()
                    || typed.chars().next().is_some_and(|c| c.is_ascii_alphanumeric() || c == '^' || c == '?');
                if starts {
                    self.cmd = typed.to_ascii_uppercase();
                    self.command_changed();
                    self.focus_cmd = true;
                    // Transfer these text events once, before TextEdit is rendered.
                    ctx.input_mut(|i| i.events.retain(|e| !matches!(e, Event::Text(_))));
                    ctx.memory_mut(|m| m.request_focus(cmd_id));
                }
            }
        }
        if cmd_focused || self.focus_cmd || !self.cmd.is_empty() || !self.focused_is_chart() {
            return;
        }
        let pressed = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        if pressed(Modifiers::NONE, Key::Escape) {
            self.with_chart(|c, _| {
                c.cancel();
            });
        }
        if pressed(Modifiers::NONE, Key::Delete) || pressed(Modifiers::NONE, Key::Backspace) {
            self.with_chart(|c, _| {
                c.delete_selected();
            });
        }
        if pressed(Modifiers::NONE, Key::ArrowLeft) {
            self.with_chart(|c, _| c.pan_bars(-5.0));
        }
        if pressed(Modifiers::NONE, Key::ArrowRight) {
            self.with_chart(|c, _| c.pan_bars(5.0));
        }
        if pressed(Modifiers::COMMAND, Key::ArrowUp)
            || pressed(Modifiers::NONE, Key::Plus)
            || pressed(Modifiers::NONE, Key::Equals)
        {
            self.with_chart(|c, _| c.zoom(1.25));
        }
        if pressed(Modifiers::COMMAND, Key::ArrowDown) || pressed(Modifiers::NONE, Key::Minus) {
            self.with_chart(|c, _| c.zoom(0.8));
        }
    }

    /// Keys that also produce text (a bare or shifted letter, digit or symbol).
    fn types_text(key: Key) -> bool {
        let n = key.name();
        n.len() == 1 || matches!(key, Key::Space | Key::Minus | Key::Plus | Key::Equals | Key::Period | Key::Comma)
    }

    fn hotkey_applies(&self, a: prefs::Action) -> bool {
        use prefs::Action as A;
        match a {
            A::DrawTrendline
            | A::DrawHorizontal
            | A::DrawVertical
            | A::DrawFib
            | A::ResetView
            | A::JumpLatest
            | A::LogScale
            | A::Magnet => self.focused_is_chart(),
            a => a.layout_slot().is_none_or(|i| i < self.layout_names.len()),
        }
    }

    fn run_hotkey(&mut self, a: prefs::Action) {
        use prefs::Action as A;
        match a {
            A::Help => self.apply(Function::Help),
            A::Congress => self.apply(Function::Congress),
            A::Politicians => self.apply(Function::Politician(String::new())),
            A::Alerts => self.apply(Function::Alerts),
            A::Watchlist => self.apply(Function::Watchlist),
            A::Desk => self.apply(Function::Desk),
            A::Journal => self.apply(Function::Journal(String::new())),
            A::Refresh => self.apply(Function::Refresh),
            A::FrameTiming => self.show_perf = !self.show_perf,
            A::Portfolio => self.apply(Function::Portfolio(command::PortView::Summary, String::new())),
            A::Positions => self.apply(Function::Portfolio(command::PortView::Positions, String::new())),
            A::Orders => self.apply(Function::Portfolio(command::PortView::Orders, String::new())),
            A::Buy => self.quick_trade(true),
            A::Sell => self.quick_trade(false),
            A::NextPane => self.cycle_focus(1),
            A::PrevPane => self.cycle_focus(-1),
            A::ToggleSidebar => self.sidebar_open = !self.sidebar_open,
            A::NextTheme => self.cycle_theme(1),
            A::SwapSidebarSide => self.apply(Function::Sidebar("SWAP".into())),
            A::SplitRight => self.apply(Function::Split { down: false, panel: String::new() }),
            A::SplitDown => self.apply(Function::Split { down: true, panel: String::new() }),
            A::NewTab => {
                let target = self.focused_visible();
                let kind = self.panel_from_spec("", None).expect("chart");
                let id = self.new_pane(kind);
                self.dock_pane(id, target, crate::dock::Zone::Center);
            }
            A::ClosePane => self.close_pane(self.focus),
            A::PopOut => self.pop_out(self.focus),
            A::SaveLayout => {
                self.save_layout();
                self.status = format!("Saved layout {:?}", self.layout_name);
            }
            A::Settings => self.apply(Function::Settings),
            A::CommandPalette => self.palette.toggle(),
            A::DrawTrendline => self.with_chart(|c, _| c.start_tool(Tool::Trendline)),
            A::DrawHorizontal => self.with_chart(|c, _| c.start_tool(Tool::Horizontal)),
            A::DrawVertical => self.with_chart(|c, _| c.start_tool(Tool::Vertical)),
            A::DrawFib => self.with_chart(|c, _| c.start_tool(Tool::Fib)),
            A::ResetView => self.with_chart(|c, _| c.reset_view()),
            A::JumpLatest => self.with_chart(|c, _| c.jump_latest()),
            A::LogScale => self.with_chart(|c, _| c.log = !c.log),
            A::Magnet => self.with_chart(|c, _| c.magnet = !c.magnet),
            a => {
                if let Some(spec) = a.panel_spec() {
                    self.apply(Function::NewPanel(spec.into()));
                } else if let Some(name) = a.layout_slot().and_then(|i| self.layout_names.get(i)).cloned() {
                    self.switch_layout(&name);
                }
            }
        }
    }

    fn news_watchlist_keys(&self) -> Vec<String> {
        self.watchlists
            .group(&self.watch_ui.selected_group)
            .or_else(|| self.watchlists.group(&self.watchlists.last_used))
            .map(|g| g.members.iter().map(|m| m.symbol.clone()).collect())
            .unwrap_or_else(|| self.watchlist.clone())
    }

    fn news_leader_keys(&self) -> Vec<String> {
        self.panes
            .iter()
            .find_map(|p| match &p.kind {
                PaneKind::Research(r) => {
                    r.results.as_ref().map(|s| s.data.results.iter().map(|row| row.symbol.clone()).collect())
                }
                _ => None,
            })
            .unwrap_or_default()
    }

    fn periodic(&mut self) {
        let visible = self.visible_panes();
        let shown = |f: &dyn Fn(&PaneKind) -> bool| visible.iter().any(|&i| f(&self.panes[i].kind));
        let visible_wl = self.sidebar_open || shown(&|k| matches!(k, PaneKind::Watchlist));
        if visible_wl && self.last_quotes.is_none_or(|t| t.elapsed() > Duration::from_secs(20)) {
            self.worker.send(Request::Quotes(self.watchlist.clone()));
            self.last_quotes = Some(Instant::now());
        }
        if shown(&|k| matches!(k, PaneKind::Wei)) && self.last_wei.is_none_or(|t| t.elapsed() > Duration::from_secs(20))
        {
            self.worker.send(Request::Quotes(markets_view::overview_keys()));
            self.last_wei = Some(Instant::now());
        }
        if shown(&|k| matches!(k, PaneKind::Curve))
            && self.last_curve.is_none_or(|t| t.elapsed() > Duration::from_secs(600))
        {
            self.worker.send(Request::YieldCurves);
            self.last_curve = Some(Instant::now());
        }
        let watch_news = tr_data::news::bound_symbols(self.news_watchlist_keys());
        let leader_news = tr_data::news::bound_symbols(self.news_leader_keys());
        // News every 5 minutes and order books every second, while visible.
        for &i in &visible {
            let p = &mut self.panes[i];
            match &mut p.kind {
                PaneKind::News(n) if n.fetched.is_none_or(|t| t.elapsed() > Duration::from_secs(300)) => {
                    n.fetched = Some(Instant::now());
                    n.loading = true;
                    let symbols = match n.scope {
                        tr_data::news::NewsScope::Watchlist => Some(watch_news.clone()),
                        tr_data::news::NewsScope::Leaders => Some(leader_news.clone()),
                        tr_data::news::NewsScope::Symbol => n.symbol.clone().map(|s| vec![s]),
                        tr_data::news::NewsScope::Top => None,
                    };
                    self.worker.send(Request::News { pane: p.id, symbols });
                }
                PaneKind::Chart(c)
                    if c.show_news && c.news_fetched.is_none_or(|t| t.elapsed() > Duration::from_secs(300)) =>
                {
                    c.news_fetched = Some(Instant::now());
                    self.worker.send(Request::News { pane: p.id, symbols: Some(vec![c.symbol.clone()]) });
                }
                PaneKind::Options(o) if o.fetched.is_none_or(|t| t.elapsed() > Duration::from_secs(30)) => {
                    o.fetched = Some(Instant::now());
                    self.worker.send(Request::Options {
                        pane: p.id,
                        symbol: o.symbol.clone(),
                        expiration: o.expiration,
                    });
                }
                PaneKind::OrderBook(b) if b.fetched.is_none_or(|t| t.elapsed() > Duration::from_millis(1000)) => {
                    b.fetched = Some(Instant::now());
                    self.worker.send(Request::OrderBook { pane: p.id, symbol: b.symbol.clone(), depth: b.depth });
                }
                _ => {}
            }
        }
        self.broker_periodic(&visible);
        if let Some((q, at)) = &self.search_pending
            && at.elapsed() > Duration::from_millis(300)
        {
            self.worker.send(Request::Search(q.clone()));
            self.search_pending = None;
        }
        if self.last_feed.is_none_or(|t| t.elapsed() > Duration::from_secs(60)) {
            self.worker.send(Request::Latest { limit: 2_000 });
            self.last_feed = Some(Instant::now());
        }
        if self.price_alerts.rules.iter().any(|r| r.enabled)
            && self.last_price_alerts.is_none_or(|t| t.elapsed() > Duration::from_secs(20))
        {
            self.worker.send(Request::EvaluatePriceAlerts);
            self.last_price_alerts = Some(Instant::now());
        }
        self.sync_chart_alerts();
        self.sync_chart_journal();
        self.save_dirty_themes(false);
        if self.last_save.elapsed() > Duration::from_secs(10) {
            self.autosave();
            self.last_save = Instant::now();
        }
        self.toasts.retain(|(_, t)| t.elapsed() < Duration::from_secs(TOAST_SECS));
    }

    /// Save the active layout (only when it changed) and app settings.
    fn autosave(&mut self) {
        self.save_dirty_themes(true);
        let json = self.current_doc().to_json();
        if json != self.last_saved {
            self.save_layout();
        }
        self.save_settings();
    }
}

/// Apply the native macOS window chrome (unified toolbar, inset traffic lights, sidebar
/// vibrancy). Returns whether the window is now translucent.
#[cfg(target_os = "macos")]
fn style_native_window(frame: &eframe::Frame, vibrancy: bool) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let vibrancy = vibrancy && std::env::var("TR_VIBRANCY").map_or(true, |v| v != "0");
    let Ok(handle) = frame.window_handle() else { return false };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return false };
    // SAFETY: called on the main thread (eframe runs `ui` there) with a live NSView.
    let ok = unsafe { tr_macos::style_window(h.ns_view.as_ptr(), vibrancy) };
    tracing::info!("macOS window chrome applied: {ok} (vibrancy {vibrancy})");
    ok && vibrancy
}

#[cfg(not(target_os = "macos"))]
fn style_native_window(_frame: &eframe::Frame, _vibrancy: bool) -> bool {
    false
}

/// The main window's live title bar geometry (macOS).
#[cfg(target_os = "macos")]
fn native_chrome(frame: &eframe::Frame) -> Option<tr_macos::Chrome> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = frame.window_handle().ok()?;
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return None };
    // SAFETY: main thread, live NSView owned by the window.
    unsafe { tr_macos::chrome_metrics(h.ns_view.as_ptr()) }
}

#[cfg(not(target_os = "macos"))]
fn native_chrome(_frame: &eframe::Frame) -> Option<tr_macos::Chrome> {
    None
}

impl eframe::App for TerminalApp {
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        if self.vibrancy { [0.0; 4] } else { visuals.window_fill.to_normalized_gamma_f32() }
    }

    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let frame_start = Instant::now();
        if let Some(prev) = self.last_frame.replace(frame_start) {
            let dt = prev.elapsed().as_secs_f64() * 1e3;
            self.perf_dt_ms = if self.perf_dt_ms == 0.0 { dt } else { self.perf_dt_ms * 0.9 + dt * 0.1 };
        }
        let ctx = ui.ctx().clone();
        if !self.window_styled {
            self.window_styled = true;
            self.vibrancy = style_native_window(frame, self.prefs.appearance.vibrancy);
        } else if !self.updater_started {
            // From the second frame, so loading Sparkle doesn't delay the first.
            self.updater_started = true;
            tr_macos::updater::start();
        }
        if self.last_chrome_poll.is_none_or(|t| t.elapsed() > Duration::from_millis(750)) {
            let c = native_chrome(frame);
            if c != self.chrome {
                tracing::info!("window chrome: {c:?}");
                self.chrome = c;
            }
            self.last_chrome_poll = Some(Instant::now());
        }
        if self.last_accent_poll.is_none_or(|t| t.elapsed() > Duration::from_secs(3)) {
            if let Some([r, g, b]) = tr_macos::accent_color() {
                self.system_accent = Color32::from_rgb(r, g, b);
            }
            anim::set_reduced(tr_macos::reduce_motion());
            self.last_accent_poll = Some(Instant::now());
        }
        self.system_theme = ctx.system_theme();
        let ti = self.active_theme_index(&ctx);
        if self.applied_theme.as_ref() != Some(&self.themes[ti]) {
            theme::configure(&ctx, &self.themes[ti], self.system_accent);
            self.applied_theme = Some(self.themes[ti].clone());
        }
        theme::refresh(&ctx, &self.themes[ti], self.system_accent);
        self.drain();
        self.periodic();

        let screen = ui.max_rect();
        let open = anim::spring_with(
            &ctx,
            egui::Id::new("sidebar-open"),
            if self.sidebar_open { 1.0 } else { 0.0 },
            0.32,
            0.9,
        );
        let sb_w = (SIDEBAR_W * open).round();
        let right_side = self.prefs.appearance.sidebar_side == prefs::SidebarSide::Right;
        let main = if right_side {
            Rect::from_min_max(screen.min, pos2(screen.right() - sb_w, screen.bottom()))
        } else {
            Rect::from_min_max(pos2(screen.left() + sb_w, screen.top()), screen.max)
        };
        ui.painter().rect_filled(main, 0.0, theme::window());
        let toolbar = self.toolbar_rect(main);
        let status = Rect::from_min_max(pos2(main.left(), main.bottom() - STATUS_H), main.max);
        let gap = theme::dp(6.0);
        let area = Rect::from_min_max(
            pos2(main.left() + gap, toolbar.bottom() + gap),
            pos2(main.right() - gap, status.top() - gap * 0.5),
        );

        if let Some(error) = self.settings_writer.as_ref().and_then(|w| w.error()) {
            self.status = error;
        }
        self.hotkeys(&ctx);
        self.command_palette(&ctx);
        self.watchlist_dialogs(&ctx);
        let mut sidebar_cmds = Vec::new();
        if sb_w >= 1.0 {
            // The sidebar slides in from its edge.
            let x = if right_side { screen.right() - sb_w } else { screen.left() + sb_w - SIDEBAR_W };
            let full = Rect::from_min_size(pos2(x, screen.top()), vec2(SIDEBAR_W, screen.height()));
            let mut child = ui.new_child(UiBuilder::new().max_rect(full).id_salt("sidebar-root"));
            let visible = if right_side {
                Rect::from_min_max(pos2(screen.right() - sb_w, screen.top()), screen.max)
            } else {
                Rect::from_min_max(screen.min, pos2(screen.left() + sb_w, screen.bottom()))
            };
            child.set_clip_rect(visible);
            sidebar_cmds = self.sidebar(&mut child, full);
        }
        // The traffic lights sit at the window's left edge: clear them unless a left
        // sidebar already covers that corner.
        let inset = if right_side { self.traffic_inset() } else { (self.traffic_inset() - sb_w).max(0.0) };
        self.toolbar(ui, toolbar, inset);
        self.status_bar(ui, status);

        let mut actions = Vec::new();
        for c in sidebar_cmds {
            match c {
                SidebarCmd::Action(a) => actions.push(a),
                SidebarCmd::Function(f) => self.apply(f),
                SidebarCmd::Layout(op) => self.layout_op(op),
            }
        }
        actions.extend(self.render_dock(ui, area, gap));
        actions.extend(self.render_windows(&ctx));
        self.run_actions(actions);
        self.trading_windows(&ctx);
        self.toasts(&ctx);

        // TR_SMOKE_SECS=N: render for N seconds, log frame stats and quit (CI smoke test).
        if let Some(secs) = std::env::var("TR_SMOKE_SECS").ok().and_then(|v| v.parse::<f64>().ok()) {
            ctx.request_repaint();
            if !self.smoke_done && self.started.elapsed().as_secs_f64() > secs {
                self.smoke_done = true;
                tracing::info!(
                    "smoke: {} frames in {:.1}s, ui cpu {:.2} ms/frame, panes {}, layout {:?}",
                    ctx.cumulative_frame_nr(),
                    self.started.elapsed().as_secs_f64(),
                    self.perf_cpu_ms,
                    self.panes.len(),
                    self.layout_name
                );
                self.autosave();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        let cpu = frame_start.elapsed().as_secs_f64() * 1e3;
        self.perf_cpu_ms = self.perf_cpu_ms * 0.9 + cpu * 0.1;
        if self.show_perf {
            // Measure continuously while the readout is visible.
            ctx.request_repaint();
        }
        // Keep the clock ticking and pick up worker results without spinning.
        ctx.request_repaint_after(Duration::from_millis(500));
        if self.started.elapsed() < Duration::from_secs(2) {
            ctx.request_repaint();
        }
    }

    fn on_exit(&mut self) {
        self.autosave();
        if self.demo.save_error.is_some() {
            self.demo.save();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Isolated UI fixture: all paths are temporary and the worker only records requests.
    pub(super) fn test_app() -> TerminalApp {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "terminal-rs-bar-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let (worker, _) = Worker::test_channel();
        let ctx = egui::Context::default();
        let prefs = Prefs::default();
        let themes = crate::themes::builtins();
        let theme_store = crate::themes::ThemeStore::new(dir.join("themes"));
        let system_accent = theme::DEFAULT_ACCENT;
        let (demo, _) = demo::DemoState::load(dir.join("demo.json"));
        let broker = broker::BrokerState {
            store: tr_broker::SecretStore::file(dir.join("unused-secrets.json")),
            saved: Default::default(),
            form: Default::default(),
            creds: Default::default(),
            from_env: false,
            reveal: false,
            portfolio: None,
            error: None,
            loading: false,
            fetched: None,
            message: None,
            busy: None,
            portal_url: None,
            connecting_since: None,
            simulate: true,
            last_quotes: None,
            testing: false,
            confirm_disconnect: None,
            disconnecting: None,
        };
        TerminalApp {
            broker,
            trading: Default::default(),
            demo,
            worker,
            provider: "sample".into(),
            panes: Vec::new(),
            dock: Node::default(),
            focus: 0,
            next_id: 1,
            last_opened: None,
            preset: None,
            drag: None,
            dock_motion: Default::default(),
            renaming_tab: None,
            store: Store::new(dir.join("layouts")),
            layout_name: "Default".into(),
            layout_names: Vec::new(),
            last_saved: Vec::new(),
            renaming_layout: None,
            editing_theme: prefs.appearance.theme.clone(),
            themes,
            theme_store,
            applied_theme: None,
            dirty_themes: Vec::new(),
            themes_changed_at: None,
            renaming_theme: None,
            theme_io: String::new(),
            system_theme: ctx.system_theme(),
            layout_theme: false,
            prefs,
            rebinding: None,
            hotkey_filter: String::new(),
            settings_tab: SettingsTab::Appearance,
            whats_news_epoch: 0,
            whats_news_health: tr_data::whats_news::Health::pending(),
            io_path: String::new(),
            curves: None,
            last_wei: None,
            last_curve: None,
            wei_updated: None,
            net_hits: (String::new(), Vec::new()),
            search_pending: None,
            suggestion: None,
            command_catalogue: Default::default(),
            recent_symbols: Vec::new(),
            cmd: String::new(),
            cmd_history: Vec::new(),
            cmd_hist_pos: None,
            focus_cmd: true,
            status: "Ready. Type a ticker and press <GO> (Enter). F1 for help.".into(),
            toasts: Vec::new(),
            latest: Vec::new(),
            politicians: Vec::new(),
            pol_search: String::new(),
            alerts: Vec::new(),
            price_alerts: tr_data::price_alerts::AlertBatch::empty(0),
            alert_draft: alerts::AlertDraft::default(),
            last_price_alerts: None,
            journal: Default::default(),
            congress_status: String::new(),
            congress_checked: None,
            watchlist: DEFAULT_WATCHLIST.map(String::from).to_vec(),
            watchlists: watchlists::Watchlists::migrate(&DEFAULT_WATCHLIST.map(String::from)),
            watch_ui: Default::default(),
            settings_writer: None,
            quotes: HashMap::new(),
            last_quotes: None,
            last_feed: None,
            last_save: Instant::now(),
            started: Instant::now(),
            smoke_done: false,
            show_perf: false,
            perf_cpu_ms: 0.0,
            perf_dt_ms: 0.0,
            last_frame: None,
            sidebar_open: true,
            system_accent,
            last_accent_poll: None,
            window_styled: false,
            updater_started: false,
            vibrancy: false,
            chrome: None,
            last_chrome_poll: None,
            palette: Default::default(),
        }
    }

    #[test]
    fn price_history_commands_preserve_progress_panel_and_submit_background_work() {
        let mut app = test_app();
        let (worker, mut requests) = Worker::test_channel();
        app.worker = worker;
        app.apply(Function::History("ADD AAPL,MSFT".into()));
        assert!(matches!(requests.try_recv(),Ok(Request::HistoryTask(command)) if command=="ADD AAPL,MSFT"));
        assert!(matches!(requests.try_recv(), Ok(Request::HistoryStatus)));
        assert!(app.panes.iter().any(|p| p.kind.settings() == PaneSettings::PriceHistory));
        app.apply(Function::Universe);
        assert!(matches!(requests.try_recv(), Ok(Request::Research { scan: None, .. })));
        assert_eq!(app.panes.len(), 2, "Research must preserve the price-history progress panel");
        app.apply(Function::History("CANCEL".into()));
        assert!(matches!(requests.try_recv(),Ok(Request::HistoryTask(command)) if command=="CANCEL"));
        assert!(app.panes.iter().any(|p| p.kind.settings() == PaneSettings::Research(None)));
        assert_eq!(app.panes.len(), 2);
        let kind = app.panel_from_spec("HIST", None).unwrap();
        assert_eq!(kind.settings(), PaneSettings::PriceHistory);
    }

    #[test]
    fn recent_fx_keys_are_repaired_and_deduplicated_at_startup() {
        let symbols = ["USDBRLUSD Curncy", "BRL Curncy", "BRL=X", "BTC", "???", "ação", "SPY"].map(String::from);
        assert_eq!(canonical_recent_symbols(&symbols), ["USDBRL Curncy", "BTCUSD Curncy", "SPY US Equity"]);
    }

    #[test]
    fn legacy_settings_migrate_to_a_layout() {
        let old = r#"{"layout":"MainRight2","watchlist":["AAPL","BTCUSD Curncy"],"sidebar":false,
            "panes":[{"Chart":{"symbol":"NVDA","tf":"D1","chart_type":"Candles","log":false,"indicators":[],
            "drawings":[],"compare":null}},"Feed","Wei"]}"#;
        let s: Settings = serde_json::from_str(old).unwrap();
        assert!(!s.sidebar);
        let doc = migrate_legacy(&s, &prefs::PanelDefaults::default()).unwrap();
        assert_eq!(doc.panes.len(), 3);
        assert_eq!(doc.dock.visible(), vec![1, 2, 3]);
        assert!(matches!(&doc.panes[0].settings, PaneSettings::Chart { symbol, .. } if symbol == "NVDA"));
        assert_eq!(doc.panes[1].settings, PaneSettings::Feed(None));
        // New-format files carry no legacy fields and are not migrated.
        let new: Settings = serde_json::from_str(r#"{"watchlist":[],"active_layout":"Desk"}"#).unwrap();
        assert!(migrate_legacy(&new, &prefs::PanelDefaults::default()).is_none());
        assert_eq!(new.active_layout.as_deref(), Some("Desk"));
    }

    #[test]
    fn pane_settings_round_trip_through_kinds() {
        let d = prefs::PanelDefaults::default();
        for s in [
            crate::layouts::chart_settings("BTCUSD Curncy", &d),
            PaneSettings::Feed(Some(crate::layouts::FeedSettings { buys: Some(true), ..Default::default() })),
            PaneSettings::News(crate::layouts::NewsSettings::Full {
                symbol: Some("AAPL US Equity".into()),
                scope: tr_data::news::NewsScope::Symbol,
            }),
            PaneSettings::News(crate::layouts::NewsSettings::Full {
                symbol: None,
                scope: tr_data::news::NewsScope::Top,
            }),
            PaneSettings::News(crate::layouts::NewsSettings::Full {
                symbol: None,
                scope: tr_data::news::NewsScope::Watchlist,
            }),
            PaneSettings::OrderBook { symbol: "ETHUSD Curncy".into(), depth: 50, group: 1.0 },
            PaneSettings::Search("gold".into()),
            PaneSettings::Settings,
            PaneSettings::Wei,
            PaneSettings::PriceHistory,
            PaneSettings::Research(None),
            PaneSettings::Research(Some("DARVAS_BREAKOUT".into())),
            PaneSettings::Desk { group: "Growth".into(), selected: Some("AAPL US Equity".into()) },
            PaneSettings::Backtest {
                symbol: "AAPL US Equity".into(),
                spec: "ADTREND".into(),
                holdout: "20%".into(),
                commission: "5".into(),
                slippage: "5".into(),
                shorts: false,
            },
            {
                let mut c = ChartState::new(0, "AAPL", tr_core::Timeframe::D1);
                c.archive = true;
                c.enable_review_overlays();
                PaneKind::Chart(Box::new(c)).settings()
            },
        ] {
            assert_eq!(PaneKind::from_settings(s.clone()).settings(), s);
        }
    }

    #[test]
    fn review_opens_linked_daily_and_weekly_archive_charts() {
        let mut app = test_app();
        let (worker, mut requests) = Worker::test_channel();
        app.worker = worker;
        app.apply(Function::Review);
        let charts: Vec<_> = app
            .panes
            .iter()
            .filter_map(|p| match &p.kind {
                PaneKind::Chart(c) => Some((c.archive, c.tf, c.show_box, c.show_rs, c.show_volume_ctx, p.link)),
                _ => None,
            })
            .collect();
        assert_eq!(charts.len(), 2);
        assert!(charts.iter().any(|c| c.0 && c.1 == tr_core::Timeframe::D1 && c.2 && c.3 && c.4));
        assert!(charts.iter().any(|c| c.0 && c.1 == tr_core::Timeframe::W1 && c.2 && c.3 && c.4));
        assert_eq!(charts[0].5, charts[1].5);
        assert!(charts[0].5.is_some());
        let reqs: Vec<_> = std::iter::from_fn(|| requests.try_recv().ok()).collect();
        let archive: Vec<_> = reqs.iter().filter(|r| matches!(r, Request::ArchiveHistory { .. })).collect();
        assert_eq!(archive.len(), 2);
        assert!(archive.iter().all(|r| matches!(r, Request::ArchiveHistory { before: None, .. })));
        assert!(!reqs.iter().any(|r| matches!(r, Request::History { .. })));
    }
}

#[cfg(test)]
mod ticker_switch_tests;
