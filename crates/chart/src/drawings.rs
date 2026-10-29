//! User drawings. Anchors are stored as (timestamp, price) so they survive pan, zoom and
//! timeframe changes; they are mapped to bar indices only when drawn.
//!
//! Live drawings sit on the current pane; the rest of a review session lives in
//! [`AnnotationStore`], keyed by canonical security so stepping AAPL → MSFT → AAPL
//! does not throw the first name's levels away. Daily, weekly and monthly marks stay
//! on that security key. Intraday interval-specific marks (trendlines, verticals, fibs)
//! are stored under the security plus the timeframe. Horizontals stay on the security
//! so they still carry across intervals, including layouts saved before that split.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tr_core::Timeframe;

pub const FIB_LEVELS: [f64; 7] = [0.0, 0.236, 0.382, 0.5, 0.618, 0.786, 1.0];

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Anchor {
    pub ts: i64,
    pub price: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tool {
    Trendline,
    Horizontal,
    Vertical,
    Fib,
}

impl Tool {
    pub fn points(self) -> usize {
        match self {
            Tool::Horizontal | Tool::Vertical => 1,
            Tool::Trendline | Tool::Fib => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tool::Trendline => "Trendline",
            Tool::Horizontal => "Horizontal line",
            Tool::Vertical => "Vertical line",
            Tool::Fib => "Fib retracement",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Drawing {
    pub tool: Tool,
    pub a: Anchor,
    pub b: Anchor,
}

impl Drawing {
    pub fn new(tool: Tool, a: Anchor, b: Anchor) -> Self {
        Self { tool, a, b }
    }

    /// Fib levels as `(ratio, price)`, measured from `b` (0) back to `a` (1), the
    /// usual retracement convention for a swing drawn from `a` to `b`.
    pub fn fib_levels(&self) -> Vec<(f64, f64)> {
        FIB_LEVELS.iter().map(|&r| (r, self.b.price + (self.a.price - self.b.price) * r)).collect()
    }

    /// Price of a horizontal line, for PM Desk user-stop candidates.
    pub fn horizontal_price(&self) -> Option<f64> {
        (self.tool == Tool::Horizontal && self.a.price.is_finite() && self.a.price > 0.0).then_some(self.a.price)
    }

    /// Price-only drawings that stay meaningful on another interval of the same name
    /// (daily ↔ weekly review). Trendlines, verticals and fibs stay on the pane that
    /// drew them — their timestamps are interval-specific.
    pub fn is_compatible_level(&self) -> bool {
        self.horizontal_price().is_some()
    }

    fn same_compatible_level(&self, other: &Drawing) -> bool {
        match (self.horizontal_price(), other.horizontal_price()) {
            (Some(a), Some(b)) => (a - b).abs() < 1e-9,
            _ => false,
        }
    }
}

/// Canonical store key for a security (`AAPL` and `AAPL US Equity` collapse).
/// The chart crate has no split/dividend basis toggle — one price series per name.
pub fn store_key(symbol: &str) -> String {
    tr_core::Security::parse(symbol).map(|s| s.key()).unwrap_or_else(|| symbol.trim().to_ascii_uppercase())
}

/// Store key for one chart view. Non-intraday timeframes share [`store_key`] so a
/// layout written before intraday views still round-trips. Intraday views add the
/// timeframe label (`AAPL US Equity|5m`).
pub fn view_key(symbol: &str, tf: Timeframe) -> String {
    let base = store_key(symbol);
    if base.is_empty() || !tf.is_intraday() { base } else { format!("{base}|{}", tf.label()) }
}

/// Per-security drawings that outlive the live pane. Empty keys are dropped so a
/// layout does not grow a hole for every name the user merely passed through.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AnnotationStore {
    entries: BTreeMap<String, Vec<Drawing>>,
}

impl AnnotationStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, symbol: &str) -> Vec<Drawing> {
        self.entries.get(&store_key(symbol)).cloned().unwrap_or_default()
    }

    /// Drawings for this symbol at `tf`. Intraday views show their own interval-specific
    /// marks plus the symbol's horizontals. Daily, weekly and monthly return the symbol entry.
    pub fn get_view(&self, symbol: &str, tf: Timeframe) -> Vec<Drawing> {
        if !tf.is_intraday() {
            return self.get(symbol);
        }
        let mut out = self.compatible_levels(symbol);
        if let Some(specific) = self.entries.get(&view_key(symbol, tf)) {
            out.extend(specific.iter().filter(|d| !d.is_compatible_level()).cloned());
        }
        out
    }

    /// Remember `drawings` for `symbol`. An empty list forgets the name.
    pub fn stash(&mut self, symbol: &str, drawings: &[Drawing]) {
        let key = store_key(symbol);
        if key.is_empty() {
            return;
        }
        if drawings.is_empty() {
            self.entries.remove(&key);
        } else {
            self.entries.insert(key, drawings.to_vec());
        }
    }

    /// Remember `drawings` for this symbol and timeframe.
    /// Horizontals always update the symbol entry. Intraday trendlines, verticals and
    /// fibs update only that timeframe's entry, leaving the daily marks in place.
    pub fn stash_view(&mut self, symbol: &str, tf: Timeframe, drawings: &[Drawing]) {
        let key = store_key(symbol);
        if key.is_empty() {
            return;
        }
        if !tf.is_intraday() {
            self.stash(symbol, drawings);
            return;
        }
        let (levels, specific): (Vec<Drawing>, Vec<Drawing>) =
            drawings.iter().cloned().partition(|d| d.is_compatible_level());
        let vk = view_key(symbol, tf);
        if specific.is_empty() {
            self.entries.remove(&vk);
        } else {
            self.entries.insert(vk, specific);
        }
        let mut kept = self.entries.get(&key).cloned().unwrap_or_default();
        kept.retain(|d| !d.is_compatible_level());
        kept.extend(levels);
        if kept.is_empty() {
            self.entries.remove(&key);
        } else {
            self.entries.insert(key, kept);
        }
    }

    /// Legacy layouts only stored the live pane's drawings. Fold those into the
    /// store when this name has no entry yet.
    pub fn seed_if_absent(&mut self, symbol: &str, drawings: &[Drawing]) {
        let key = store_key(symbol);
        if key.is_empty() || drawings.is_empty() || self.entries.contains_key(&key) {
            return;
        }
        self.entries.insert(key, drawings.to_vec());
    }

    /// Fold legacy pane drawings into the store when this view has no entry yet.
    /// Non-intraday layouts keep the symbol key. Intraday layouts put interval-specific
    /// marks on the timeframe key and horizontals on the symbol, without replacing marks
    /// already stored for that name.
    pub fn seed_view_if_absent(&mut self, symbol: &str, tf: Timeframe, drawings: &[Drawing]) {
        if !tf.is_intraday() {
            self.seed_if_absent(symbol, drawings);
            return;
        }
        let vk = view_key(symbol, tf);
        if !vk.is_empty() {
            let specific: Vec<Drawing> = drawings.iter().filter(|d| !d.is_compatible_level()).cloned().collect();
            if !specific.is_empty() {
                self.entries.entry(vk).or_insert(specific);
            }
        }
        let base = store_key(symbol);
        if base.is_empty() {
            return;
        }
        if self.entries.contains_key(&base) {
            self.merge_compatible_levels(symbol, drawings);
            return;
        }
        let levels: Vec<Drawing> = drawings.iter().filter(|d| d.is_compatible_level()).cloned().collect();
        if !levels.is_empty() {
            self.entries.entry(base).or_insert(levels);
        }
    }

    pub fn compatible_levels(&self, symbol: &str) -> Vec<Drawing> {
        self.get(symbol).into_iter().filter(|d| d.is_compatible_level()).collect()
    }

    /// Add incoming horizontals that this name does not already have (same price).
    pub fn merge_compatible_levels(&mut self, symbol: &str, incoming: &[Drawing]) {
        let key = store_key(symbol);
        if key.is_empty() {
            return;
        }
        let dest = self.entries.entry(key).or_default();
        for d in incoming.iter().filter(|d| d.is_compatible_level()) {
            if !dest.iter().any(|e| e.same_compatible_level(d)) {
                dest.push(d.clone());
            }
        }
        if dest.is_empty() {
            self.entries.remove(&store_key(symbol));
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Vec<Drawing>)> {
        self.entries.iter()
    }
}

/// Screen-space geometry of a drawing, produced by the chart for hit testing.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    Segment([f64; 2], [f64; 2]),
    HLine(f64),
    VLine(f64),
}

/// Distance in pixels from point `p` to a shape.
pub fn distance(shape: &Shape, p: [f64; 2]) -> f64 {
    match shape {
        Shape::HLine(y) => (p[1] - y).abs(),
        Shape::VLine(x) => (p[0] - x).abs(),
        Shape::Segment(a, b) => {
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let len2 = dx * dx + dy * dy;
            let t = if len2 == 0.0 { 0.0 } else { (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0) };
            let (cx, cy) = (a[0] + t * dx, a[1] + t * dy);
            ((p[0] - cx).powi(2) + (p[1] - cy).powi(2)).sqrt()
        }
    }
}

/// Snap `price` to the nearest of a bar's O/H/L/C (magnet mode).
pub fn magnet(price: f64, ohlc: [f64; 4]) -> f64 {
    ohlc.into_iter().min_by(|a, b| (a - price).abs().total_cmp(&(b - price).abs())).unwrap_or(price)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fib_levels_retrace_from_b() {
        let d = Drawing::new(Tool::Fib, Anchor { ts: 0, price: 100.0 }, Anchor { ts: 1, price: 200.0 });
        let lv = d.fib_levels();
        assert_eq!(lv[0], (0.0, 200.0));
        assert_eq!(lv[3], (0.5, 150.0));
        assert!((lv[4].1 - 138.2).abs() < 1e-9);
        assert_eq!(lv[6], (1.0, 100.0));
    }

    #[test]
    fn hit_distances() {
        assert_eq!(distance(&Shape::Segment([0.0, 0.0], [10.0, 0.0]), [5.0, 3.0]), 3.0);
        assert_eq!(distance(&Shape::Segment([0.0, 0.0], [10.0, 0.0]), [13.0, 4.0]), 5.0);
        assert_eq!(distance(&Shape::HLine(50.0), [999.0, 47.0]), 3.0);
        assert_eq!(distance(&Shape::VLine(10.0), [12.0, 0.0]), 2.0);
    }

    #[test]
    fn magnet_snaps() {
        assert_eq!(magnet(10.4, [10.0, 12.0, 9.0, 11.0]), 10.0);
        assert_eq!(magnet(11.7, [10.0, 12.0, 9.0, 11.0]), 12.0);
    }

    #[test]
    fn horizontal_price_is_the_line_level() {
        let h = Drawing::new(Tool::Horizontal, Anchor { ts: 1, price: 42.5 }, Anchor { ts: 1, price: 42.5 });
        assert_eq!(h.horizontal_price(), Some(42.5));
        let t = Drawing::new(Tool::Trendline, Anchor { ts: 0, price: 10.0 }, Anchor { ts: 1, price: 20.0 });
        assert_eq!(t.horizontal_price(), None);
    }

    fn hline(price: f64) -> Drawing {
        Drawing::new(Tool::Horizontal, Anchor { ts: 0, price }, Anchor { ts: 0, price })
    }

    fn trend(ts: i64) -> Drawing {
        Drawing::new(Tool::Trendline, Anchor { ts, price: 10.0 }, Anchor { ts: ts + 1, price: 20.0 })
    }

    #[test]
    fn store_survives_a_to_b_to_a_and_keeps_b_empty() {
        let mut store = AnnotationStore::new();
        let aapl = hline(150.0);
        store.stash("AAPL", &[aapl.clone()]);
        assert_eq!(store.get("AAPL US Equity"), vec![aapl.clone()]);
        assert!(store.get("MSFT").is_empty(), "B must not inherit A's drawings");
        store.stash("MSFT", &[]);
        assert!(store.get("MSFT US Equity").is_empty());
        assert_eq!(store.get("aapl"), vec![aapl], "returning to A restores its levels");
    }

    #[test]
    fn store_merges_horizontals_but_not_trendlines() {
        let mut store = AnnotationStore::new();
        store.stash("AAPL", &[hline(100.0), trend(1)]);
        store.merge_compatible_levels("AAPL US Equity", &[hline(100.0), hline(110.0), trend(9)]);
        let got = store.get("AAPL");
        assert_eq!(got.iter().filter(|d| d.is_compatible_level()).count(), 2);
        assert!(got.iter().any(|d| d.horizontal_price() == Some(110.0)));
        assert_eq!(got.iter().filter(|d| d.tool == Tool::Trendline).count(), 1);
    }

    #[test]
    fn seed_if_absent_does_not_overwrite_and_empty_stash_forgets() {
        let mut store = AnnotationStore::new();
        store.seed_if_absent("AAPL", &[hline(1.0)]);
        store.seed_if_absent("AAPL", &[hline(2.0)]);
        assert_eq!(store.get("AAPL")[0].horizontal_price(), Some(1.0));
        store.stash("AAPL", &[]);
        assert!(store.is_empty());
    }

    #[test]
    fn intraday_marks_are_keyed_by_timeframe_and_daily_levels_stay() {
        let mut store = AnnotationStore::new();
        let daily_trend = trend(1);
        store.stash("AAPL", &[hline(150.0), daily_trend.clone()]);
        store.stash_view("AAPL", Timeframe::M5, &[hline(150.0), trend(50)]);
        let m5 = store.get_view("AAPL US Equity", Timeframe::M5);
        assert!(m5.iter().any(|d| d.horizontal_price() == Some(150.0)));
        assert!(m5.iter().any(|d| d.tool == Tool::Trendline && d.a.ts == 50));
        assert!(!m5.iter().any(|d| d.tool == Tool::Trendline && d.a.ts == 1));
        let daily = store.get_view("AAPL", Timeframe::D1);
        assert!(daily.iter().any(|d| d.tool == Tool::Trendline && d.a.ts == 1));
        assert!(!daily.iter().any(|d| d.a.ts == 50));
        assert_eq!(store.get("AAPL").iter().filter(|d| d.tool == Tool::Trendline).count(), 1);
        // A horizontal edit on 1h updates the shared level and does not remove the 5m trend.
        store.stash_view("aapl", Timeframe::H1, &[hline(160.0)]);
        assert!(store.get_view("AAPL", Timeframe::D1).iter().any(|d| d.horizontal_price() == Some(160.0)));
        assert!(!store.get_view("AAPL", Timeframe::D1).iter().any(|d| d.horizontal_price() == Some(150.0)));
        assert!(store.get_view("AAPL", Timeframe::M5).iter().any(|d| d.a.ts == 50));
        assert!(store.get_view("AAPL", Timeframe::M1).iter().any(|d| d.horizontal_price() == Some(160.0)));
        assert!(store.get_view("AAPL", Timeframe::M1).iter().all(|d| d.tool != Tool::Trendline));
        // The store is a string map, so the timeframe key round-trips with the layout JSON.
        assert!(store.iter().any(|(k, _)| k.ends_with("|5m")));
        assert!(store.iter().any(|(k, _)| !k.contains('|')));
    }

    #[test]
    fn legacy_symbol_keys_still_seed_an_intraday_view() {
        let mut store = AnnotationStore::new();
        store.seed_view_if_absent("AAPL", Timeframe::D1, &[hline(188.5), trend(3)]);
        assert_eq!(store.get("AAPL").len(), 2);
        store.seed_view_if_absent("AAPL", Timeframe::D1, &[hline(1.0)]);
        assert_eq!(store.get("AAPL").len(), 2, "a second seed must not replace the saved marks");
        let mut fresh = AnnotationStore::new();
        fresh.seed_view_if_absent("AAPL", Timeframe::M15, &[hline(188.5), trend(9)]);
        let view = fresh.get_view("AAPL", Timeframe::M15);
        assert!(view.iter().any(|d| d.horizontal_price() == Some(188.5)));
        assert!(view.iter().any(|d| d.a.ts == 9));
        assert!(fresh.get_view("AAPL", Timeframe::D1).iter().all(|d| d.tool != Tool::Trendline));
    }
}
