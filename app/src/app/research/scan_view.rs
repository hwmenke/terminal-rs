//! SCAN VIEW PATTERN / RSI-C: dense sortable boards over the verified research archive.
//! One engine board feeds both views; only columns and buckets differ. All compute happens in
//! the worker (`scan_view_research`); the pane only filters, sorts and paints cached rows.
use super::ResearchPane;
use crate::{congress_view::Action, theme, widgets::HeatScale};
use egui::{Align2, Color32, Key, Painter, Rect, Sense, Stroke, Ui, pos2, vec2};
use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, LazyLock},
};
use tr_core::{Bar, Provenance};
use tr_data::whats_news::{
    Skipped,
    equity_engine::{
        Agreement, BoardCache, DAILY_TAIL, EngineBoard, EngineRow, PatternKind, PatternRead, RsiCounter, RsicState,
        ScanView, ScanViewSpec, Unavailable, WeeklyRead, refuse_sample, sort_order,
    },
};

#[cfg(test)]
mod tests;

const ROW_H: f32 = 22.0;
pub const SAMPLE_REFUSAL: &str =
    "SCAN VIEW unavailable in sample / offline display mode — verified archive bars only";
const INSPECTOR: &str = "Reference EWM RSI (α=1/n, first-delta seed, ≥n deltas; no-loss window reads 100) — differs from scan RSI14 (Wilder SMA seed, flat 50). Windows: last 400 daily / 120 completed weekly closes. Weekly = completed weeks only.";

/// Worker payload. The board is cached by content; loader skips and source ride beside it.
#[derive(Debug, Clone)]
pub struct ViewSnapshot {
    pub board: Arc<EngineBoard>,
    pub skipped: Vec<Skipped>,
    pub source: String,
    pub provenance: Provenance,
}

pub(crate) type Loaded = (Vec<(String, Vec<Bar>)>, Vec<Skipped>, String, Provenance);

static BOARD_CACHE: LazyLock<BoardCache> = LazyLock::new(BoardCache::default);
/// One board load at a time: a second pane waits, then hits the cache.
static VIEW_GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

