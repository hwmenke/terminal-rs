//! Auction Map: worker requests and timestamp-based cached chart geometry.
use super::*;
use crate::{command::AuctionCmd, layouts::AuctionSettings};
use tr_data::auction::{
    self as math, AuctionKey, AuctionParams, AuctionReport, Level, LevelEnd, LevelKind, SessionStatus,
};

#[derive(Default)]
pub(super) struct AuctionState {
    pub enabled: bool,
    view: AuctionSettings,
    input: Option<(Arc<Vec<Bar>>, AuctionKey)>,
    sent: Option<(AuctionKey, Instant)>,
    result: Option<Result<Arc<AuctionReport>, String>>,
    previous: Option<(AuctionKey, Arc<Vec<Bar>>)>,
    menu_level: Option<Level>,
    revision: u64,
    paint: AuctionPaint,
}
impl AuctionState {
    pub fn invalidate(&mut self) {
        self.input = None;
        self.sent = None;
        self.result = None;
        self.previous = None;
        self.menu_level = None;
        self.paint = AuctionPaint::default();
    }
}
#[derive(Default)]
struct AuctionPaint {
    key: Vec<u64>,
    palette: Option<theme::Palette>,
    shapes: Vec<egui::Shape>,
    labels: Vec<(Pos2, String, Color32)>,
    hits: Vec<Hit>,
    footer: String,
    note: String,
    detail: String,
    footer_rect: Option<Rect>,
}
struct Hit {
    rect: Rect,
    detail: Arc<str>,
    level: Option<Level>,
    row: Option<RowDetail>,
}
struct RowDetail {
    lo: f64,
    hi: f64,
    volume: f64,
    share: f64,
    poc_share: f64,
    brackets: Option<u8>,
    merged: bool,
}
impl Hit {
    fn tooltip(&self) -> String {
        match &self.row {
            Some(r) => format!(
                "{:.4}–{:.4}\nVolume {:.0} · {:.2}% of profile · {:.1}% of POC\n{}\n30m brackets: {}",
                r.lo,
                r.hi,
                r.volume,
                r.share,
                r.poc_share,
                self.detail,
                if r.merged {
                    "merged for display".into()
                } else {
                    r.brackets.map_or("unavailable".into(), |n| n.to_string())
                }
            ),
            None => self.detail.to_string(),
        }
    }
}
fn line(shapes: &mut Vec<egui::Shape>, a: Pos2, b: Pos2, color: Color32, width: f32) {
    shapes.push(egui::Shape::line_segment([a, b], Stroke::new(width, color)));
}
fn dashed(shapes: &mut Vec<egui::Shape>, a: Pos2, b: Pos2, color: Color32) {
    let n = ((b.x - a.x).abs() / 7.).ceil().max(1.) as usize;
    for i in 0..n {
        let t = i as f32 / n as f32;
        let u = ((i as f32 + 0.5) / n as f32).min(1.);
        line(shapes, a.lerp(b, t), a.lerp(b, u), color, 1.);
    }
}
impl ChartState {
    pub fn auction_settings(&self) -> Option<AuctionSettings> {
        self.auction.enabled.then(|| self.auction.view.clone())
    }
    pub fn restore_auction(&mut self, view: Option<AuctionSettings>) {
        self.auction.enabled = view.is_some();
        if let Some(mut v) = view {
            v.composite = v.composite.min(20);
            v.value_area = v.value_area.clamp(50, 95);
            v.max_age = v.max_age.clamp(1, 20);
            self.auction.view = v;
        }
        self.auction.invalidate();
    }
    pub fn set_auction(&mut self, cmd: AuctionCmd, w: &Worker) {
        use AuctionCmd::*;
        let a = &mut self.auction;
        match cmd {
            Toggle(on) => a.enabled = on.unwrap_or(!a.enabled),
            Composite(n) => {
                a.enabled = true;
                a.view.composite = n.min(20);
            }
            ValueArea(p) => {
                a.enabled = true;
                a.view.value_area = p.clamp(50, 95);
            }
            MaxAge(n) => {
                a.enabled = true;
                a.view.max_age = n.clamp(1, 20);
            }
            Naked(on) => {
                a.enabled = true;
                a.view.naked = on.unwrap_or(!a.view.naked);
            }
            Filled(on) => {
                a.enabled = true;
                a.view.filled = on.unwrap_or(!a.view.filled);
            }
            Strips(on) => {
                a.enabled = true;
                a.view.strips = on.unwrap_or(!a.view.strips);
            }
            Ib(on) => {
                a.enabled = true;
                a.view.ib = on.unwrap_or(!a.view.ib);
            }
        }
        let _ = self.request_auction(w);
    }
    fn auction_key(&mut self) -> AuctionKey {
        let closed = tr_data::closed::closed_len(self.tf, &self.bars, self.sec.class, self.last_refresh);
        let params = AuctionParams {
            lookback: 20,
            value_area_pct: self.auction.view.value_area,
            max_age: self.auction.view.max_age as usize,
        };
        let epoch = self.stitch.as_ref().map_or(0, |s| s.epoch);
        let gaps = self.stitch.as_ref().map_or(&[][..], |s| s.gaps.as_slice());
        if let Some((bars, k)) = &self.auction.input
            && Arc::ptr_eq(bars, &self.bars)
            && k.closed == closed
            && k.params == params
            && k.epoch == epoch
            && k.gaps == gaps
            && k.generation == self.generation
            && k.tf == self.tf
        {
            return k.clone();
        }
        let mut key = AuctionKey::new(self.generation, self.tf, closed, params, &self.bars, epoch);
        key.gaps = gaps.to_vec();
        self.auction.input = Some((self.bars.clone(), key.clone()));
        key
    }
    #[must_use]
    pub(super) fn request_auction(&mut self, w: &Worker) -> Option<Duration> {
        if !self.auction.enabled {
            return None;
        }
        let reason = if self.archive || !self.tf.is_intraday() || self.tf.seconds() > 3600 {
            Some("Auction Map needs a live intraday chart (1m–1h)")
        } else if !tr_data::time_link::cash_class(self.sec.class)
            || tr_data::cash_session::require_us_session(&self.sec.ticker, None).is_err()
        {
            Some("Auction Map needs a US cash session")
        } else {
            None
        };
        if let Some(reason) = reason {
            self.auction.result = Some(Err(reason.into()));
            self.auction.paint = Default::default();
            return None;
        }
        let key = self.auction_key();
        if let Some((sent, at)) = &self.auction.sent {
            if *sent == key {
                return None;
            }
            let keep = self.auction.previous.as_ref().is_some_and(|(old, bars)| {
                old.generation == key.generation
                    && old.tf == key.tf
                    && old.params == key.params
                    && old.epoch == key.epoch
                    && old.gaps == key.gaps
                    && key.closed >= old.closed
                    && bars.first().is_some_and(|first| {
                        let start = self.bars.partition_point(|b| b.ts < first.ts);
                        start + old.closed <= key.closed
                            && tr_data::stable_hash::bars_digest(&self.bars[start..start + old.closed]) == old.inputs
                    })
            });
            if !keep {
                self.auction.result = None;
                self.auction.paint = Default::default();
            }
            let elapsed = at.elapsed();
            if elapsed < Duration::from_millis(300) {
                return Some(Duration::from_millis(300) - elapsed);
            }
        }
        w.send(Request::AuctionMap {
            pane: self.id,
            key: key.clone(),
            symbol: self.symbol.clone(),
            class: self.sec.class,
            bars: self.bars.clone(),
            as_of: self.last_refresh,
            stitch: self.stitch.clone(),
        });
        self.auction.sent = Some((key, Instant::now()));
        None
    }
    pub fn on_auction_map(&mut self, key: AuctionKey, result: Result<Arc<AuctionReport>, String>) {
        if self.auction_key() != key {
            return;
        }
        self.auction.previous = result.as_ref().ok().map(|_| (key, self.bars.clone()));
        self.auction.result = Some(result);
        self.auction.revision = self.auction.revision.wrapping_add(1);
        self.auction.paint = Default::default();
    }
    /// Room above the price pane's bottom edge taken by the Bar Vault footer label
    /// and cells; the Auction footer sits on top of it.
    fn auction_footer_lift(&self) -> f32 {
        let vault_label =
            !self.extended && self.tf.is_intraday() && self.stitch.as_ref().is_some_and(|s| !s.cells.is_empty());
        if vault_label { 22. } else { 6. }
    }
    /// Height reserved at the bottom-left for the Auction footer (two rows) plus
    /// whatever sits under it. Zero when the map is off or has no report.
    pub(super) fn auction_footer_room(&self) -> f32 {
        if self.auction.enabled && matches!(self.auction.result, Some(Ok(_))) {
            self.auction_footer_lift() + 32.
        } else {
            0.
        }
    }
    /// Left edge of the Trend Cone's fan when it is on screen; the cone owns the
    /// right side then, so the composite dock hides and level tags stop before it.
    fn cone_origin_x(&self, rect: Rect, v: &Viewport) -> Option<f32> {
        let Some(Ok(r)) = self.cone.result.as_ref().filter(|_| self.cone.enabled) else { return None };
        Some(rect.left() + v.x_of(r.closed as f64 - 0.5) as f32).filter(|x| *x < rect.right())
    }
    /// One legend line for the latest session: POC, value area and open location.
    pub(super) fn auction_legend_line(&self) -> Option<String> {
        let Some(Ok(r)) = self.auction.result.as_ref().filter(|_| self.auction.enabled) else { return None };
        let s = r.sessions.iter().rev().find(|s| s.total > 0.)?;
        let fmt = |p: f64| scale::format_price(p, self.sec.decimals(p));
        let open = match s.open_location {
            Some(math::OpenLocation::AboveValue) => " · OPEN ABOVE VALUE",
            Some(math::OpenLocation::InValue) => " · OPEN IN VALUE",
            Some(math::OpenLocation::BelowValue) => " · OPEN BELOW VALUE",
            Some(math::OpenLocation::OutsideRange) => " · OPEN OUTSIDE RANGE",
            None => "",
        };
        Some(format!(
            "AMAP {}{} POC {} · VA {}–{} {:.0}%{open}",
            s.date.format("%m-%d"),
            if s.status == SessionStatus::Developing { " DEV" } else { "" },
            fmt((s.poc_bin as f64 + 0.5) * r.bin),
            fmt(s.va_bins.0 as f64 * r.bin),
            fmt((s.va_bins.1 + 1) as f64 * r.bin),
            s.va_share * 100.,
        ))
    }
    fn prepare_auction(&mut self, rect: Rect, v: &Viewport, scale: &PriceScale) {
        let cone_x = self.cone_origin_x(rect, v);
        let avwap_tags = self.avwap_tag_rects();
        let lift = self.auction_footer_lift();
        let a = &self.auction;
        let view = &a.view;
        let mut key = vec![
            a.revision,
            self.bars.len() as u64,
            self.bars.first().map_or(0, |b| b.ts as u64),
            v.right.to_bits(),
            v.bar_px.to_bits(),
            v.width_px.to_bits(),
            scale.lo.to_bits(),
            scale.hi.to_bits(),
            scale.log as u64,
            rect.left().to_bits() as u64,
            rect.top().to_bits() as u64,
            rect.width().to_bits() as u64,
            rect.height().to_bits() as u64,
            view.strips as u64,
            view.composite as u64,
            view.naked as u64,
            view.filled as u64,
            view.ib as u64,
            cone_x.map_or(u64::MAX, |x| x.to_bits() as u64),
            lift.to_bits() as u64,
        ];
        key.extend(avwap_tags.iter().flat_map(|r| [r.left().to_bits() as u64, r.top().to_bits() as u64]));
        if a.paint.key == key && a.paint.palette == Some(theme::palette()) {
            return;
        }
        let mut paint = AuctionPaint { key, palette: Some(theme::palette()), ..Default::default() };
        let Some(Ok(r)) = &a.result else {
            self.auction.paint = paint;
            return;
        };
        let y = |p| rect.top() + scale.y_of(p) as f32;
        let x = |ts| rect.left() + v.x_of(self.index_of_ts(ts)) as f32;
        let mut mesh = egui::Mesh::default();
        let (lo, hi) = v.visible_range(self.bars.len());
        let span = tr_data::time_link::window_of(&self.bars, self.tf, true, lo, hi);
        if view.strips {
            for s in &r.sessions {
                // A vault-only older session may supply levels, but never a fabricated strip.
                let i = self.bars.partition_point(|b| b.ts < s.open_ts);
                if self.bars.get(i).is_none_or(|b| b.ts >= s.close_ts) {
                    continue;
                }
                let left = rect.left() + v.x_of(i as f64) as f32;
                let right = x(s.close_ts).max(left + v.bar_px as f32);
                if right < rect.left() || left > rect.right() {
                    continue;
                }
                let width = right - left;
                let val = y(s.va_bins.0 as f64 * r.bin);
                let vah = y((s.va_bins.1 + 1) as f64 * r.bin);
                let py = y((s.poc_bin as f64 + 0.5) * r.bin);
                if s.total <= 0. {
                    paint.labels.push((pos2(left, rect.bottom() - 60.), "NO VOLUME".into(), theme::amber()));
                    continue;
                }
                if width < 28. || rect.height() < 200. {
                    line(&mut paint.shapes, pos2(left, vah), pos2(left, val), theme::accent(), 2.);
                    paint.shapes.push(egui::Shape::circle_filled(pos2(left, py), 2.5, theme::amber()));
                } else {
                    let maxw = (0.45 * width).min(140.);
                    paint.shapes.push(egui::Shape::rect_filled(
                        Rect::from_x_y_ranges(left..=right, vah..=val),
                        0.,
                        theme::accent().gamma_multiply(0.08),
                    ));
                    profile_rows(
                        &mut paint,
                        &mut mesh,
                        &s.volume,
                        s.first_bin,
                        r.bin,
                        left,
                        maxw,
                        false,
                        rect,
                        scale,
                        Some(s),
                        s.total,
                        s.poc_bin,
                        s.va_bins,
                    );
                    for yy in [val, vah] {
                        line(
                            &mut paint.shapes,
                            pos2(left, yy),
                            pos2(right, yy),
                            theme::accent().gamma_multiply(0.6),
                            1.,
                        );
                    }
                    line(&mut paint.shapes, pos2(left, py), pos2(right, py), theme::amber().gamma_multiply(0.35), 1.);
                    line(&mut paint.shapes, pos2(left, py), pos2(left + maxw, py), theme::amber(), 2.);
                    for k in &s.lvn {
                        let yy = y((*k as f64 + 0.5) * r.bin);
                        line(&mut paint.shapes, pos2(left, yy), pos2(left + 6., yy), theme::series(4), 2.);
                    }
                    for run in &s.single_prints {
                        let top = y((run.hi + 1) as f64 * r.bin).max(rect.top());
                        let bot = y(run.lo as f64 * r.bin).min(rect.bottom());
                        let mut yy = top;
                        while yy < bot {
                            line(
                                &mut paint.shapes,
                                pos2(left, yy),
                                pos2(left + 7., (yy + 5.).min(bot)),
                                theme::cyan().gamma_multiply(0.65),
                                1.,
                            );
                            yy += 7.;
                        }
                        if run.tail {
                            paint.labels.push((pos2(left + 9., top), "T".into(), theme::cyan()));
                        }
                    }
                    if view.ib
                        && let Some(ib) = s.ib
                    {
                        let xx = x(s.open_ts + 3600).min(right);
                        line(&mut paint.shapes, pos2(xx, y(ib.low)), pos2(xx, y(ib.high)), theme::muted(), 1.);
                        for p in [ib.low, ib.high] {
                            line(&mut paint.shapes, pos2(xx - 5., y(p)), pos2(xx + 5., y(p)), theme::muted(), 1.);
                        }
                        for m in [1., 2.] {
                            for p in [ib.high + m * (ib.high - ib.low), ib.low - m * (ib.high - ib.low)] {
                                dashed(
                                    &mut paint.shapes,
                                    pos2(xx, y(p)),
                                    pos2(right, y(p)),
                                    theme::muted().gamma_multiply(0.5),
                                );
                            }
                        }
                    }
                }
                let status = match s.status {
                    SessionStatus::Complete => {
                        if s.half_day {
                            "½ DAY".into()
                        } else {
                            String::new()
                        }
                    }
                    SessionStatus::Developing => "DEV".into(),
                    SessionStatus::Partial => format!("PARTIAL {}/{}", s.bars_used, s.expected),
                };
                if !status.is_empty() {
                    // Tag under the session's low, away from the top-left legend. Narrow
                    // sessions get a one-letter tag; the hover card has the full status.
                    let fits = width >= status.chars().count() as f32 * 5.6 + 6.;
                    let tag = if fits {
                        Some(status)
                    } else if width >= 10. {
                        status.chars().next().map(String::from)
                    } else {
                        None
                    };
                    if let Some(tag) = tag {
                        paint.labels.push((
                            pos2(
                                left.max(rect.left()) + 2.,
                                (y(s.low) + 3.).min(rect.bottom() - lift - 46.).max(rect.top() + 2.),
                            ),
                            tag,
                            theme::amber(),
                        ));
                    }
                    let top = y(s.high);
                    let bottom = y(s.low);
                    for yy in [top, bottom] {
                        dashed(&mut paint.shapes, pos2(left, yy), pos2(right, yy), theme::amber_dim());
                    }
                    for xx in [left, right] {
                        line(
                            &mut paint.shapes,
                            pos2(xx, top),
                            pos2(xx, bottom),
                            theme::amber_dim().gamma_multiply(0.5),
                            0.6,
                        );
                    }
                    if s.status == SessionStatus::Partial {
                        let stripe = Rect::from_x_y_ranges(
                            left.max(rect.left())..=right.min(rect.right()),
                            top.max(rect.top())..=bottom.min(rect.bottom()),
                        );
                        if stripe.is_positive() {
                            let mut yy = stripe.top();
                            while yy < stripe.bottom() {
                                line(
                                    &mut paint.shapes,
                                    pos2(stripe.left(), yy),
                                    pos2((stripe.left() + 8.).min(stripe.right()), (yy + 6.).min(stripe.bottom())),
                                    theme::amber().gamma_multiply(0.3),
                                    1.,
                                );
                                yy += 10.;
                            }
                        }
                    }
                }
            }
        }
        let selected: Vec<_> = if view.composite == 0 {
            r.sessions.iter().filter(|s| span.is_some_and(|(a, b)| s.first_bar_ts < b && s.last_bar_ts >= a)).collect()
        } else {
            r.sessions.iter().rev().take(view.composite as usize).collect()
        };
        let dock = if cone_x.is_none() && rect.width() >= 420. && rect.height() >= 200. {
            (rect.width() * 0.18).clamp(60., 160.)
        } else {
            0.
        };
        // Only a gap inside the composite's own time span is worth flagging.
        let first_open = selected.iter().map(|s| s.open_ts).min().unwrap_or(i64::MAX);
        let last_close = selected.iter().map(|s| s.close_ts).max().unwrap_or(i64::MIN);
        let comp_gap = r.gaps.iter().any(|(a, b)| *b > first_open && *a < last_close);
        if dock > 0.
            && let Some(c) = math::composite(&selected, view.value_area)
        {
            profile_rows(
                &mut paint,
                &mut mesh,
                &c.volume,
                c.first_bin,
                r.bin,
                rect.right(),
                dock,
                true,
                rect,
                scale,
                None,
                c.total,
                c.poc_bin,
                c.va_bins,
            );
            let py = y((c.poc_bin as f64 + 0.5) * r.bin);
            line(&mut paint.shapes, pos2(rect.right() - dock, py), pos2(rect.right(), py), theme::amber(), 2.);
            for p in [c.va_bins.0 as f64 * r.bin, (c.va_bins.1 + 1) as f64 * r.bin] {
                line(
                    &mut paint.shapes,
                    pos2(rect.right() - dock - 4., y(p)),
                    pos2(rect.right() - dock + 4., y(p)),
                    theme::accent(),
                    1.,
                );
            }
            let poc_price = (c.poc_bin as f64 + 0.5) * r.bin;
            let header = format!(
                "{} {}s{}{}",
                if view.composite == 0 { "VIS" } else { "COMP" },
                selected.len(),
                if c.partial > 0 { format!(" · {} partial", c.partial) } else { String::new() },
                if comp_gap { " · gap" } else { "" }
            );
            // Pills: the POC price on its line, the header below the corner badge
            // (REAL / STALE … sits at the top-right, ~35–48 px down).
            for (text, at, color) in [
                (
                    format!("POC {}", scale::format_price(poc_price, self.sec.decimals(poc_price))),
                    py - 17.,
                    theme::amber(),
                ),
                (header, rect.top() + 54., theme::text()),
            ] {
                let width = (text.chars().count() as f32 * 5.6 + 8.).min(dock);
                let pill = Rect::from_min_size(pos2(rect.right() - dock, at), vec2(width, 14.));
                paint.shapes.push(egui::Shape::rect_filled(pill, 3., theme::card().gamma_multiply(0.92)));
                paint.shapes.push(egui::Shape::rect_stroke(
                    pill,
                    3.,
                    Stroke::new(1., color.gamma_multiply(0.45)),
                    egui::StrokeKind::Inside,
                ));
                paint.labels.push((pill.min + vec2(4., 1.5), text, color));
            }
        }
        let tag_right = cone_x.map_or(rect.right() - dock - 8., |x| (x - 8.).max(rect.left() + 160.));
        if view.naked {
            let mut levels: Vec<_> = r
                .levels
                .iter()
                .filter(|l| {
                    l.age > 0 && (l.end == LevelEnd::Naked || matches!(l.end, LevelEnd::CutByGap { .. }) || view.filled)
                })
                .filter(|l| l.price >= scale.lo && l.price <= scale.hi)
                .collect();
            let last = r.sessions.last().map_or(0., |s| s.close);
            levels.sort_by(|a, b| (a.price - last).abs().total_cmp(&(b.price - last).abs()));
            let bottom = rect.bottom() - lift - 40.;
            levels.truncate((((bottom - rect.top() - 57.) / 17.).floor().max(1.) as usize).min(12));
            levels.sort_by(|a, b| b.price.total_cmp(&a.price));
            let count = levels.len();
            let mut tag_y = rect.top() + 57.;
            for (row, l) in levels.into_iter().enumerate() {
                let yy = y(l.price);
                let end = match l.end {
                    LevelEnd::Naked => tag_right,
                    LevelEnd::Touched { ts } | LevelEnd::CutByGap { ts } => x(ts).min(tag_right),
                };
                // A gap immediately after the origin ends at its last bar,
                // before the scheduled close. Still show the GAP cap there.
                let start = match l.end {
                    LevelEnd::CutByGap { ts } => x(l.origin_close_ts).min(x(ts)),
                    _ => x(l.origin_close_ts),
                }
                .max(rect.left());
                if end < rect.left() || start > end {
                    continue;
                }
                let color = if l.kind == LevelKind::Poc { theme::amber() } else { theme::accent() }
                    .gamma_multiply(if matches!(l.end, LevelEnd::Touched { .. }) { 0.3 } else { 0.8 });
                if l.kind == LevelKind::Poc {
                    line(&mut paint.shapes, pos2(start, yy), pos2(end, yy), color, 1.4);
                } else {
                    dashed(&mut paint.shapes, pos2(start, yy), pos2(end, yy), color);
                }
                match l.end {
                    LevelEnd::Touched { .. } => paint.shapes.push(egui::Shape::circle_filled(pos2(end, yy), 2., color)),
                    LevelEnd::CutByGap { .. } => {
                        line(&mut paint.shapes, pos2(end, yy - 4.), pos2(end, yy + 4.), color, 2.)
                    }
                    LevelEnd::Naked => {}
                }
                let ty = yy.max(tag_y).min(bottom - (count - row - 1) as f32 * 17.);
                tag_y = ty + 17.;
                let label = format!(
                    "{}{} {} · {}s · {} tests{}{}{}",
                    if self.bars.first().is_some_and(|b| b.ts > l.origin_open_ts) { "◂ " } else { "" },
                    match l.kind {
                        LevelKind::Poc => "nPOC",
                        LevelKind::Vah => "nVAH",
                        LevelKind::Val => "nVAL",
                    },
                    scale::format_price(l.price, self.sec.decimals(l.price)),
                    l.age,
                    l.tests,
                    if l.uncertain { " ?" } else { "" },
                    if l.gapped_over { " ⇅" } else { "" },
                    if matches!(l.end, LevelEnd::CutByGap { .. }) { " GAP" } else { "" }
                );
                let width = (label.chars().count() as f32 * 5.6 + 8.).min(rect.width());
                let mut right = end;
                // Step left of any AVWAP price tag on the same row (those are drawn later, on top).
                for _ in 0..4 {
                    let probe = Rect::from_min_max(pos2(right - width, ty - 8.), pos2(right, ty + 8.));
                    match avwap_tags.iter().find(|r| r.intersects(probe)) {
                        Some(r) => right = r.left() - 4.,
                        None => break,
                    }
                }
                let tag = Rect::from_min_size(pos2((right - width).max(rect.left()), ty - 7.), vec2(width, 15.));
                line(&mut paint.shapes, pos2(end, yy), pos2(end.min(tag.right()), ty), color, 0.8);
                paint.shapes.push(egui::Shape::rect_filled(tag, 3., theme::card().gamma_multiply(0.94)));
                paint.shapes.push(egui::Shape::rect_stroke(
                    tag,
                    3.,
                    Stroke::new(1., color.gamma_multiply(0.5)),
                    egui::StrokeKind::Inside,
                ));
                paint.labels.push((tag.min + vec2(4., 1.5), label, color));
                paint.hits.push(Hit { rect:tag,detail:format!("{:?} {:.4}\nOrigin {} · {} sessions · {} tests\nDistance {:+.4} ({:+.2}%)\n{:?}\nUntouched in closed regular-session bars until the recorded end.\nClick: centre origin · Alt+click: anchor VWAP at open",l.kind,l.price,l.origin,l.age,l.tests,l.price-last,(l.price/last-1.)*100.,l.end).into(),level:Some(l.clone()),row:None });
            }
        }
        // One mesh for all profile rows. Lines and labels remain separate, cached shapes.
        paint.shapes.insert(0, egui::Shape::mesh(mesh));
        let time = tr_core::ny::civil(r.closed_thru).format("%H:%M").to_string();
        paint.footer = format!("AUCTION · bin {:.4} · VA {}% · RTH · CLOSED THRU {time}", r.bin, view.value_area);
        let sources: BTreeSet<_> = r.sessions.iter().map(|s| s.source_tf.to_string()).collect();
        paint.detail = format!(
            "Verified closed bars · {}\nUniform high–low volume spread: v × overlap / (H−L). Not tick volume at price.\nTick-anchored lattice: tick × nice_ceil(median complete-session range / 40 / tick).\nPOC: maximum volume; ties nearest range midpoint, then lower.\nValue area: contiguous expansion from POC; larger neighbour, remaining side volume, then lower.\nNaked levels end at the first closed RTH range containing price, or at a vault gap.\nVIS uses whole sessions overlapping the visible bars.\n{} ETH bars ignored · {} gap(s)\nSessions: {}\nSources: {}\n{}",
            r.source,
            r.eth_ignored,
            r.gaps.len(),
            r.sessions.iter().map(|s| s.date.to_string()).collect::<Vec<_>>().join(", "),
            sources.into_iter().collect::<Vec<_>>().join(" / "),
            r.fine_note.as_deref().unwrap_or("")
        );
        paint.note = format!("uniform H–L · {}", r.fine_note.as_deref().unwrap_or("1m"));
        let width =
            ((paint.footer.chars().count() + 2) as f32 * 5.5).max(214. + paint.note.chars().count() as f32 * 5.5);
        paint.footer_rect = Some(Rect::from_min_max(
            pos2(rect.left() + 8., rect.bottom() - lift - 28.),
            pos2((rect.left() + 12. + width).min(rect.right()), rect.bottom() - lift),
        ));
        self.auction.paint = paint;
    }
    pub(super) fn paint_auction(&mut self, ui: &mut Ui, rect: Rect, v: &Viewport, scale: &PriceScale) {
        if !self.auction.enabled {
            return;
        }
        let p = ui.painter_at(rect);
        if !matches!(self.auction.result, Some(Ok(_))) {
            let reason = self.auction.result.as_ref().and_then(|r| r.as_ref().err());
            let text = reason.map_or_else(
                || "AUCTION MAP · building verified session profiles…".into(),
                |s| format!("AUCTION MAP · NO REAL DATA\n{s}"),
            );
            let g = p.layout(
                text,
                mono(11.),
                if reason.is_some() { theme::down() } else { theme::muted() },
                (rect.width() - 28.).max(1.),
            );
            let offset = if self.cone.enabled && !matches!(self.cone.result, Some(Ok(_))) { 110. } else { 44. };
            let at = rect.min + vec2(8., offset);
            p.rect_filled(Rect::from_min_size(at, g.size() + vec2(12., 10.)), 4., theme::card());
            p.galley(at + vec2(6., 5.), g, theme::text());
            return;
        }
        self.prepare_auction(rect, v, scale);
        let paint = &self.auction.paint;
        p.extend(paint.shapes.iter().cloned());
        for (at, text, color) in &paint.labels {
            p.text(*at, Align2::LEFT_TOP, text, mono(9.), *color);
        }
        let updating = self
            .auction
            .sent
            .as_ref()
            .is_some_and(|(k, _)| self.auction.previous.as_ref().is_none_or(|(old, _)| old != k));
        let base = rect.bottom() - self.auction_footer_lift();
        // A card under the two footer rows keeps them legible over candles.
        if let Some(r) = paint.footer_rect {
            p.rect_filled(r.expand2(vec2(4., 1.)), 3., theme::card().gamma_multiply(0.85));
        }
        p.text(
            pos2(rect.left() + 8., base - 27.),
            Align2::LEFT_TOP,
            format!("{}{}", paint.footer, if updating { " …" } else { "" }),
            mono(9.),
            theme::text(),
        );
        widgets::seq_key(
            &p,
            pos2(rect.left() + 8., base - 13.),
            theme::series(2),
            &["20", "40", "60", "80", "100% of POC"],
        );
        let fallback = matches!(&self.auction.result, Some(Ok(r)) if r.fine_note.is_some());
        p.text(
            pos2(rect.left() + 222., base - 13.),
            Align2::LEFT_TOP,
            &paint.note,
            mono(9.),
            if fallback { theme::amber() } else { theme::muted() },
        );
        if self.hover_enabled
            && let Some(at) = ui.input(|i| i.pointer.hover_pos())
        {
            if let Some(hit) = paint.hits.iter().rev().find(|h| h.rect.contains(at)) {
                ui.interact(hit.rect, ui.id().with((self.id, "auction-hit")), Sense::hover())
                    .on_hover_text(hit.tooltip());
            } else if let Some(r) = paint.footer_rect
                && r.contains(at)
            {
                ui.interact(r, ui.id().with((self.id, "auction-math")), Sense::hover()).on_hover_text(&paint.detail);
            }
        }
    }
    fn auction_origin(&mut self, l: &Level, anchor: bool) {
        let i = self.bars.partition_point(|b| b.ts < l.origin_open_ts);
        let loaded = self.bars.get(i).is_some_and(|b| b.ts < l.origin_close_ts);
        if anchor {
            if !loaded || self.bars[i].ts != l.origin_open_ts {
                self.avwap_notice = Some(tr_data::anchored_vwap::BEFORE_HISTORY.into());
            } else if let Err(e) = self.anchor_at_bar(i) {
                self.avwap_notice = Some(e);
            }
        } else if loaded {
            self.center_on_ts((l.origin_open_ts + l.origin_close_ts) / 2);
        } else {
            self.request_jump_to_ts(l.origin_open_ts);
        }
    }
    pub(super) fn auction_click(&mut self, ui: &Ui, response: &egui::Response) -> bool {
        if !self.auction.enabled || !self.hover_enabled || !response.clicked() {
            return false;
        }
        let level = response
            .interact_pointer_pos()
            .and_then(|p| self.auction.paint.hits.iter().rev().find(|h| h.rect.contains(p) && h.level.is_some()))
            .and_then(|h| h.level.clone());
        if let Some(l) = level {
            self.auction_origin(
                &l,
                ui.input(|i| {
                    i.modifiers.alt
                        || i.events
                            .iter()
                            .any(|e| matches!(e,egui::Event::PointerButton { modifiers,.. } if modifiers.alt))
                }),
            );
            true
        } else {
            false
        }
    }
    pub(super) fn auction_context(&mut self, response: &egui::Response) {
        if response.secondary_clicked() {
            self.auction.menu_level = response
                .interact_pointer_pos()
                .and_then(|p| self.auction.paint.hits.iter().rev().find(|h| h.rect.contains(p)))
                .and_then(|h| h.level.clone());
        }
    }
    pub(super) fn auction_menu(&mut self, ui: &mut Ui) {
        let level = self.auction.menu_level.clone();
        ui.menu_button("Auction Map", |ui| {
            ui.checkbox(&mut self.auction.enabled, "On");
            ui.checkbox(&mut self.auction.view.strips, "Session strips");
            ui.menu_button("Composite", |ui| {
                for (n, label) in
                    [(5, "5 sessions"), (10, "10 sessions"), (20, "20 sessions"), (0, "Visible whole sessions")]
                {
                    ui.radio_value(&mut self.auction.view.composite, n, label);
                }
            });
            ui.checkbox(&mut self.auction.view.naked, "Naked levels");
            ui.checkbox(&mut self.auction.view.filled, "Filled levels");
            ui.checkbox(&mut self.auction.view.ib, "Initial balance + extensions");
            if let Some(l) = level {
                ui.separator();
                if ui.button("Centre origin session").clicked() {
                    self.auction_origin(&l, false);
                    ui.close();
                }
                if ui.button("Anchor VWAP at origin open").clicked() {
                    self.auction_origin(&l, true);
                    ui.close();
                }
            }
        });
    }
}
use std::collections::BTreeSet;
#[allow(clippy::too_many_arguments)]
fn profile_rows(
    paint: &mut AuctionPaint,
    mesh: &mut egui::Mesh,
    volume: &[f64],
    first: i64,
    bin: f64,
    left: f32,
    width: f32,
    reverse: bool,
    rect: Rect,
    scale: &PriceScale,
    session: Option<&math::SessionProfile>,
    total: f64,
    poc: i64,
    va: (i64, i64),
) {
    let max = volume[(poc - first) as usize];
    if max <= 0. {
        return;
    }
    let y = |p| rect.top() + scale.y_of(p) as f32;
    // Share session metadata across row hit targets; format numeric cards only
    // for the hovered row. This keeps viewport rebuilds within the frame budget.
    let detail: Arc<str> = session.map_or_else(
        || "Composite · whole sessions · aligned lattice sum".into(),
        |s| {
            format!(
                "{}{} · {:?}\nbuilt from {} · {}/{} bars · {} holes · {} missing volume",
                s.date,
                if s.half_day { " · ½ day" } else { "" },
                s.status,
                s.source_tf,
                s.bars_used,
                s.expected,
                s.holes,
                s.no_volume
            )
            .into()
        },
    );
    let mut i = 0;
    while i < volume.len() {
        let start = i;
        let bottom = y((first + i as i64) as f64 * bin);
        let mut sum = volume[i];
        let mut count = 1;
        while i + 1 < volume.len() && (bottom - y((first + i as i64 + 1) as f64 * bin)).abs() < 2. {
            i += 1;
            sum += volume[i];
            count += 1;
        }
        let top = y((first + i as i64 + 1) as f64 * bin);
        if sum > 0. && bottom >= rect.top() && top <= rect.bottom() {
            let ratio = (sum / count as f64 / max).min(1.);
            let len = ratio as f32 * width;
            let row = Rect::from_x_y_ranges(
                if reverse { left - len..=left } else { left..=left + len },
                top.max(rect.top())..=bottom.min(rect.bottom()),
            );
            let inside = first + start as i64 >= va.0 && first + i as i64 <= va.1;
            let color = widgets::seq_ramp((ratio * 5.).ceil() as usize, theme::series(2)).gamma_multiply(if inside {
                1.
            } else {
                0.45
            });
            mesh.add_colored_rect(row, color);
            paint.hits.push(Hit {
                rect: row,
                detail: detail.clone(),
                level: None,
                row: Some(RowDetail {
                    lo: (first + start as i64) as f64 * bin,
                    hi: (first + i as i64 + 1) as f64 * bin,
                    volume: sum,
                    share: sum / total * 100.,
                    poc_share: ratio * 100.,
                    brackets: session.and_then(|s| s.brackets.get(start).copied()),
                    merged: count > 1,
                }),
            });
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> ChartState {
        let mut c = ChartState::new(7, "AAPL", Timeframe::M5);
        c.bars = Arc::new(
            [("2026-11-06", 100.), ("2026-11-09", 103.), ("2026-11-10", 106.)]
                .into_iter()
                .flat_map(|(date, p)| {
                    tr_data::intraday::regular_opens(date.parse().unwrap(), Timeframe::M5)
                        .into_iter()
                        .map(move |ts| Bar::new(ts, p, p + 0.4, p - 0.4, p, 100.).with_provenance(Provenance::Real))
                })
                .collect(),
        );
        c.last_refresh = Some(c.bars.last().unwrap().ts + 300);
        c.auction.enabled = true;
        c.viewport = Some(Viewport { right: c.bars.len() as f64 + 8., bar_px: 3., width_px: 780. });
        c
    }
    fn report(c: &ChartState) -> Arc<AuctionReport> {
        Arc::new(
            math::build(&c.bars, c.tf, None, c.sec.class, c.last_refresh.unwrap(), &[], Default::default(), 0.01)
                .unwrap(),
        )
    }
    fn ready(c: &mut ChartState) {
        let key = c.auction_key();
        let r = report(c);
        c.on_auction_map(key, Ok(r));
    }
    fn requests(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Request>) -> Vec<AuctionKey> {
        let mut out = vec![];
        while let Ok(req) = rx.try_recv() {
            if let Request::AuctionMap { key, .. } = req {
                out.push(key);
            }
        }
        out
    }
    fn geometry(c: &mut ChartState) {
        let rect = Rect::from_min_size(pos2(0., 0.), vec2(780., 500.));
        let scale = PriceScale::padded(98., 108., 0.01, false, 500.);
        c.prepare_auction(rect, &c.viewport.unwrap(), &scale);
    }
    fn paint(c: &mut ChartState, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                events,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800., 600.))),
                ..Default::default()
            },
            |ui| {
                let rect = Rect::from_min_size(Pos2::ZERO, vec2(780., 500.));
                c.paint_auction(ui, rect, &c.viewport.unwrap(), &PriceScale::padded(98., 108., 0.01, false, 500.));
            },
        );
        output.textures_delta.clear();
        output
    }
    fn all_text(out: &egui::FullOutput) -> String {
        out.shapes
            .iter()
            .filter_map(|s| if let egui::Shape::Text(t) = &s.shape { Some(t.galley.job.text.clone()) } else { None })
            .collect::<Vec<_>>()
            .join("\n")
    }
    #[test]
    fn auction_request_is_not_resent_for_an_unchanged_key_or_a_forming_tick() {
        let (w, mut rx) = Worker::test_channel();
        let mut c = fixture();
        c.last_refresh = Some(c.bars.last().unwrap().ts);
        assert_eq!(c.request_auction(&w), None);
        let key = requests(&mut rx).pop().unwrap();
        assert_eq!(c.request_auction(&w), None);
        assert!(requests(&mut rx).is_empty());
        Arc::make_mut(&mut c.bars).last_mut().unwrap().volume *= 2.;
        assert_eq!(c.request_auction(&w), None);
        assert!(requests(&mut rx).is_empty());
        assert_eq!(c.auction_key(), key);
        Arc::make_mut(&mut c.bars)[1].volume *= 2.;
        assert!(c.request_auction(&w).is_some());
    }
    #[test]
    fn auction_response_for_an_old_key_is_dropped() {
        let mut c = fixture();
        let key = c.auction_key();
        c.generation += 1;
        c.on_auction_map(key, Ok(report(&c)));
        assert!(c.auction.result.is_none());
    }
    #[test]
    fn auction_keeps_previous_report_while_pending_and_paging_but_not_across_symbol_change() {
        let (w, mut rx) = Worker::test_channel();
        let mut c = fixture();
        c.last_refresh = Some(c.bars.last().unwrap().ts);
        let _ = c.request_auction(&w);
        let key = requests(&mut rx).pop().unwrap();
        c.on_auction_map(key, Ok(report(&c)));
        let old = c.auction.result.as_ref().unwrap().as_ref().unwrap().clone();
        c.last_refresh = Some(c.last_refresh.unwrap() + 300);
        assert!(c.request_auction(&w).is_some());
        assert!(Arc::ptr_eq(&old, c.auction.result.as_ref().unwrap().as_ref().unwrap()));
        // Prepending verified history also retains the old timestamp-anchored geometry.
        let mut older: Vec<_> = tr_data::intraday::regular_opens("2026-11-05".parse().unwrap(), c.tf)
            .into_iter()
            .map(|ts| Bar::new(ts, 99., 100., 98., 99., 100.).with_provenance(Provenance::Real))
            .collect();
        older.extend_from_slice(&c.bars);
        c.bars = Arc::new(older);
        assert!(c.request_auction(&w).is_some());
        assert!(c.auction.result.as_ref().is_some_and(Result::is_ok));
        c.set_symbol("MSFT", &w);
        assert!(c.auction.result.is_none());
        assert!(c.auction.paint.shapes.is_empty());
    }
    #[test]
    fn auction_refusal_paints_zero_digits_and_no_rows() {
        let mut c = fixture();
        c.auction.result = Some(Err("Auction Map refused: unverified bars".into()));
        let out = paint(&mut c, &egui::Context::default(), vec![]);
        let text = all_text(&out);
        assert!(text.contains("AUCTION MAP · NO REAL DATA"));
        assert!(!text.chars().any(|c| c.is_ascii_digit()));
        assert!(c.auction.paint.shapes.is_empty());
        assert!(c.auction.paint.hits.is_empty());
    }
    #[test]
    fn archive_and_daily_charts_refuse_locally_and_send_no_request() {
        let (w, mut rx) = Worker::test_channel();
        for (tf, archive, class) in [
            (Timeframe::M5, true, AssetClass::Equity),
            (Timeframe::D1, false, AssetClass::Equity),
            (Timeframe::H4, false, AssetClass::Equity),
            (Timeframe::M5, false, AssetClass::Crypto),
        ] {
            let mut c = fixture();
            c.tf = tf;
            c.archive = archive;
            c.sec.class = class;
            let _ = c.request_auction(&w);
            assert!(requests(&mut rx).is_empty());
            assert!(c.auction.result.as_ref().is_some_and(Result::is_err));
        }
    }
    #[test]
    fn strips_anchor_at_each_session_first_bar_and_collapse_to_spines_when_narrow() {
        let mut c = fixture();
        ready(&mut c);
        geometry(&mut c);
        let r = c.auction.result.as_ref().unwrap().as_ref().unwrap();
        let first = &r.sessions[0];
        let anchor = c.viewport.unwrap().x_of(0.) as f32;
        let row = c
            .auction
            .paint
            .hits
            .iter()
            .find(|h| h.level.is_none() && h.detail.contains(&first.date.to_string()))
            .unwrap();
        assert!((row.rect.left() - anchor).abs() < 0.01);
        c.viewport.as_mut().unwrap().bar_px = 0.1;
        geometry(&mut c);
        assert!(c.auction.paint.hits.iter().all(|h| !h.detail.contains("built from")));
        assert!(c.auction.paint.shapes.iter().any(|s| matches!(s, egui::Shape::Circle(_))));
    }
    #[test]
    fn partial_session_paints_partial_tag_and_no_filled_gap() {
        let mut c = fixture();
        Arc::make_mut(&mut c.bars).remove(3);
        ready(&mut c);
        let out = paint(&mut c, &egui::Context::default(), vec![]);
        assert!(all_text(&out).contains("PARTIAL 77/78"));
        assert!(
            c.auction
                .result
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .levels
                .iter()
                .all(|l| l.origin != "2026-11-06".parse::<chrono::NaiveDate>().unwrap())
        );
    }
    #[test]
    fn composite_follows_visible_sessions_and_cache_is_reused_for_unchanged_viewport() {
        let mut c = fixture();
        ready(&mut c);
        c.auction.view.composite = 0;
        geometry(&mut c);
        assert!(c.auction.paint.labels.iter().any(|(_, s, _)| s.starts_with("VIS 3s")));
        let ptr = c.auction.paint.shapes.as_ptr();
        geometry(&mut c);
        assert_eq!(ptr, c.auction.paint.shapes.as_ptr());
        c.viewport = Some(Viewport { right: 234., bar_px: 10., width_px: 780. });
        geometry(&mut c);
        assert!(c.auction.paint.labels.iter().any(|(_, s, _)| s.starts_with("VIS 1s")));
    }
    #[test]
    fn gap_immediately_after_origin_paints_the_cap_and_tag() {
        let mut c = fixture();
        // Friday's complete origin followed by a missing Monday session.
        let origin_last = c.bars[77].ts;
        let next_first = c.bars[156].ts;
        c.bars = Arc::new(c.bars.iter().copied().filter(|b| b.ts <= origin_last || b.ts >= next_first).collect());
        let key = c.auction_key();
        let r = math::build(
            &c.bars,
            c.tf,
            None,
            c.sec.class,
            c.last_refresh.unwrap(),
            &[(origin_last, next_first)],
            Default::default(),
            0.01,
        )
        .unwrap();
        assert!(r.levels.iter().any(|l| l.end == LevelEnd::CutByGap { ts: origin_last }));
        c.on_auction_map(key, Ok(Arc::new(r)));
        c.viewport.as_mut().unwrap().right = c.bars.len() as f64 + 8.;
        geometry(&mut c);
        assert!(
            c.auction
                .paint
                .hits
                .iter()
                .any(|h| h.level.as_ref().is_some_and(|l| l.end == LevelEnd::CutByGap { ts: origin_last }))
        );
        assert!(c.auction.paint.labels.iter().any(|(_, text, _)| text.contains(" GAP")));
    }
    #[test]
    fn clicking_a_naked_tag_centres_and_alt_click_anchors_the_origin() {
        let mut c = fixture();
        ready(&mut c);
        geometry(&mut c);
        let l = c.auction.paint.hits.iter().find_map(|h| h.level.clone()).unwrap();
        let before = c.viewport;
        c.auction_origin(&l, false);
        assert_ne!(c.glide.as_ref().map(|g| g.to).or(c.viewport), before);
        c.auction_origin(&l, true);
        assert_eq!(c.avwap_anchors, vec![l.origin_open_ts]);
        c.bars = Arc::new(c.bars.iter().copied().filter(|b| b.ts >= l.origin_close_ts).collect());
        c.auction_origin(&l, true);
        assert_eq!(c.avwap_notice.as_deref(), Some(tr_data::anchored_vwap::BEFORE_HISTORY));
        c.auction_origin(&l, false);
        assert_eq!(c.pending_jump_ts, Some(l.origin_open_ts));
    }
    #[test]
    fn tag_click_is_not_seen_by_an_active_drawing_tool() {
        let ctx = egui::Context::default();
        let mut c = fixture();
        ready(&mut c);
        geometry(&mut c);
        let tag = c.auction.paint.hits.iter().find(|h| h.level.is_some()).unwrap().rect.center();
        c.draw.tool = Some(Tool::Trendline);
        let mut consumed = false;
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800., 600.))),
                ..Default::default()
            },
            |ui| {
                ui.interact(
                    Rect::from_min_size(Pos2::ZERO, vec2(780., 500.)),
                    ui.id().with("plot"),
                    Sense::click_and_drag(),
                );
            },
        )
        .textures_delta
        .clear();
        for pressed in [true, false] {
            let mut out = ctx.run_ui(
                egui::RawInput {
                    events: vec![
                        egui::Event::PointerMoved(tag),
                        egui::Event::PointerButton {
                            pos: tag,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800., 600.))),
                    ..Default::default()
                },
                |ui| {
                    let rect = Rect::from_min_size(Pos2::ZERO, vec2(780., 500.));
                    let response = ui.interact(rect, ui.id().with("plot"), Sense::click_and_drag());
                    let owner = c.cockpit_input(ui, &response, rect, &c.viewport.unwrap());
                    if owner.is_none() && c.auction_click(ui, &response) {
                        consumed = true;
                    }
                },
            );
            out.textures_delta.clear();
        }
        assert!(consumed);
        assert!(c.drawings.is_empty());
    }
    #[test]
    fn partial_settings_clamp_and_view_only_commands_reuse_the_worker_report() {
        let (w, mut rx) = Worker::test_channel();
        let mut c = fixture();
        c.restore_auction(Some(AuctionSettings { composite: 255, value_area: 0, max_age: 0, ..Default::default() }));
        assert_eq!(c.auction.view.value_area, 50);
        assert_eq!(c.auction.view.composite, 20);
        assert_eq!(c.auction.view.max_age, 1);
        let _ = c.request_auction(&w);
        requests(&mut rx);
        c.set_auction(AuctionCmd::Composite(0), &w);
        c.set_auction(AuctionCmd::Strips(None), &w);
        c.set_auction(AuctionCmd::Filled(None), &w);
        assert!(requests(&mut rx).is_empty());
    }
    #[test]
    fn auction_prepare_performance() {
        let mut c = fixture();
        let mut date = "2026-09-01".parse().unwrap();
        let mut bars = vec![];
        for day in 0..20 {
            bars.extend(tr_data::intraday::regular_opens(date, Timeframe::M1).into_iter().enumerate().map(
                |(i, ts)| {
                    let p = 100. + day as f64 * 0.2 + (i as f64 * 0.07).sin();
                    Bar::new(ts, p, p + 0.12, p - 0.09, p + 0.02, 100. + (i % 37) as f64 * 10.)
                        .with_provenance(Provenance::Real)
                },
            ));
            date = tr_data::history::calendar::next_session(date);
        }
        c.tf = Timeframe::M1;
        c.bars = Arc::new(bars);
        c.last_refresh = Some(c.bars.last().unwrap().ts + 60);
        c.viewport = Some(Viewport { right: c.bars.len() as f64, bar_px: 780. / c.bars.len() as f64, width_px: 780. });
        ready(&mut c);
        let start = Instant::now();
        for _ in 0..100 {
            c.auction.paint = Default::default();
            geometry(&mut c);
        }
        let build = start.elapsed() / 100;
        let start = Instant::now();
        for _ in 0..1000 {
            geometry(&mut c);
        }
        eprintln!("Auction geometry, 20 sessions: rebuild {build:?}; cached {:?}", start.elapsed() / 1000);
    }
    #[test]
    fn auction_response_received_while_hidden_is_ready_when_reenabled() {
        let (w, mut rx) = Worker::test_channel();
        let mut c = fixture();
        let _ = c.request_auction(&w);
        let key = requests(&mut rx).pop().unwrap();
        c.set_auction(AuctionCmd::Toggle(Some(false)), &w);
        c.on_auction_map(key, Ok(report(&c)));
        c.set_auction(AuctionCmd::Toggle(Some(true)), &w);
        assert!(c.auction.result.as_ref().is_some_and(Result::is_ok));
        assert!(requests(&mut rx).is_empty());
    }
    #[test]
    fn naked_tags_step_left_of_an_avwap_price_tag_on_the_same_row() {
        let ctx = egui::Context::default();
        let mut c = fixture();
        ready(&mut c);
        // Anchored at the first open, the AVWAP sits at ~103, on Monday's nPOC.
        c.avwap_anchors = vec![c.bars[0].ts];
        let key = c.avwap_key(chrono::Utc::now().timestamp()).unwrap();
        let report = tr_data::anchored_vwap::build(&c.bars, &key.anchors, key.closed_len);
        c.on_anchored_vwap(key, Ok(Arc::new(report)));
        ctx.run_ui(Default::default(), |ui| {
            let rect = Rect::from_min_size(Pos2::ZERO, vec2(780., 500.));
            let scale = PriceScale::padded(98., 108., 0.01, false, 500.);
            c.paint_avwap(ui, rect, &c.viewport.unwrap(), &scale, None);
        })
        .textures_delta
        .clear();
        let avwap = c.avwap_tag_rects();
        assert_eq!(avwap.len(), 1);
        geometry(&mut c);
        let tags: Vec<_> = c.auction.paint.hits.iter().filter(|h| h.level.is_some()).map(|h| h.rect).collect();
        assert!(!tags.is_empty());
        assert!(tags.iter().all(|t| !t.intersects(avwap[0])), "{tags:?} vs {avwap:?}");
    }
    #[test]
    fn footer_rides_above_the_vault_label_and_narrow_strips_use_one_letter_tags() {
        let mut c = fixture();
        Arc::make_mut(&mut c.bars).remove(3);
        ready(&mut c);
        assert_eq!(c.auction_footer_room(), 38.);
        c.stitch = Some(Arc::new(tr_data::vault::StitchInfo {
            cells: vec![tr_data::vault::SessionCell {
                date: "2026-11-10".parse().unwrap(),
                state: tr_data::vault::CellState::Complete,
                expected: 78,
                present: 78,
                holes: 0,
                fetch_ok: 1,
                fetch_fail: 0,
                corrected: 0,
                last_seen: None,
                half_day: false,
            }],
            ..Default::default()
        }));
        assert_eq!(c.auction_footer_room(), 54.);
        geometry(&mut c);
        let footer = c.auction.paint.footer_rect.unwrap();
        assert!(footer.bottom() <= 500. - 22.);
        assert!(c.auction.paint.labels.iter().any(|(_, s, _)| s == "PARTIAL 77/78"));
        // ~20 px per session: the partial strip is tagged "P", never the long tag.
        c.viewport = Some(Viewport { right: c.bars.len() as f64 + 8., bar_px: 0.26, width_px: 780. });
        geometry(&mut c);
        assert!(c.auction.paint.labels.iter().any(|(_, s, _)| s == "P"));
        assert!(!c.auction.paint.labels.iter().any(|(_, s, _)| s.starts_with("PARTIAL")));
    }
    #[test]
    fn legend_line_names_the_latest_session_value_and_open_location() {
        let mut c = fixture();
        assert!(c.auction_legend_line().is_none());
        ready(&mut c);
        let line = c.auction_legend_line().unwrap();
        assert!(line.starts_with("AMAP 11-10 POC "), "{line}");
        assert!(line.contains("OPEN OUTSIDE RANGE"), "{line}");
    }
    #[test]
    fn non_us_cash_charts_refuse_locally_without_a_fine_fetch() {
        let (w, mut rx) = Worker::test_channel();
        let mut c = ChartState::new(9, "VOD.L", Timeframe::M5);
        c.restore_auction(Some(Default::default()));
        let _ = c.request_auction(&w);
        assert!(requests(&mut rx).is_empty());
        assert_eq!(c.auction.result.as_ref().unwrap().as_ref().unwrap_err(), "Auction Map needs a US cash session");
    }
}
