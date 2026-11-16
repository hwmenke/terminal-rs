//! UI-only timing scopes; disabled scopes never read the clock or allocate.
use egui::{Color32, RichText};
use std::{
    cell::Cell,
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
    time::{Duration, Instant},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Layer {
    Drain,
    Chrome,
    Dock,
    Windows,
    Hud,
    ChartTotal,
    Input,
    Scene,
    GridAxes,
    ZBand,
    Stack,
    Rail,
    Stops,
    Auction,
    Levels,
    Drawings,
    Avwap,
    Cone,
    Markers,
    Link,
    Crosshair,
    Legend,
    Edge,
}
impl Layer {
    pub const ALL: [Self; 23] = [
        Self::Drain,
        Self::Chrome,
        Self::Dock,
        Self::Windows,
        Self::Hud,
        Self::ChartTotal,
        Self::Input,
        Self::Scene,
        Self::GridAxes,
        Self::ZBand,
        Self::Stack,
        Self::Rail,
        Self::Stops,
        Self::Auction,
        Self::Levels,
        Self::Drawings,
        Self::Avwap,
        Self::Cone,
        Self::Markers,
        Self::Link,
        Self::Crosshair,
        Self::Legend,
        Self::Edge,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Drain => "drain",
            Self::Chrome => "chrome",
            Self::Dock => "dock",
            Self::Windows => "windows",
            Self::Hud => "hud",
            Self::ChartTotal => "chart total",
            Self::Input => "input",
            Self::Scene => "scene",
            Self::GridAxes => "grid/axes",
            Self::ZBand => "zband",
            Self::Stack => "stack",
            Self::Rail => "rail",
            Self::Stops => "stops",
            Self::Auction => "auction",
            Self::Levels => "levels",
            Self::Drawings => "drawings",
            Self::Avwap => "avwap",
            Self::Cone => "cone",
            Self::Markers => "markers",
            Self::Link => "link",
            Self::Crosshair => "crosshair",
            Self::Legend => "legend",
            Self::Edge => "edge",
        }
    }
    pub fn budget_ms(self) -> f32 {
        match self {
            Self::Drain => 0.5,
            Self::Chrome => 1.5,
            Self::Dock => 6.0,
            Self::Windows => 1.5,
            Self::Hud => 0.3,
            // Slice 1: worker session summaries restore the original frame targets.
            Self::ChartTotal => 2.0,
            Self::Scene | Self::Auction => 0.3,
            Self::GridAxes | Self::Stops | Self::Avwap | Self::Cone => 0.25,
            Self::ZBand | Self::Markers => 0.15,
            Self::Stack => 0.4,
            Self::Legend => 0.4,
            Self::Levels => 0.1,
            Self::Edge => 0.2,
            _ => 0.1,
        }
    }
}
thread_local! {static ENABLED:Cell<bool>=const {Cell::new(false)};static SUMS:Cell<[f32;23]>=const {Cell::new([0.0;23])};}
pub struct Scope(Option<(Layer, Instant)>);
impl Drop for Scope {
    fn drop(&mut self) {
        if let Some((l, t)) = self.0 {
            SUMS.with(|s| {
                let mut v = s.get();
                v[l as usize] += t.elapsed().as_secs_f32() * 1000.0;
                s.set(v);
            });
        }
    }
}
pub fn scope(l: Layer) -> Scope {
    Scope(enabled().then(|| (l, Instant::now())))
}
pub fn enabled() -> bool {
    ENABLED.with(Cell::get)
}
pub fn set_enabled(on: bool) {
    ENABLED.with(|v| v.set(on));
    if !on {
        end_frame();
    }
}
pub fn end_frame() -> [f32; 23] {
    SUMS.with(|v| v.replace([0.0; 23]))
}
#[derive(Clone)]
pub struct Meter {
    ema: f32,
    ring: [f32; 120],
    len: usize,
    idx: usize,
}
impl Default for Meter {
    fn default() -> Self {
        Self { ema: 0.0, ring: [0.0; 120], len: 0, idx: 0 }
    }
}
impl Meter {
    pub fn push(&mut self, v: f32) {
        self.ema = if self.len == 0 { v } else { 0.9 * self.ema + 0.1 * v };
        self.ring[self.idx] = v;
        self.idx = (self.idx + 1) % 120;
        self.len = (self.len + 1).min(120);
    }
    pub fn ema(&self) -> f32 {
        self.ema
    }
    pub fn p95(&self) -> f32 {
        if self.len == 0 {
            return 0.0;
        }
        let mut a = self.ring;
        a[..self.len].sort_unstable_by(f32::total_cmp);
        a[(self.len * 95).div_ceil(100) - 1]
    }
    pub fn max(&self) -> f32 {
        self.ring[..self.len].iter().copied().fold(0.0, f32::max)
    }
}
#[derive(Default)]
pub struct FrameProfile {
    pub layers: [Meter; 23],
    pub ui: Meter,
    pub dt: Meter,
}
impl FrameProfile {
    pub fn push(&mut self, sums: [f32; 23], ui: f32, dt: f32) {
        for (m, v) in self.layers.iter_mut().zip(sums) {
            m.push(v);
        }
        self.ui.push(ui);
        self.dt.push(dt);
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    Wall,
    Cpu,
}
struct JobHistory {
    count: u64,
    samples: VecDeque<f32>,
    ema: f32,
    max: f32,
    at: Instant,
}
#[derive(Default)]
pub struct JobLog(Mutex<BTreeMap<(&'static str, Phase), JobHistory>>);
pub struct JobRow {
    pub kind: &'static str,
    pub phase: Phase,
    pub count: u64,
    pub last: f32,
    pub ema: f32,
    pub p95: f32,
    pub max: f32,
    pub age: Duration,
}
/// A worker CPU guard also records early returns and unwinding.
pub struct JobTimer<'a> {
    log: &'a JobLog,
    kind: &'static str,
    start: Instant,
}
impl<'a> JobTimer<'a> {
    pub fn new(log: &'a JobLog, kind: &'static str) -> Self {
        Self { log, kind, start: Instant::now() }
    }
}
impl Drop for JobTimer<'_> {
    fn drop(&mut self) {
        self.log.record(self.kind, Phase::Cpu, self.start.elapsed());
    }
}
impl JobLog {
    pub fn record(&self, kind: &'static str, phase: Phase, duration: Duration) {
        let v = duration.as_secs_f32() * 1000.0;
        let Ok(mut log) = self.0.lock() else { return };
        let h = log.entry((kind, phase)).or_insert_with(|| JobHistory {
            count: 0,
            samples: VecDeque::new(),
            ema: v,
            max: 0.0,
            at: Instant::now(),
        });
        h.count += 1;
        h.ema = 0.9 * h.ema + 0.1 * v;
        h.max = h.max.max(v);
        h.at = Instant::now();
        if h.samples.len() == 64 {
            h.samples.pop_front();
        }
        h.samples.push_back(v);
    }
    pub fn time<T>(&self, kind: &'static str, f: impl FnOnce() -> T) -> T {
        let t = Instant::now();
        let value = f();
        self.record(kind, Phase::Cpu, t.elapsed());
        value
    }
    pub fn snapshot(&self) -> Vec<JobRow> {
        let Ok(log) = self.0.lock() else { return vec![] };
        log.iter()
            .map(|(&(kind, phase), h)| {
                let mut a = h.samples.iter().copied().collect::<Vec<_>>();
                a.sort_unstable_by(f32::total_cmp);
                JobRow {
                    kind,
                    phase,
                    count: h.count,
                    last: *h.samples.back().unwrap(),
                    ema: h.ema,
                    p95: a[(a.len() * 95).div_ceil(100) - 1],
                    max: h.max,
                    age: h.at.elapsed(),
                }
            })
            .collect()
    }
}
pub fn hud(ctx: &egui::Context, anchor: egui::Rect, p: &FrameProfile, jobs: &[JobRow]) {
    if !enabled() {
        return;
    }
    let _s = scope(Layer::Hud);
    egui::Area::new(egui::Id::new("frame-profile-hud"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, anchor.top() - ctx.content_rect().bottom() - 6.0))
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(crate::theme::card())
                .stroke(egui::Stroke::new(1.0, crate::theme::border()))
                .inner_margin(8.0)
                .show(ui, |ui| {
                    let row = |ui: &mut egui::Ui, text: String, over: bool| {
                        ui.label(RichText::new(text).monospace().size(10.5).color(if over {
                            crate::theme::down()
                        } else {
                            Color32::LIGHT_GRAY
                        }));
                    };
                    row(
                        ui,
                        format!(
                            "FRAME ui {:.2} p95 {:.2} max {:.2} /8 ms · dt {:.1} · {:.0} fps",
                            p.ui.ema(),
                            p.ui.p95(),
                            p.ui.max(),
                            p.dt.ema(),
                            1000.0 / p.dt.ema().max(0.01)
                        ),
                        p.ui.p95() > 8.0,
                    );
                    row(ui, "LAYER (sum across panes)   ema    p95  budget ms".into(), false);
                    for l in Layer::ALL {
                        let m = &p.layers[l as usize];
                        if m.ema() >= 0.005 {
                            row(
                                ui,
                                format!("{:23} {:6.2} {:6.2} {:6.2}", l.label(), m.ema(), m.p95(), l.budget_ms()),
                                m.p95() > l.budget_ms(),
                            );
                        }
                    }
                    let child: f32 = Layer::ALL.iter().skip(6).map(|l| p.layers[*l as usize].ema()).sum();
                    row(
                        ui,
                        format!(
                            "chart other              {:6.2}",
                            (p.layers[Layer::ChartTotal as usize].ema() - child).max(0.0)
                        ),
                        false,
                    );
                    row(ui, "dt − ui includes scheduling, tessellation, GPU/present; no GPU timer".into(), false);
                    row(ui, "WORKER (last 60 s)     n    last    ema    p95    max ms".into(), false);
                    for j in jobs.iter().filter(|j| j.age.as_secs() < 60) {
                        row(
                            ui,
                            format!(
                                "{} {:16} {:4} {:6.1} {:6.1} {:6.1} {:6.1}",
                                if j.phase == Phase::Cpu { "cpu" } else { "wall" },
                                j.kind,
                                j.count,
                                j.last,
                                j.ema,
                                j.p95,
                                j.max
                            ),
                            false,
                        );
                    }
                });
        });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn meter_ema_and_p95() {
        let mut m = Meter::default();
        for i in 1..=100 {
            m.push(i as f32);
        }
        assert_eq!(m.p95(), 95.);
        assert_eq!(m.max(), 100.);
        assert!((m.ema() - 91.).abs() < 0.1);
        for _ in 0..120 {
            m.push(1.);
        }
        assert_eq!(m.p95(), 1.);
    }
    #[test]
    fn scope_is_inert_when_disabled() {
        set_enabled(false);
        {
            let _s = scope(Layer::Scene);
        }
        assert_eq!(end_frame(), [0.; 23]);
    }
    #[test]
    fn end_frame_returns_and_resets() {
        set_enabled(true);
        {
            let _s = scope(Layer::Scene);
            std::hint::black_box((0..1000).sum::<u64>());
        }
        assert!(end_frame()[Layer::Scene as usize] > 0.);
        assert_eq!(end_frame(), [0.; 23]);
        set_enabled(false);
    }
    #[test]
    fn job_log_caps_history_and_reports_p95() {
        let log = JobLog::default();
        for i in 1..=100 {
            log.record("cone", Phase::Cpu, Duration::from_millis(i));
        }
        let r = log.snapshot();
        assert_eq!(r[0].count, 100);
        assert_eq!(r[0].last, 100.);
        assert_eq!(r[0].p95, 97.);
        assert_eq!(log.0.lock().unwrap().values().next().unwrap().samples.len(), 64);
    }
    #[test]
    fn hud_paints_layer_and_worker_rows_only_when_on() {
        fn texts(s: &egui::Shape) -> String {
            match s {
                egui::Shape::Text(t) => t.galley.job.text.clone(),
                egui::Shape::Vec(v) => v.iter().map(texts).collect::<Vec<_>>().join(" "),
                _ => String::new(),
            }
        }
        let ctx = egui::Context::default();
        let mut p = FrameProfile::default();
        p.push([0.1; 23], 1., 16.7);
        let jobs = JobLog::default();
        jobs.record("cone", Phase::Cpu, Duration::from_millis(10));
        for on in [false, true] {
            set_enabled(on);
            let mut text = String::new();
            for _ in 0..2 {
                let mut out = ctx.run_ui(Default::default(), |ui| hud(ui.ctx(), ui.max_rect(), &p, &jobs.snapshot()));
                out.textures_delta.clear();
                text = out.shapes.iter().map(|s| texts(&s.shape)).collect::<Vec<_>>().join(" ");
            }
            assert_eq!(text.contains("LAYER"), on);
            assert_eq!(text.contains("cpu cone"), on);
        }
        set_enabled(false);
    }
}