/// Testable worker core. Sample mode refuses before `load` (no DB, no bridge, no provider).
pub(crate) async fn scan_view_research<F, Fut>(
    sample: bool,
    load: F,
    spec: &ScanViewSpec,
) -> anyhow::Result<ViewSnapshot>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = anyhow::Result<Loaded>>,
{
    if sample {
        anyhow::bail!("{SAMPLE_REFUSAL}");
    }
    let _permit = VIEW_GATE.acquire().await?;
    let (mut histories, skipped, source, provenance) = load().await?;
    provenance.require_verified("SCAN VIEW").map_err(anyhow::Error::msg)?;
    refuse_sample(&histories).map_err(anyhow::Error::msg)?;
    // The engine consumes only the last DAILY_TAIL bars; don't carry the rest.
    for (_, bars) in &mut histories {
        if bars.len() > DAILY_TAIL {
            bars.drain(..bars.len() - DAILY_TAIL);
        }
    }
    let params = spec.params;
    let (board, _hit) = tokio::task::spawn_blocking(move || BOARD_CACHE.get_or_compute(&histories, params)).await?;
    Ok(ViewSnapshot { board, skipped, source, provenance })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    D,
    W,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fam {
    Os,
    Ob,
    Up,
    Down,
    Mixed,
}
impl Fam {
    fn of(s: RsicState) -> Self {
        if s.is_os() {
            Self::Os
        } else if s.is_ob() {
            Self::Ob
        } else if s.is_up() {
            Self::Up
        } else if s.is_down() {
            Self::Down
        } else {
            Self::Mixed
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Os => "OS",
            Self::Ob => "OB",
            Self::Up => "↑ TREND",
            Self::Down => "↓ TREND",
            Self::Mixed => "MIXED",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Bucket {
    DPat(PatternKind),
    WPat(PatternKind),
    D(Fam),
    W(Fam),
    DwUp,
    DwDown,
    Pullback,
    Accel,
    Fade,
}
const KINDS: [PatternKind; 4] =
    [PatternKind::Breakout, PatternKind::Breakdown, PatternKind::FromBottom, PatternKind::FromTop];
const FAMS: [Fam; 5] = [Fam::Os, Fam::Ob, Fam::Up, Fam::Down, Fam::Mixed];
impl Bucket {
    pub fn is_weekly(self) -> bool {
        matches!(self, Self::WPat(_) | Self::W(_))
    }
    fn combo(self) -> bool {
        matches!(self, Self::DwUp | Self::DwDown | Self::Pullback)
    }
    fn label(self) -> String {
        match self {
            Self::DPat(k) | Self::WPat(k) => kind_label(k).to_string(),
            Self::D(f) | Self::W(f) => f.label().to_string(),
            Self::DwUp => "D+W ↑".into(),
            Self::DwDown => "D+W ↓".into(),
            Self::Pullback => "PULLBACK".into(),
            Self::Accel => "ACCEL".into(),
            Self::Fade => "FADE".into(),
        }
    }
    fn tint(self) -> Color32 {
        match self {
            Self::DPat(k) | Self::WPat(k) => kind_fill(k).0,
            Self::D(f) | Self::W(f) => match f {
                Fam::Os => theme::heat_div(0),
                Fam::Ob => theme::heat_div(4),
                Fam::Up => theme::up().gamma_multiply(0.5),
                Fam::Down => theme::down().gamma_multiply(0.5),
                Fam::Mixed => theme::control(),
            },
            Self::DwUp | Self::Accel => theme::up().gamma_multiply(0.5),
            Self::DwDown | Self::Fade => theme::down().gamma_multiply(0.5),
            Self::Pullback => theme::amber().gamma_multiply(0.5),
        }
    }
    /// Membership of one row (cohort rules are applied by the caller).
    pub fn holds(self, r: &EngineRow) -> bool {
        let weekly = r.weekly.as_ref().ok();
        match self {
            Self::DPat(k) => r.pattern_d.as_ref().is_ok_and(|p| p.kind == Some(k)),
            Self::WPat(k) => weekly.and_then(|w| w.pattern.as_ref().ok()).is_some_and(|p| p.kind == Some(k)),
            Self::D(f) => r.rsic_d.as_ref().is_ok_and(|c| Fam::of(c.state) == f),
            Self::W(f) => weekly.and_then(|w| w.rsic.as_ref().ok()).is_some_and(|c| Fam::of(c.state) == f),
            Self::DwUp => r.agreement == Some(Agreement::Up),
            Self::DwDown => r.agreement == Some(Agreement::Down),
            Self::Pullback => r.pullback,
            Self::Accel => r.accel.is_some_and(|a| a >= 0.0),
            Self::Fade => r.accel.is_some_and(|a| a < 0.0),
        }
    }
}
fn kind_label(k: PatternKind) -> &'static str {
    match k {
        PatternKind::Breakout => "▲ BREAKOUT",
        PatternKind::Breakdown => "▼ BREAKDOWN",
        PatternKind::FromBottom => "↗ FROM BOTTOM",
        PatternKind::FromTop => "↘ FROM TOP",
    }
}
/// (fill, ink) for a pattern badge: breaks are solid, thirds are tinted.
fn kind_fill(k: PatternKind) -> (Color32, Color32) {
    match k {
        PatternKind::Breakout => (theme::up(), theme::on_fill(theme::up())),
        PatternKind::Breakdown => (theme::down(), theme::on_fill(theme::down())),
        PatternKind::FromBottom => (theme::up().gamma_multiply(0.3), theme::up()),
        PatternKind::FromTop => (theme::down().gamma_multiply(0.3), theme::down()),
    }
}
fn kind_rank(k: PatternKind) -> f64 {
    match k {
        PatternKind::Breakout => 4.0,
        PatternKind::FromBottom => 3.0,
        PatternKind::FromTop => 2.0,
        PatternKind::Breakdown => 1.0,
    }
}
/// (fill, ink) for an RSI-C state badge.
fn state_fill(s: RsicState) -> (Color32, Color32) {
    let heat = |step: usize| {
        let f = theme::heat_div(step);
        (f, theme::on_fill(f))
    };
    match s {
        RsicState::OsExtreme | RsicState::Oversold => heat(0),
        RsicState::OsLean => heat(1),
        RsicState::ObExtreme | RsicState::Overbought => heat(4),
        RsicState::ObLean => heat(3),
        RsicState::UpStrong => (theme::up(), theme::on_fill(theme::up())),
        RsicState::Up => (theme::up().gamma_multiply(0.45), theme::up()),
        RsicState::UpTilt => (theme::up().gamma_multiply(0.22), theme::up()),
        RsicState::DownStrong => (theme::down(), theme::on_fill(theme::down())),
        RsicState::Down => (theme::down().gamma_multiply(0.45), theme::down()),
        RsicState::DownTilt => (theme::down().gamma_multiply(0.22), theme::down()),
        RsicState::Mixed => (theme::control(), theme::muted()),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Col {
    Ticker,
    Last,
    Close,
    DPattern,
    DPos,
    VsH,
    VsL,
    WPattern,
    WPos,
    WWks,
    WWeek,
    DRsic,
    DState,
    DMean,
    DAlign,
    DOs,
    DOb,
    DUp,
    DDown,
    Strip,
    WState,
    WMean,
    WAlign,
    Dw,
    Accel,
}
struct ColDef {
    col: Col,
    title: &'static str,
    w: f32,
    side: Option<Side>,
}
const fn c(col: Col, title: &'static str, w: f32, side: Option<Side>) -> ColDef {
    ColDef { col, title, w, side }
}
const PATTERN_COLS: &[ColDef] = &[
    c(Col::Ticker, "TICKER", 80.0, None),
    c(Col::Last, "LAST", 84.0, None),
    c(Col::Close, "CLOSE", 70.0, None),
    c(Col::DPattern, "D PATTERN", 118.0, Some(Side::D)),
    c(Col::DPos, "D POS%", 62.0, Some(Side::D)),
    c(Col::VsH, "vs H63%", 66.0, Some(Side::D)),
    c(Col::VsL, "vs L63%", 66.0, Some(Side::D)),
    c(Col::WPattern, "W PATTERN", 118.0, Some(Side::W)),
    c(Col::WPos, "W POS%", 62.0, Some(Side::W)),
    c(Col::WWks, "W WKS", 52.0, Some(Side::W)),
    c(Col::WWeek, "W WEEK", 84.0, Some(Side::W)),
    c(Col::DRsic, "D RSI-C", 112.0, Some(Side::D)),
];
const STRIP_CELL: f32 = 9.0;
const RSIC_COLS: &[ColDef] = &[
    c(Col::Ticker, "TICKER", 80.0, None),
    c(Col::Last, "LAST", 84.0, None),
    c(Col::DState, "D STATE", 124.0, Some(Side::D)),
    c(Col::DMean, "D MEAN", 58.0, Some(Side::D)),
    c(Col::DAlign, "D ALIGN", 64.0, Some(Side::D)),
    c(Col::DOs, "D <20", 54.0, Some(Side::D)),
    c(Col::DOb, "D >80", 54.0, Some(Side::D)),
    c(Col::DUp, "D ↑", 54.0, Some(Side::D)),
    c(Col::DDown, "D ↓", 54.0, Some(Side::D)),
    c(Col::Strip, "RSI 2…21", 20.0 * STRIP_CELL + 10.0, Some(Side::D)),
    c(Col::WState, "W STATE", 112.0, Some(Side::W)),
    c(Col::WMean, "W MEAN", 58.0, Some(Side::W)),
    c(Col::WAlign, "W ALIGN", 64.0, Some(Side::W)),
    c(Col::Dw, "D/W", 70.0, None),
    c(Col::Accel, "Δ", 74.0, None),
];
fn cols(view: ScanView) -> &'static [ColDef] {
    match view {
        ScanView::Pattern => PATTERN_COLS,
        ScanView::RsiCounter => RSIC_COLS,
    }
}
fn col_at(view: ScanView, x: f32) -> Option<&'static ColDef> {
    let mut left = 0.0;
    cols(view).iter().find(|d| {
        left += d.w;
        x < left
    })
}

fn rsi_scale() -> HeatScale {
    HeatScale::diverging(30.0, "RSI − 50", |v| format!("{v:.0}"))
}
fn pos_scale() -> HeatScale {
    HeatScale::diverging(50.0 / 3.0, "pos − 50 (%)", |v| format!("{v:.1}"))
}

#[derive(Clone, Default)]
pub struct Counts {
    map: HashMap<Bucket, usize>,
    /// Rows in the same-date cohort (or all rows when older-dated rows are included).
    pub cohort: usize,
    pub older: usize,
}
impl Counts {
    pub fn get(&self, b: Bucket) -> usize {
        self.map.get(&b).copied().unwrap_or(0)
    }
}

#[derive(Clone)]
pub struct ScanViewPane {
    pub spec: Result<ScanViewSpec, String>,
    pub snapshot: Option<Arc<ViewSnapshot>>,
    pub sort: (Col, bool),
    pub bucket: Option<Bucket>,
    pub include_older: bool,
    pub hits_only: bool,
    pub order: Vec<usize>,
    pub counts: Counts,
    pub side: Side,
    /// Last navigation message ("<SYM>: weekly unavailable — …"); the app copies it to the status bar.
    pub notice: Option<String>,
    query: String,
}

impl ScanViewPane {
    pub fn new(spec: Result<ScanViewSpec, String>) -> Self {
        let rsic = spec.as_ref().is_ok_and(|s| s.view == ScanView::RsiCounter);
        Self {
            spec,
            snapshot: None,
            sort: if rsic { (Col::DMean, true) } else { (Col::DPos, true) },
            bucket: None,
            include_older: false,
            hits_only: true,
            order: vec![],
            counts: Counts::default(),
            side: Side::D,
            notice: None,
            query: String::new(),
        }
    }
    pub fn view(&self) -> Option<ScanView> {
        self.spec.as_ref().ok().map(|s| s.view)
    }
    fn board(&self) -> Option<&EngineBoard> {
        self.snapshot.as_deref().map(|s| &*s.board)
    }
    fn in_day(&self, b: &EngineBoard, r: &EngineRow) -> bool {
        self.include_older || Some(r.last_ts) == b.as_of_ts
    }
    fn in_week(&self, b: &EngineBoard, r: &EngineRow) -> bool {
        self.include_older || r.weekly.as_ref().ok().map(|w| w.week_start) == b.as_of_week && b.as_of_week.is_some()
    }
    fn counted(&self, b: &EngineBoard, r: &EngineRow, bucket: Bucket) -> bool {
        let cohort = if bucket.is_weekly() {
            self.in_week(b, r)
        } else if bucket.combo() {
            self.in_day(b, r) && self.in_week(b, r)
        } else {
            self.in_day(b, r)
        };
        cohort && bucket.holds(r)
    }
    pub fn buckets(&self) -> Vec<Bucket> {
        match self.view() {
            Some(ScanView::Pattern) => {
                KINDS.iter().map(|k| Bucket::DPat(*k)).chain(KINDS.iter().map(|k| Bucket::WPat(*k))).collect()
            }
            Some(ScanView::RsiCounter) => FAMS
                .iter()
                .map(|f| Bucket::D(*f))
                .chain(FAMS.iter().map(|f| Bucket::W(*f)))
                .chain([Bucket::DwUp, Bucket::DwDown, Bucket::Pullback, Bucket::Accel, Bucket::Fade])
                .collect(),
            None => vec![],
        }
    }
    /// Recount the cohort buckets (on a new snapshot or the older-dated toggle; never per frame).
    pub fn recount(&mut self) {
        let mut counts = Counts::default();
        if let Some(snap) = self.snapshot.clone() {
            let b = &*snap.board;
            for bucket in self.buckets() {
                counts.map.insert(bucket, b.rows.iter().filter(|r| self.counted(b, r, bucket)).count());
            }
            counts.cohort = b.rows.iter().filter(|r| self.in_day(b, r)).count();
            counts.older = b.rows.len() - b.rows.iter().filter(|r| Some(r.last_ts) == b.as_of_ts).count();
        }
        self.counts = counts;
    }
    /// Rebuild `order` from the filter, bucket, cohort and sort (header click / filter change only).
    pub fn refilter(&mut self, query: &str) {
        self.query = query.trim().to_ascii_uppercase();
        let Some(snap) = self.snapshot.clone() else {
            self.order.clear();
            return;
        };
        let b = &*snap.board;
        let pattern = self.view() == Some(ScanView::Pattern);
        let idx: Vec<usize> = b
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                if !self.query.is_empty() && !r.symbol.to_ascii_uppercase().contains(&self.query) {
                    return false;
                }
                match self.bucket {
                    Some(bucket) => self.counted(b, r, bucket),
                    None => {
                        self.in_day(b, r)
                            && (!pattern
                                || !self.hits_only
                                || r.pattern_d.as_ref().is_ok_and(|p| p.kind.is_some())
                                || r.weekly
                                    .as_ref()
                                    .ok()
                                    .and_then(|w| w.pattern.as_ref().ok())
                                    .is_some_and(|p| p.kind.is_some()))
                    }
                }
            })
            .map(|(i, _)| i)
            .collect();
        let (col, desc) = self.sort;
        self.order = if col == Col::Ticker {
            let mut o = sort_order(&b.rows, &idx, |_| None, false);
            if desc {
                o.reverse();
            }
            o
        } else {
            sort_order(&b.rows, &idx, |r| sort_key(col, r), desc)
        };
    }
    pub fn set_sort(&mut self, col: Col) {
        self.sort = if self.sort.0 == col { (col, !self.sort.1) } else { (col, col != Col::Ticker) };
        let q = self.query.clone();
        self.refilter(&q);
    }
    pub fn toggle_bucket(&mut self, bucket: Bucket) {
        if self.bucket == Some(bucket) {
            self.bucket = None;
        } else {
            self.bucket = Some(bucket);
            self.side = if bucket.is_weekly() { Side::W } else { Side::D };
        }
        let q = self.query.clone();
        self.refilter(&q);
    }
    pub fn symbols(&self) -> Vec<&str> {
        let Some(b) = self.board() else { return vec![] };
        self.order.iter().filter_map(|i| b.rows.get(*i)).map(|r| r.symbol.as_str()).collect()
    }
    pub fn row_at(&self, index: usize) -> Option<&EngineRow> {
        self.board()?.rows.get(*self.order.get(index)?)
    }
    pub fn row_for(&self, symbol: &str) -> Option<&EngineRow> {
        let key = tr_data::security(symbol).key();
        self.board()?.rows.iter().find(|r| r.symbol.eq_ignore_ascii_case(symbol) || tr_data::security(&r.symbol).key() == key)
    }
    /// Trigger bar to open for `side`. Never substitutes the other side.
    pub fn trigger(&self, row: &EngineRow, side: Side) -> Result<i64, String> {
        let name = tr_data::security(&row.symbol).ticker;
        let view = self.view().unwrap_or(ScanView::Pattern);
        let why = |u: &Unavailable| u.to_string();
        match side {
            Side::D => match view {
                ScanView::Pattern => row.pattern_d.as_ref().map(|p| p.trigger_ts).map_err(why),
                ScanView::RsiCounter => row.rsic_d.as_ref().map(|_| row.last_ts).map_err(why),
            }
            .map_err(|e| format!("{name}: daily unavailable — {e}")),
            Side::W => match &row.weekly {
                Err(e) => Err(why(e)),
                Ok(w) => match view {
                    ScanView::Pattern => w.pattern.as_ref().map(|p| p.trigger_ts).map_err(why),
                    ScanView::RsiCounter => w.rsic.as_ref().map(|_| w.trigger_ts).map_err(why),
                },
            }
            .map_err(|e| format!("{name}: weekly unavailable — {e}")),
        }
    }
    /// Enter / click: open the chosen side's trigger bar, or leave a notice.
    pub fn open(&mut self, index: usize, side: Side) -> Option<Action> {
        let row = self.row_at(index)?.clone();
        match self.trigger(&row, side) {
            Ok(ts) => {
                self.notice = None;
                Some(Action::OpenArchiveAt { symbol: tr_data::security(&row.symbol).key(), ts })
            }
            Err(msg) => {
                self.notice = Some(msg);
                None
            }
        }
    }
    /// (source, provenance, last date, close) for watchlist saves and handoff.
    pub fn origin_for(&self, symbol: &str) -> Option<(String, Provenance, Option<String>, Option<f64>)> {
        let snap = self.snapshot.as_ref()?;
        let row = self.row_for(symbol)?;
        Some((snap.source.clone(), snap.provenance, Some(row.last_date.clone()), Some(row.close)))
    }
}

fn sort_key(col: Col, r: &EngineRow) -> Option<f64> {
    let d = r.rsic_d.as_ref().ok();
    let w = r.weekly.as_ref().ok();
    let wr = w.and_then(|w| w.rsic.as_ref().ok());
    let wp = w.and_then(|w| w.pattern.as_ref().ok());
    let dp = r.pattern_d.as_ref().ok();
    match col {
        Col::Ticker => None,
        Col::Last => Some(r.last_ts as f64),
        Col::Close => Some(r.close),
        Col::DPattern => dp.and_then(|p| p.kind).map(kind_rank),
        Col::DPos => dp.and_then(|p| p.position),
        Col::VsH => dp.map(|p| (r.close / p.hi_long - 1.0) * 100.0),
        Col::VsL => dp.map(|p| (r.close / p.lo_long - 1.0) * 100.0),
        Col::WPattern => wp.and_then(|p| p.kind).map(kind_rank),
        Col::WPos => wp.and_then(|p| p.position),
        Col::WWks => wp.map(|p| p.bars_used as f64),
        Col::WWeek => w.map(|w| w.week_start as f64),
        Col::DRsic | Col::DState => d.map(|c| f64::from(c.state.sort_key())),
        Col::DMean | Col::Strip => d.map(|c| c.mean),
        Col::DAlign => d.map(|c| c.align),
        Col::DOs => d.map(|c| c.share(c.os20)),
        Col::DOb => d.map(|c| c.share(c.ob80)),
        Col::DUp => d.map(|c| c.share(c.up)),
        Col::DDown => d.map(|c| c.share(c.down)),
        Col::WState => wr.map(|c| f64::from(c.state.sort_key())),
        Col::WMean => wr.map(|c| c.mean),
        Col::WAlign => wr.map(|c| c.align),
        Col::Dw => r.agreement.map(|a| if a == Agreement::Up { 1.0 } else { -1.0 }),
        Col::Accel => r.accel,
    }
}

fn date(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0).map(|t| t.date_naive().to_string()).unwrap_or_else(|| "—".into())
}
fn weekday_date(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0).map(|t| t.format("%a %Y-%m-%d").to_string()).unwrap_or_else(|| "—".into())
}
fn pct(v: f64) -> String {
    format!("{:.1}%", v * 100.0)
}

