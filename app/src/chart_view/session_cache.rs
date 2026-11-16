//! Per-pane, nonpersistent worker snapshot. No history scans in request/paint.
use super::*;
use crate::widgets::{MathCard, MathCardExt};
use tr_data::chart_session::SessionSummary;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionSummaryKey {
    pub generation: u64,
    pub bars_revision: u64,
    pub stitch_revision: u64,
    pub tf: Timeframe,
    pub extended: bool,
    pub as_of: Option<i64>,
}
#[derive(Default)]
pub(super) struct SessionCache {
    pub bars_revision: u64,
    pub stitch_revision: u64,
    desired: Option<SessionSummaryKey>,
    pending: Option<SessionSummaryKey>,
    sent_at: Option<Instant>,
    accepted: Option<SessionSummaryKey>,
    pub snapshot: Option<Arc<SessionSummary>>,
    // Keep the exact coverage receipt that produced the snapshot's index map.
    pub stitch: Option<Arc<tr_data::vault::StitchInfo>>,
    pub lines: Vec<(String, usize)>,
    pub cards: Vec<MathCard>,
    pub status: String,
    error: Option<String>,
}
fn stamp(ts: Option<i64>) -> String {
    ts.map(|t| {
        chrono::DateTime::from_timestamp(t, 0)
            .map(|d| d.format("%Y-%m-%d %H:%M:%S UTC").to_string())
            .unwrap_or_else(|| "unavailable".into())
    })
    .unwrap_or_else(|| "unavailable".into())
}
impl ChartState {
    pub fn invalidate_session_summary(&mut self) {
        // Retain the physical in-flight slot across identity changes; only its
        // matching completion can release it. Never run two jobs for one pane.
        let c = &mut self.session;
        c.desired = None;
        c.accepted = None;
        c.snapshot = None;
        c.stitch = None;
        c.lines.clear();
        c.cards.clear();
        c.status.clear();
        c.error = None;
    }
    pub(super) fn session_key(&self) -> SessionSummaryKey {
        SessionSummaryKey {
            generation: self.generation,
            bars_revision: self.session.bars_revision,
            stitch_revision: self.session.stitch_revision,
            tf: self.tf,
            extended: self.extended,
            as_of: self.last_refresh,
        }
    }
    pub(super) fn request_session_summary(&mut self, w: &Worker) -> Option<Duration> {
        if self.bars.is_empty() || !self.tf.is_intraday() || !tr_data::time_link::cash_class(self.sec.class) {
            self.invalidate_session_summary();
            return None;
        }
        if !self.provenance.is_verified() {
            self.invalidate_session_summary();
            self.session.status = "SESSION · NO REAL DATA · unverified input".into();
            return None;
        }
        let key = self.session_key();
        let c = &mut self.session;
        let changed = c.desired != Some(key);
        c.desired = Some(key);
        if changed
            && c.accepted != Some(key)
            && let Some(old) = &c.snapshot
        {
            c.status = format!(
                "PREVIOUS SNAPSHOT · observed {} · CLOSED THRU {}",
                stamp(old.observed_at),
                stamp(old.closed_thru)
            );
        }
        if c.accepted == Some(key) || c.pending.is_some() {
            return None;
        }
        if let Some(at) = c.sent_at {
            let wait = Duration::from_millis(300).saturating_sub(at.elapsed());
            if !wait.is_zero() {
                return Some(wait);
            }
        }
        w.send(Request::ChartSessionSummary {
            pane: self.id,
            key,
            bars: self.bars.clone(),
            class: self.sec.class,
            stitch: self.stitch.clone(),
        });
        c.pending = Some(key);
        c.sent_at = Some(Instant::now());
        if let Some(old) = &c.snapshot {
            c.status = format!(
                "PREVIOUS SNAPSHOT · observed {} · CLOSED THRU {}",
                stamp(old.observed_at),
                stamp(old.closed_thru)
            );
        } else {
            c.status = "SESSION · pending verified summary".into();
        }
        None
    }
    pub fn on_session_summary(&mut self, key: SessionSummaryKey, result: Result<Arc<SessionSummary>, String>) {
        if self.session.pending != Some(key) {
            return;
        }
        self.session.pending = None; // Also release obsolete work so the latest revision can run.
        if key != self.session_key() || self.session.desired != Some(key) {
            return;
        }
        self.session.accepted = Some(key);
        self.session.lines.clear();
        self.session.cards.clear();
        self.session.snapshot = None;
        self.session.stitch = None;
        match result {
            Err(reason) => {
                self.session.status = format!("SESSION · NO REAL DATA · {reason}");
                self.session.error = Some(reason);
            }
            Ok(r) => {
                self.session.error = None;
                self.session.status = format!(
                    "{} · {} · observed {} · CLOSED THRU {}{}",
                    r.provenance.label(),
                    r.source,
                    stamp(r.observed_at),
                    stamp(r.closed_thru),
                    if r.forming_included {
                        " · FORMING INCLUDED (descriptive input; RTH metrics exclude ETH)"
                    } else {
                        ""
                    }
                );
                let dec = self.sec.decimals(r.stats.vwap.unwrap_or(1.));
                let vwap = r.stats.vwap.map_or("—".into(), |v| format!("{v:.dec$}"));
                let rvol = r.stats.relative_volume.map_or("—".into(), |v| format!("{v:.2}×"));
                let mut line = format!("VWAP {vwap}  RVOL {rvol}");
                if let Some(poc) = r.stats.poc() {
                    let d = self.sec.decimals(poc);
                    line.push_str(&format!("  POC {poc:.d$}"));
                }
                if !r.stats.gaps.is_empty() {
                    let n = r.stats.gaps.len();
                    line.push_str(&format!("  {n} gap{}", if n == 1 { "" } else { "s" }));
                }
                let base = |title: &str, formula: &str| {
                    MathCard::new(title, formula)
                        .n(r.bars, "chart bars")
                        .span(stamp(Some(r.from)), stamp(Some(r.to)))
                        .closed_thru(stamp(r.closed_thru))
                        .source(&r.source)
                        .provenance(r.provenance)
                        .note(format!(
                            "Observed {} · {}",
                            stamp(r.observed_at),
                            if r.forming_included {
                                "input includes a forming print; RTH metrics exclude ETH"
                            } else {
                                "all input bars closed"
                            }
                        ))
                        .caveat("Descriptive session summary; no confidence interval or predictive significance.")
                };
                let card = base("Session VWAP / RVOL", "VWAP = Σ[volume × (H+L+C)/3] / Σvolume; RVOL = cumulative volume / historical same-clock mean")
                    .value(format!("VWAP {vwap} · RVOL {rvol}"))
                    .input("Σ HLC3 × volume", format!("{:.6}",r.vwap_pv))
                    .input("Σ volume", format!("{:.6}",r.vwap_volume))
                    .input("cumulative volume", r.rvol_volume.map_or("unavailable".into(),|v|format!("{v:.6}")))
                    .input("historical same-clock mean",r.rvol_mean.map_or("unavailable".into(),|v|format!("{v:.6}")))
                    .input("contributing prior sessions",r.rvol_sessions.len().to_string())
                    .n(r.session_bars,"latest cash-session bars")
                    .span(stamp(r.session_from),stamp(r.session_to))
                    .note(format!("RVOL sessions: {}",r.rvol_sessions.join(", ")))
                    .note("Missing VWAP means no finite positive-volume HLC3; missing RVOL means no positive current volume or positive historical same-clock mean. No filled prints.");
                self.session.cards.push(card);
                if r.fits {
                    self.session.lines.push((line, 0));
                }
                let mut card =
                    base("OR30", "ORH = max high; ORL = min low over every required opening 30-minute bucket");
                if let Some(o) = r.opening {
                    let d = self.sec.decimals(o.high).max(self.sec.decimals(o.low));
                    let text = format!("OR30  H {:.d$}  L {:.d$}", o.high, o.low);
                    card = card.value(&text).input("high / low", format!("{} / {}", o.high, o.low));
                    self.session.lines.push((text, 1));
                } else {
                    card = card.unavailable("Missing required opening window or incompatible timeframe");
                }
                self.session.cards.push(card);
                let mut card = base(
                    "Prior levels",
                    "PDH = max high; PDL = min low; PDC = last close of the complete prior cash session",
                );
                if let Some(p) = r.prior {
                    let d = self.sec.decimals(p.high).max(self.sec.decimals(p.low)).max(self.sec.decimals(p.close));
                    let text = format!("PDH {:.d$}  PDL {:.d$}  PDC {:.d$}", p.high, p.low, p.close);
                    card =
                        card.value(&text).input("high / low / close", format!("{} / {} / {}", p.high, p.low, p.close));
                    self.session.lines.push((text, 2));
                } else {
                    card = card.unavailable("Complete prior calendar session unavailable");
                }
                self.session.cards.push(card);
                let profile: Vec<_> =
                    r.stats.profile.iter().filter(|b| b.volume.is_finite() && b.volume > 0.).collect();
                if let Some(peak) = profile.iter().max_by(|a, b| a.volume.total_cmp(&b.volume)) {
                    let total: f64 = profile.iter().map(|b| b.volume).sum();
                    if let Some(share) = profile_share(peak.volume, total) {
                        let d = self.sec.decimals(peak.price);
                        self.session.lines.push((format!("VP {}  peak {:.d$}  {share}", profile.len(), peak.price), 3));
                    }
                }
                if let Some(v) = r.value_area {
                    let d = self.sec.decimals(v.high).max(self.sec.decimals(v.low));
                    if let Some(share) = profile_share(v.share, 1.) {
                        self.session.lines.push((format!("VAH {:.d$}  VAL {:.d$}  {share}", v.high, v.low), 4));
                    }
                }
                if let Some(e) = &r.extended_text {
                    self.session.lines.push((e.clone(), 5));
                }
                self.session.stitch = self.stitch.clone();
                self.session.snapshot = Some(r);
            }
        }
    }
    // Stale index maps must never be applied to a replaced/prepended bar series.
    pub(super) fn session_snapshot(&self) -> Option<&SessionSummary> {
        (self.session.accepted == Some(self.session_key())).then_some(self.session.snapshot.as_deref()).flatten()
    }
    pub(super) fn paint_session_lines(&self, ui: &Ui, rect: Rect, ly: &mut f32) {
        let p = ui.painter_at(rect);
        let c = &self.session;
        // Numeric previous snapshots are retained internally, but only the labelled
        // receipt is painted until current indices/numerics have been accepted.
        if self.session_snapshot().is_some() {
            for (line, kind) in &c.lines {
                if self.auction.enabled && matches!(kind, 3 | 4) {
                    continue;
                }
                let ink = match kind {
                    1 => theme::series(0),
                    2 | 5 => theme::info(),
                    3 => theme::series(2),
                    4 => theme::accent(),
                    _ => muted(),
                };
                let at = pos2(rect.left() + 6., *ly);
                let galley = p.layout_no_wrap(line.clone(), mono(11.), ink);
                let hit = Rect::from_min_size(at, galley.size()).intersect(rect);
                p.galley(at, galley, ink);
                if let Some(card) = c.cards.get(*kind).filter(|_| *kind < 3) {
                    ui.interact(hit, ui.id().with(("session-card", kind)), Sense::hover()).math_card(|| card.clone());
                }
                *ly += 14.;
            }
        }
        if !c.status.is_empty() {
            p.text(pos2(rect.left() + 6., *ly), Align2::LEFT_TOP, &c.status, mono(9.), muted());
            *ly += 14.;
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    pub(in crate::chart_view) fn deliver(c: &mut ChartState, r: Arc<SessionSummary>) {
        let key = c.session_key();
        c.session.desired = Some(key);
        c.session.pending = Some(key);
        c.on_session_summary(key, Ok(r));
    }
    pub(in crate::chart_view) fn accept(c: &mut ChartState) {
        c.provenance = Bar::provenance(&c.bars);
        let r =
            tr_data::chart_session::build(&c.bars, c.tf, c.sec.class, c.extended, c.last_refresh, c.stitch.as_deref())
                .unwrap();
        deliver(c, Arc::new(r));
    }
    fn fixture(w: &Worker) -> ChartState {
        let mut c = ChartState::new(1, "AAPL", Timeframe::M5);
        let bars = Arc::new(tr_perf::fixtures::intraday(Timeframe::M5, 3, 9));
        c.on_history(c.generation, bars.clone(), SeriesGpu::build(&bars), 0, false, true, false, w);
        c.last_refresh = Some(bars.last().unwrap().ts + 300);
        c
    }
    fn requests(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Request>) -> Vec<SessionSummaryKey> {
        let mut keys = vec![];
        while let Ok(r) = rx.try_recv() {
            if let Request::ChartSessionSummary { key, .. } = r {
                keys.push(key);
            }
        }
        keys
    }
    #[test]
    fn same_length_correction_and_prepend_invalidate_and_remap() {
        let (w, _) = Worker::test_channel();
        let mut c = fixture(&w);
        accept(&mut c);
        let old = c.session_key();
        let mut bars = (*c.bars).clone();
        bars[1].volume *= 3.;
        let bars = Arc::new(bars);
        c.on_history(c.generation, bars.clone(), SeriesGpu::build(&bars), 0, false, true, false, &w);
        assert_ne!(c.session_key(), old);
        assert!(c.session_snapshot().is_none());
        accept(&mut c);
        assert_eq!(c.session_snapshot().unwrap().stats, tr_data::intraday::bar_stats(c.tf, &bars));
        let date = tr_core::ny::civil(bars[0].ts).date();
        let info = tr_data::vault::StitchInfo {
            cells: vec![tr_data::vault::merge::classify_session(date, c.tf, &bars, 1, 0, c.last_refresh.unwrap())],
            ..Default::default()
        };
        c.on_vault_stitch(c.generation, Arc::new(info));
        accept(&mut c);
        assert_eq!(c.session_snapshot().unwrap().coverage[0].lo, 0);
        let mut prepended: Vec<_> =
            tr_data::intraday::regular_opens(tr_data::history::calendar::previous_session(date), c.tf)
                .into_iter()
                .map(|ts| Bar::new(ts, 100., 101., 99., 100., 10.).with_provenance(Provenance::Real))
                .collect();
        let n = prepended.len();
        prepended.extend_from_slice(&bars);
        let b = Arc::new(prepended);
        c.on_history(c.generation, b.clone(), SeriesGpu::build(&b), n, true, true, false, &w);
        assert!(c.session_snapshot().is_none());
        accept(&mut c);
        assert_eq!(c.session_snapshot().unwrap().coverage[0].lo, n);
    }
    #[test]
    fn stitch_receipt_and_clock_invalidate_even_with_unchanged_epoch_and_bars() {
        let (w, _) = Worker::test_channel();
        let mut c = fixture(&w);
        accept(&mut c);
        let mut info = tr_data::vault::StitchInfo { epoch: 1, source: "test".into(), ..Default::default() };
        c.on_vault_stitch(c.generation, Arc::new(info.clone()));
        accept(&mut c);
        let key = c.session_key();
        info.fetched_at = Some(123);
        c.on_vault_stitch(c.generation, Arc::new(info));
        assert_ne!(c.session_key(), key);
        assert!(c.session_snapshot().is_none());
        accept(&mut c);
        let key = c.session_key();
        c.last_refresh = c.last_refresh.map(|t| t + 1);
        assert_ne!(c.session_key(), key);
        assert!(c.session_snapshot().is_none());
    }
    #[test]
    fn hover_pan_and_theme_send_no_new_summary_request() {
        let (w, mut rx) = Worker::test_channel();
        let mut c = fixture(&w);
        c.request_session_summary(&w);
        let key = requests(&mut rx).pop().unwrap();
        let r = tr_data::chart_session::build(&c.bars, c.tf, c.sec.class, false, c.last_refresh, None).unwrap();
        c.on_session_summary(key, Ok(Arc::new(r)));
        c.viewport.as_mut().unwrap().pan_px(24.);
        c.hover_bar = Some(2);
        c.request_session_summary(&w);
        assert!(requests(&mut rx).is_empty());
        assert_eq!(c.session_snapshot().unwrap().bars, c.bars.len());
    }
    #[test]
    fn obsolete_response_releases_slot_and_latest_revision_coalesces() {
        let (w, mut rx) = Worker::test_channel();
        let mut c = fixture(&w);
        c.request_session_summary(&w);
        let old = requests(&mut rx).pop().unwrap();
        let r =
            Arc::new(tr_data::chart_session::build(&c.bars, c.tf, c.sec.class, false, c.last_refresh, None).unwrap());
        for _ in 0..5 {
            c.session.bars_revision += 1;
            c.request_session_summary(&w);
        }
        assert!(requests(&mut rx).is_empty());
        c.on_session_summary(old, Ok(r.clone()));
        assert!(c.session.pending.is_none());
        assert!(c.session.snapshot.is_none());
        assert!(c.request_session_summary(&w).is_some()); // 300 ms throttle still applies.
        c.session.sent_at = None;
        c.request_session_summary(&w);
        let keys = requests(&mut rx);
        assert_eq!(keys, vec![c.session_key()]);
        c.on_session_summary(keys[0], Ok(r.clone()));
        c.on_session_summary(old, Err("obsolete".into()));
        assert!(c.session_snapshot().is_some());
        c.session.bars_revision += 1;
        c.session.sent_at = None;
        c.request_session_summary(&w);
        assert!(c.session.status.contains("PREVIOUS SNAPSHOT"));
        let key = requests(&mut rx).pop().unwrap();
        c.on_session_summary(key, Err("refused".into()));
        assert!(c.session.snapshot.is_none());
        assert!(c.session.lines.is_empty());
        assert!(c.session.status.contains("refused"));
    }
    #[test]
    fn sample_and_source_identity_clear_numeric_snapshot() {
        let (w, _) = Worker::test_channel();
        let mut c = fixture(&w);
        accept(&mut c);
        c.stitch = Some(Arc::new(tr_data::vault::StitchInfo { source: "old".into(), ..Default::default() }));
        c.on_vault_stitch(
            c.generation,
            Arc::new(tr_data::vault::StitchInfo { source: "new".into(), ..Default::default() }),
        );
        assert!(c.session.snapshot.is_none());
        accept(&mut c);
        let mut b = (*c.bars).clone();
        b[0].provenance = Provenance::Sample;
        let b = Arc::new(b);
        c.on_history(c.generation, b.clone(), SeriesGpu::build(&b), 0, false, true, false, &w);
        assert!(c.session.snapshot.is_none());
        c.request_session_summary(&w);
        assert!(c.session.status.contains("NO REAL DATA"));
    }
    #[test]
    fn identity_change_discards_old_snapshot_and_releases_old_flight() {
        let (w, mut rx) = Worker::test_channel();
        let mut c = fixture(&w);
        c.request_session_summary(&w);
        let old = requests(&mut rx).pop().unwrap();
        let r =
            Arc::new(tr_data::chart_session::build(&c.bars, c.tf, c.sec.class, false, c.last_refresh, None).unwrap());
        c.load(&w);
        assert!(c.session.snapshot.is_none());
        assert_eq!(c.session.pending, Some(old));
        c.on_session_summary(old, Ok(r));
        assert!(c.session.pending.is_none());
        assert!(c.session.snapshot.is_none());
        let key = c.session_key();
        c.extended = !c.extended;
        assert_ne!(c.session_key(), key);
        let key = c.session_key();
        c.tf = Timeframe::M30;
        assert_ne!(c.session_key(), key);
    }
    #[test]
    fn accepted_strings_match_legacy_and_hovered_ohlc_follows_cursor() {
        let (w, _) = Worker::test_channel();
        let mut c = fixture(&w);
        accept(&mut c);
        for (legacy, kind) in [
            (session_line(c.tf, &c.sec, &c.bars), 0),
            (opening_line(c.tf, &c.sec, &c.bars), 1),
            (prior_line(c.tf, &c.sec, &c.bars), 2),
            (profile_line(c.tf, &c.sec, &c.bars), 3),
            (value_line(c.tf, &c.sec, &c.bars), 4),
        ] {
            assert_eq!(c.session.lines.iter().find(|(_, k)| *k == kind).map(|(s, _)| s.clone()), legacy);
        }
        let ctx = egui::Context::default();
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(1440., 900.));
        let mut texts = vec![];
        for i in [1, 80] {
            let mut out = ctx.run_ui(Default::default(), |ui| c.paint_legend(ui, rect, i, &[]));
            out.textures_delta.clear();
            let text = out
                .shapes
                .iter()
                .filter_map(
                    |s| if let egui::Shape::Text(t) = &s.shape { Some(t.galley.job.text.as_str()) } else { None },
                )
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("VWAP"));
            assert!(text.contains("OR30"));
            assert!(text.contains("PDH"));
            assert!(text.contains("CLOSED THRU"));
            texts.push(text);
        }
        assert_ne!(texts[0], texts[1]);
    }
}