/// Hover text for a pattern cell: the exact comparison that fired.
pub fn pattern_tip(p: &PatternRead, weekly: bool) -> String {
    let (h, hs, l, ls) = (p.hi_long, p.hi_short, p.lo_long, p.lo_short);
    let (long, short) = if weekly { (format!("{}wk", p.bars_used), "26wk".to_string()) } else { ("63".into(), "21".into()) };
    let (hi_f, lo_f) = if weekly { ("0.999 × ", "1.001 × ") } else { ("", "") };
    let incl = if weekly { " (incl. this week)" } else { " (prior bars)" };
    let pos = p.position.map(|v| format!(" · pos {}", pct(v))).unwrap_or_else(|| " · flat window".into());
    let trigger = format!(" · trigger {}", weekday_date(p.trigger_ts));
    let c = p.close;
    match p.kind {
        Some(PatternKind::Breakout) => format!("C {c:.2} ≥ {hi_f}H{long} {h:.2}{incl}{pos}{trigger}"),
        Some(PatternKind::Breakdown) => format!("C {c:.2} ≤ {lo_f}L{long} {l:.2}{incl}{pos}{trigger}"),
        Some(PatternKind::FromBottom) => {
            format!("C {c:.2} ≥ {hi_f}H{short} {hs:.2}{incl}{pos} ≤ 33.3%{trigger}")
        }
        Some(PatternKind::FromTop) => {
            format!("C {c:.2} ≤ {lo_f}L{short} {ls:.2}{incl}{pos} ≥ 66.7%{trigger}")
        }
        None => format!("no pattern · C {c:.2} · H{long} {h:.2} · L{long} {l:.2}{pos}{trigger}"),
    }
}

fn txt(p: &Painter, r: Rect, s: impl ToString, color: Color32, right: bool) {
    let (x, align) = if right { (r.right() - 4.0, Align2::RIGHT_CENTER) } else { (r.left() + 4.0, Align2::LEFT_CENTER) };
    p.text(pos2(x, r.center().y), align, s.to_string(), theme::mono(11.0), color);
}
fn badge(p: &Painter, r: Rect, label: &str, (fill, ink): (Color32, Color32)) {
    let b = r.shrink2(vec2(2.0, 3.0));
    p.rect_filled(b, 3.0, fill);
    p.text(b.center(), Align2::CENTER_CENTER, label, theme::mono(10.5), ink);
}
fn signed(v: f64, prec: usize) -> (String, Color32) {
    (format!("{v:+.prec$}"), if v >= 0.0 { theme::up() } else { theme::down() })
}
fn dash(p: &Painter, r: Rect) {
    txt(p, r, "—", theme::muted(), false);
}

fn paint_pattern(p: &Painter, r: Rect, read: &Result<PatternRead, Unavailable>) {
    match read {
        Ok(PatternRead { kind: Some(k), .. }) => badge(p, r, kind_label(*k), kind_fill(*k)),
        Ok(_) => txt(p, r, "·", theme::muted(), false),
        Err(_) => dash(p, r),
    }
}
fn paint_pos(p: &Painter, r: Rect, read: Option<&PatternRead>, scale: &HeatScale) {
    match read.and_then(|x| x.position) {
        Some(v) => {
            let fill = scale.fill(100.0 * v - 50.0);
            p.rect_filled(r.shrink2(vec2(1.0, 2.0)), 2.0, fill);
            txt(p, r, format!("{:.1}", 100.0 * v), theme::on_fill(fill), true);
        }
        None => dash(p, r),
    }
}
fn paint_state(p: &Painter, r: Rect, c: Option<&Result<RsiCounter, Unavailable>>, flat_chip: bool) {
    match c {
        Some(Ok(c)) => {
            let badge_w = if flat_chip { r.width() - 34.0 } else { r.width() };
            badge(p, Rect::from_min_size(r.min, vec2(badge_w, r.height())), c.state.label(), state_fill(c.state));
            if flat_chip && c.flat {
                p.text(pos2(r.right() - 3.0, r.center().y), Align2::RIGHT_CENTER, "FLAT", theme::mono(9.0), theme::amber());
            }
        }
        _ => dash(p, r),
    }
}

/// Paint one data row; returns nothing (hit-testing happens on the row response).
fn paint_row(
    p: &Painter,
    rect: Rect,
    view: ScanView,
    r: &EngineRow,
    params: (usize, usize),
    older: bool,
    (rs, ps): (&HeatScale, &HeatScale),
) {
    let mut x = rect.left();
    let weekly: Option<&WeeklyRead> = r.weekly.as_ref().ok();
    let dp = r.pattern_d.as_ref().ok();
    let wp = weekly.and_then(|w| w.pattern.as_ref().ok());
    let _ = params;
    for d in cols(view) {
        let cell = Rect::from_min_size(pos2(x, rect.top()), vec2(d.w, rect.height()));
        x += d.w;
        match d.col {
            Col::Ticker => txt(p, cell, tr_data::security(&r.symbol).ticker, theme::text(), false),
            Col::Last => txt(p, cell, &r.last_date, if older { theme::amber() } else { theme::muted() }, false),
            Col::Close => txt(p, cell, format!("{:.2}", r.close), theme::text(), true),
            Col::DPattern => paint_pattern(p, cell, &r.pattern_d),
            Col::DPos => paint_pos(p, cell, dp, ps),
            Col::VsH => match dp {
                Some(x) => {
                    let (s, c) = signed((r.close / x.hi_long - 1.0) * 100.0, 1);
                    txt(p, cell, s, c, true)
                }
                None => dash(p, cell),
            },
            Col::VsL => match dp {
                Some(x) => {
                    let (s, c) = signed((r.close / x.lo_long - 1.0) * 100.0, 1);
                    txt(p, cell, s, c, true)
                }
                None => dash(p, cell),
            },
            Col::WPattern => match weekly {
                Some(w) => paint_pattern(p, cell, &w.pattern),
                None => dash(p, cell),
            },
            Col::WPos => paint_pos(p, cell, wp, &ps),
            Col::WWks => match wp {
                Some(x) => txt(p, cell, x.bars_used, if x.full_year { theme::text() } else { theme::amber() }, true),
                None => dash(p, cell),
            },
            Col::WWeek => match weekly {
                Some(w) => txt(p, cell, date(w.week_start), theme::muted(), false),
                None => dash(p, cell),
            },
            Col::DRsic => paint_state(p, cell, Some(&r.rsic_d), false),
            Col::DState => paint_state(p, cell, Some(&r.rsic_d), true),
            Col::WState => paint_state(p, cell, weekly.map(|w| &w.rsic), false),
            Col::DMean | Col::WMean => {
                let c = if d.col == Col::DMean { r.rsic_d.as_ref().ok() } else { weekly.and_then(|w| w.rsic.as_ref().ok()) };
                match c {
                    Some(c) => {
                        let fill = rs.fill(c.mean - 50.0);
                        p.rect_filled(cell.shrink2(vec2(1.0, 2.0)), 2.0, fill);
                        txt(p, cell, format!("{:.2}", c.mean), theme::on_fill(fill), true);
                    }
                    None => dash(p, cell),
                }
            }
            Col::DAlign | Col::WAlign => {
                let c = if d.col == Col::DAlign { r.rsic_d.as_ref().ok() } else { weekly.and_then(|w| w.rsic.as_ref().ok()) };
                match c {
                    Some(c) => txt(p, cell, format!("{:.3}", c.align), theme::text(), true),
                    None => dash(p, cell),
                }
            }
            Col::DOs | Col::DOb | Col::DUp | Col::DDown => match r.rsic_d.as_ref().ok() {
                Some(c) => {
                    let n = match d.col {
                        Col::DOs => c.os20,
                        Col::DOb => c.ob80,
                        Col::DUp => c.up,
                        _ => c.down,
                    };
                    txt(p, cell, pct(c.share(n)), if n == 0 { theme::muted() } else { theme::text() }, true)
                }
                None => dash(p, cell),
            },
            Col::Strip => match r.rsic_d.as_ref().ok() {
                Some(c) => {
                    for (i, v) in c.values.iter().enumerate() {
                        let cr = Rect::from_min_size(pos2(cell.left() + 4.0 + i as f32 * STRIP_CELL, cell.top() + 4.0), vec2(STRIP_CELL - 1.0, ROW_H - 8.0));
                        p.rect_filled(cr, 1.0, v.map_or_else(theme::control, |v| rs.fill(v - 50.0)));
                    }
                }
                None => dash(p, cell),
            },
            Col::Dw => match r.agreement {
                Some(Agreement::Up) => txt(p, cell, "D+W ↑", theme::up(), false),
                Some(Agreement::Down) => txt(p, cell, "D+W ↓", theme::down(), false),
                None if r.pullback => txt(p, cell, "PULLBACK", theme::amber(), false),
                None => txt(p, cell, "·", theme::muted(), false),
            },
            Col::Accel => match r.accel {
                Some(a) => {
                    let (s, c) = signed(a, 2);
                    txt(p, cell, s, c, true)
                }
                None => dash(p, cell),
            },
        }
    }
}

fn col_title(d: &ColDef, params: (usize, usize)) -> String {
    if d.col == Col::Accel { format!("Δ{}/{}", params.0, params.1) } else { d.title.to_string() }
}

/// Tooltip under the pointer at horizontal offset `x` within the row.
fn tip_at(view: ScanView, r: &EngineRow, x: f32) -> Option<String> {
    let d = col_at(view, x)?;
    let start: f32 = cols(view).iter().take_while(|c| c.col != d.col).map(|c| c.w).sum();
    match d.col {
        Col::DPattern => r.pattern_d.as_ref().ok().map(|p| pattern_tip(p, false)),
        Col::WPattern => r.weekly.as_ref().ok()?.pattern.as_ref().ok().map(|p| pattern_tip(p, true)),
        Col::Strip => {
            let i = ((x - start - 4.0) / STRIP_CELL).floor();
            if !(0.0..20.0).contains(&i) {
                return None;
            }
            let i = i as usize;
            let c = r.rsic_d.as_ref().ok()?;
            Some(match c.values[i] {
                Some(v) => format!("RSI({}) = {v:.2} · reference EWM", i + 2),
                None => format!("RSI({}) unavailable on this window", i + 2),
            })
        }
        _ => None,
    }
}

fn chip(ui: &mut Ui, label: &str, n: usize, tint: Color32, selected: bool) -> egui::Response {
    let ink = theme::text();
    let l = ui.painter().layout_no_wrap(label.to_string(), theme::mono(10.0), ink);
    let g = ui.painter().layout_no_wrap(n.to_string(), theme::mono(15.0), ink);
    let size = vec2(l.size().x + g.size().x + 20.0, 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    p.rect_filled(rect, 4.0, if resp.hovered() { tint.gamma_multiply(1.3) } else { tint });
    if selected {
        p.rect_stroke(rect, 4.0, Stroke::new(2.0, theme::text()), egui::StrokeKind::Inside);
    }
    p.galley(pos2(rect.left() + 7.0, rect.center().y - l.size().y / 2.0), l.clone(), ink);
    p.galley(pos2(rect.left() + 13.0 + l.size().x, rect.center().y - g.size().y / 2.0), g, ink);
    resp
}

impl ResearchPane {
    pub(super) fn view_symbols(&self) -> Option<Vec<&str>> {
        self.view.as_ref().map(ScanViewPane::symbols)
    }
    pub(super) fn view_navigate(&mut self, key: Key) -> Option<Option<Action>> {
        let view = self.view.as_mut()?;
        match key {
            Key::ArrowLeft => {
                view.side = Side::D;
                Some(None)
            }
            Key::ArrowRight => {
                view.side = Side::W;
                Some(None)
            }
            Key::Enter => {
                let side = view.side;
                Some(view.open(self.selected, side))
            }
            _ => None,
        }
    }
    /// Accept a worker snapshot: verified provenance only; a refused snapshot keeps the old board.
    pub fn accept_view(&mut self, result: Result<Arc<ViewSnapshot>, String>) {
        self.loading = false;
        let selected = self.visible_symbols().get(self.selected).map(|s| (*s).to_owned());
        self.error = None;
        let query = self.search.clone();
        let Some(view) = self.view.as_mut() else { return };
        match result.and_then(|s| {
            s.provenance.require_verified("SCAN VIEW")?;
            Ok(s)
        }) {
            Ok(snapshot) => {
                view.snapshot = Some(snapshot);
                view.recount();
                view.refilter(&query);
                self.restore_selection(selected);
            }
            Err(e) => {
                if e.starts_with(SAMPLE_REFUSAL) {
                    // Sample mode: no digits painted, so drop the previous board.
                    view.snapshot = None;
                    view.order.clear();
                    view.counts = Default::default();
                }
                self.error = Some(e);
            }
        }
    }
    pub(super) fn show_view(&mut self, ui: &mut Ui) -> (Vec<Action>, bool) {
        let mut actions = vec![];
        let mut changed = false;
        let mut recount = false;
        let refresh = ui
            .horizontal(|ui| {
                let refresh = ui.add_enabled(!self.loading, egui::Button::new("Refresh")).clicked();
                ui.label("Filter");
                if ui.text_edit_singleline(&mut self.search).changed() {
                    changed = true;
                }
                if let Some(v) = self.view.as_mut() {
                    if ui.checkbox(&mut v.include_older, "Include older-dated").changed() {
                        recount = true;
                    }
                    if v.view() == Some(ScanView::Pattern) {
                        changed |= ui.selectable_value(&mut v.hits_only, true, "Hits only").changed();
                        changed |= ui.selectable_value(&mut v.hits_only, false, "All rows").changed();
                    }
                }
                refresh
            })
            .inner;
        if recount || changed {
            let q = self.search.clone();
            if let Some(v) = self.view.as_mut() {
                if recount {
                    v.recount();
                }
                v.refilter(&q);
            }
            self.selected = 0;
            self.scroll_selected = true;
        }
        if let Some(Err(e)) = self.view.as_ref().map(|v| &v.spec) {
            ui.colored_label(theme::down(), e.clone());
            return (actions, refresh);
        }
        ui.label("↑/↓ j/k · PgUp/PgDn Home/End · ←/→ D/W side · Enter: open trigger bar · click header: sort · w/W: save to group");
        if self.loading {
            ui.spinner();
            ui.label("Reading research archive…");
        }
        if let Some(error) = &self.error {
            if error.starts_with("SCAN VIEW unavailable") {
                ui.group(|ui| {
                    ui.colored_label(theme::amber(), error.clone());
                });
            } else {
                ui.colored_label(theme::down(), format!("Refresh failed: {error}"));
            }
        }
        let Some(view) = self.view.as_mut() else { return (actions, refresh) };
        let (Some(snap), Ok(spec)) = (view.snapshot.clone(), view.spec.clone()) else { return (actions, refresh) };
        let board = &*snap.board;
        let kind = spec.view;
        let params = (spec.params.rsi_n, spec.params.lag);

        // Header line + staleness banner (the only wall-clock read, at paint time).
        let day = board.as_of_ts.map_or_else(|| "—".into(), date);
        let week = board.as_of_week.map_or_else(|| "—".into(), date);
        ui.label(
            egui::RichText::new(format!(
                "[{}] {} · D as of {day} · W week of {week} · {} rows · {} short · {} invalid · {} older-dated hidden",
                snap.provenance.label(),
                snap.source,
                board.rows.len(),
                board.short.len(),
                board.invalid.len() + snap.skipped.len(),
                if view.include_older { 0 } else { view.counts.older },
            ))
            .strong(),
        );
        if let Some(ts) = board.as_of_ts
            && chrono::DateTime::from_timestamp(ts, 0).is_some_and(|t| {
                t.date_naive() < tr_data::history::calendar::last_expected_session(chrono::Utc::now())
            })
        {
            ui.colored_label(theme::amber(), "Archive not refreshed (HIST REFRESH)");
        }
        // Bucket strip.
        let mut toggle = None;
        ui.push_id("scan-view-buckets", |ui| {
            ui.horizontal_wrapped(|ui| {
                let mut last_group = "";
                for bucket in view.buckets() {
                    let group = match (kind, bucket) {
                        (ScanView::Pattern, Bucket::DPat(_)) => "DAILY 63/21",
                        (ScanView::Pattern, Bucket::WPat(_)) => "WEEKLY 52/26 ±0.1%",
                        (_, Bucket::D(_)) => "DAILY",
                        (_, Bucket::W(_)) => "WEEKLY",
                        _ => "D / W",
                    };
                    if group != last_group {
                        ui.label(egui::RichText::new(group).strong().color(theme::muted()));
                        last_group = group;
                    }
                    if chip(ui, &bucket.label(), view.counts.get(bucket), bucket.tint(), view.bucket == Some(bucket)).clicked() {
                        toggle = Some(bucket);
                    }
                }
            });
        });
        if let Some(b) = toggle {
            view.toggle_bucket(b);
            self.selected = 0;
            self.scroll_selected = true;
        }
        let view = self.view.as_mut().expect("view");
        if !board.short.is_empty() || !board.invalid.is_empty() || !snap.skipped.is_empty() {
            ui.collapsing("Short / invalid names", |ui| {
                egui::ScrollArea::vertical().id_salt("scan-view-skipped").max_height(140.0).show(ui, |ui| {
                    for s in board.short.iter().chain(&board.invalid).chain(&snap.skipped) {
                        ui.label(format!("{} · {}", s.symbol, s.reason));
                    }
                });
            });
        }
        match kind {
            ScanView::RsiCounter => rsi_scale().key(ui),
            ScanView::Pattern => pos_scale().key(ui),
        }
        self.selected = self.selected.min(view.order.len().saturating_sub(1));
        if let Some(row) = view.row_at(self.selected) {
            let mut line = format!("{} · {} ", tr_data::security(&row.symbol).ticker, row.last_date);
            if let Ok(c) = &row.rsic_d {
                line += &format!(
                    "D: {} lengths · <20 {} · >80 {} · ↑ {} · ↓ {} · mean {:.2} · sd {:.2} · align {:.3}",
                    c.valid,
                    pct(c.share(c.os20)),
                    pct(c.share(c.ob80)),
                    pct(c.share(c.up)),
                    pct(c.share(c.down)),
                    c.mean,
                    c.sd,
                    c.align
                );
            }
            ui.label(egui::RichText::new(line).monospace());
            ui.label(egui::RichText::new(INSPECTOR).font(theme::mono(10.0)).color(theme::muted()));
        }
        if let Some(n) = &view.notice {
            ui.colored_label(theme::amber(), n.clone());
        }
        if view.order.is_empty() && !self.loading {
            ui.label("No matching rows.");
        }

        // Table: header sorts on click, rows are painted directly (no per-cell widgets).
        let total_w: f32 = cols(kind).iter().map(|d| d.w).sum();
        let height = ui.available_height();
        let mut sort_click = None;
        let mut clicked_row = None;
        let n = view.order.len();
        let selected = self.selected;
        let mut scroll = egui::ScrollArea::vertical().id_salt("scan-view-table").max_height(height).auto_shrink([false, false]);
        if std::mem::take(&mut self.scroll_selected) {
            scroll = scroll.vertical_scroll_offset(selected as f32 * ROW_H);
        }
        self.page_rows = (height / ROW_H).floor().max(1.0) as usize;
        egui::ScrollArea::horizontal().id_salt("scan-view-hscroll").show(ui, |ui| {
            let (hrect, hresp) = ui.allocate_exact_size(vec2(total_w, ROW_H), Sense::click());
            let hp = ui.painter();
            hp.rect_filled(hrect, 0.0, theme::control());
            let mut x = hrect.left();
            for d in cols(kind) {
                let arrow = if view.sort.0 == d.col { if view.sort.1 { " ▼" } else { " ▲" } } else { "" };
                let cell = Rect::from_min_size(pos2(x, hrect.top()), vec2(d.w, ROW_H));
                txt(hp, cell, format!("{}{arrow}", col_title(d, params)), if view.sort.0 == d.col { theme::text() } else { theme::muted() }, false);
                x += d.w;
            }
            if hresp.clicked()
                && let Some(pos) = hresp.interact_pointer_pos()
                && let Some(d) = col_at(kind, pos.x - hrect.left())
            {
                sort_click = Some(d.col);
            }
            let pointer = ui.input(|i| i.pointer.hover_pos());
            let (rsi_sc, pos_sc) = (rsi_scale(), pos_scale());
            scroll.show_rows(ui, ROW_H, n, |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                #[cfg(test)]
                let first = range.start;
                for index in range {
                    let Some(row) = view.row_at(index) else { continue };
                    let (rect, resp) = ui.allocate_exact_size(vec2(total_w, ROW_H), Sense::click());
                    let p = ui.painter();
                    if index == selected {
                        p.rect_filled(rect, 0.0, theme::accent_wash());
                    } else if resp.hovered() {
                        p.rect_filled(rect, 0.0, theme::control_hover());
                    }
                    #[cfg(test)]
                    if index == first {
                        // Perf gate probe: the 5th heat-strip cell (RSI length 7) of the first visible row.
                        let sx: f32 = cols(kind).iter().take_while(|c| c.col != Col::Strip).map(|c| c.w).sum();
                        let cell = Rect::from_min_size(
                            pos2(rect.left() + sx + 4.0 + 5.0 * STRIP_CELL, rect.top()),
                            vec2(STRIP_CELL, ROW_H),
                        );
                        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("scan-view-probe"), cell));
                    }
                    let older = board.as_of_ts.is_some_and(|t| row.last_ts != t);
                    paint_row(p, rect, kind, row, params, older, (&rsi_sc, &pos_sc));
                    if resp.clicked()
                        && let Some(pos) = resp.interact_pointer_pos()
                    {
                        clicked_row = Some((index, col_at(kind, pos.x - rect.left()).and_then(|d| d.side)));
                    }
                    if resp.hovered()
                        && let Some(pos) = pointer
                        && let Some(tip) = tip_at(kind, row, pos.x - rect.left())
                    {
                        resp.on_hover_ui_at_pointer(|ui| {
                            ui.label(egui::RichText::new(tip).monospace());
                        });
                    }
                }
            });
        });
        if let Some(col) = sort_click {
            view.set_sort(col);
            self.selected = 0;
            self.scroll_selected = true;
        }
        if let Some((index, side)) = clicked_row {
            self.selected = index;
            let side = side.unwrap_or(view.side);
            view.side = side;
            if let Some(a) = view.open(index, side) {
                actions.push(a);
            }
        }
        (actions, refresh)
    }
}
