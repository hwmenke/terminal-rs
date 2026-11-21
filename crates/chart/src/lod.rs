//! Level-of-detail pyramids. Level `k` merges `2^k` consecutive bars, so drawing any
//! zoom level costs O(plot width) instances instead of O(visible bars): at 1M bars fully
//! zoomed out we render ~800 merged candles, not a million.

use bytemuck::{Pod, Zeroable};
use tr_core::Bar;

/// One instance as uploaded to the GPU storage buffer (std430: 8 x f32).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct GpuBar {
    /// Index of the first level-0 bar covered.
    pub x: f32,
    /// Number of level-0 bars covered.
    pub w: f32,
    pub open: f32,
    pub high: f32,
    pub low: f32,
    pub close: f32,
    pub volume: f32,
    pub _pad: f32,
}

// Keep missing distinct from zero while preventing finite f64 volumes from
// overflowing GPU storage or sums into infinity.
fn gpu_volume(volume: Option<f64>) -> f32 {
    volume.map_or(f32::NAN, |v| v.min(f32::MAX as f64) as f32)
}

fn known_volume(volume: f32) -> Option<f64> {
    (volume.is_finite() && volume >= 0.0).then_some(volume as f64)
}

/// Target minimum on-screen width of one drawn instance.
pub const MIN_INSTANCE_PX: f64 = 2.0;

/// Pick the pyramid level for a zoom of `bar_px` pixels per level-0 bar.
pub fn level_for(bar_px: f64, levels: usize) -> usize {
    if bar_px >= MIN_INSTANCE_PX || levels <= 1 {
        return 0;
    }
    let k = (MIN_INSTANCE_PX / bar_px).log2().ceil().max(0.0) as usize;
    k.min(levels - 1)
}

#[derive(Debug, Clone, Default)]
pub struct BarPyramid {
    /// All levels concatenated, level 0 first.
    pub data: Vec<GpuBar>,
    /// Start offset of each level within `data`.
    pub offsets: Vec<usize>,
    pub len0: usize,
}

impl BarPyramid {
    pub fn build(bars: &[Bar]) -> Self {
        let mut data: Vec<GpuBar> = bars
            .iter()
            .enumerate()
            .map(|(i, b)| GpuBar {
                x: i as f32,
                w: 1.0,
                open: b.open as f32,
                high: b.high as f32,
                low: b.low as f32,
                close: b.close as f32,
                volume: gpu_volume(b.volume_value()),
                _pad: 0.0,
            })
            .collect();
        let mut offsets = vec![0];
        let (mut start, mut n) = (0, bars.len());
        while n > 1 {
            let next_start = data.len();
            for j in (0..n).step_by(2) {
                let a = data[start + j];
                let merged = if j + 1 < n {
                    let b = data[start + j + 1];
                    GpuBar {
                        x: a.x,
                        w: a.w + b.w,
                        open: a.open,
                        high: a.high.max(b.high),
                        low: a.low.min(b.low),
                        close: b.close,
                        volume: gpu_volume(match (known_volume(a.volume), known_volume(b.volume)) {
                            (Some(a), Some(b)) => Some(a + b),
                            (a, b) => a.or(b),
                        }),
                        _pad: 0.0,
                    }
                } else {
                    a
                };
                data.push(merged);
            }
            offsets.push(next_start);
            start = next_start;
            n = n.div_ceil(2);
        }
        Self { data, offsets, len0: bars.len() }
    }

    pub fn levels(&self) -> usize {
        self.offsets.len()
    }

    pub fn level(&self, k: usize) -> &[GpuBar] {
        let start = self.offsets[k];
        let end = self.offsets.get(k + 1).copied().unwrap_or(self.data.len());
        &self.data[start..end]
    }

    /// Absolute instance range in `data` covering level-0 indices `[lo, hi)` at level `k`.
    pub fn instance_range(&self, k: usize, lo: usize, hi: usize) -> (u32, u32) {
        let n = self.level(k).len();
        let a = (lo >> k).min(n);
        let b = hi.div_ceil(1 << k).min(n);
        let off = self.offsets[k];
        ((off + a) as u32, (off + b.max(a)) as u32)
    }

    /// Low/high and max volume over level-0 range `[lo, hi)` using level `k`.
    pub fn extent(&self, k: usize, lo: usize, hi: usize) -> Option<(f64, f64, f64)> {
        let (a, b) = self.instance_range(k, lo, hi);
        let slice = &self.data[a as usize..b as usize];
        if slice.is_empty() {
            return None;
        }
        let (mut l, mut h, mut v) = (f32::INFINITY, f32::NEG_INFINITY, 0.0f32);
        for g in slice {
            l = l.min(g.low);
            h = h.max(g.high);
            if let Some(volume) = known_volume(g.volume) {
                v = v.max(volume as f32);
            }
        }
        Some((l as f64, h as f64, v as f64))
    }
}

/// A value series (indicator, line chart) decimated per level; each point is
/// `(level-0 index of the point's bar centre, value)`. NaN values break the line.
#[derive(Debug, Clone, Default)]
pub struct LinePyramid {
    pub data: Vec<[f32; 2]>,
    pub offsets: Vec<usize>,
}

impl LinePyramid {
    /// Keeps the later point of each pair (its value is the group's close).
    pub fn build(values: &[f64]) -> Self {
        Self::reduce(values, |_, later| later)
    }

    /// For sparse markers: a pair whose later point is blank keeps the earlier one, so
    /// markers survive zooming out.
    pub fn build_markers(values: &[f64]) -> Self {
        Self::reduce(values, |earlier, later| if later[1].is_nan() { earlier } else { later })
    }

    fn reduce(values: &[f64], merge: impl Fn([f32; 2], [f32; 2]) -> [f32; 2]) -> Self {
        let mut data: Vec<[f32; 2]> = values.iter().enumerate().map(|(i, v)| [i as f32 + 0.5, *v as f32]).collect();
        let mut offsets = vec![0];
        let (mut start, mut n) = (0, values.len());
        while n > 1 {
            let next_start = data.len();
            for j in (0..n).step_by(2) {
                let p = if j + 1 < n { merge(data[start + j], data[start + j + 1]) } else { data[start + j] };
                data.push(p);
            }
            offsets.push(next_start);
            start = next_start;
            n = n.div_ceil(2);
        }
        Self { data, offsets }
    }

    pub fn levels(&self) -> usize {
        self.offsets.len()
    }

    pub fn level_len(&self, k: usize) -> usize {
        self.offsets.get(k + 1).copied().unwrap_or(self.data.len()) - self.offsets[k]
    }

    /// Absolute segment-instance range: segment `i` joins points `i` and `i+1`.
    /// One extra point on each side keeps lines continuous to the plot edges.
    pub fn segment_range(&self, k: usize, lo: usize, hi: usize) -> (u32, u32) {
        let n = self.level_len(k);
        if n < 2 {
            return (0, 0);
        }
        let a = (lo >> k).saturating_sub(1).min(n - 1);
        let b = (hi.div_ceil(1 << k) + 1).min(n - 1);
        let off = self.offsets[k];
        ((off + a) as u32, (off + b.max(a)) as u32)
    }

    /// Finite min/max over level-0 range `[lo, hi)` at level `k`.
    pub fn extent(&self, k: usize, lo: usize, hi: usize) -> Option<(f64, f64)> {
        let n = self.level_len(k);
        let a = (lo >> k).min(n);
        let b = hi.div_ceil(1 << k).min(n);
        let off = self.offsets[k];
        let mut out: Option<(f64, f64)> = None;
        for p in &self.data[off + a..off + b] {
            let v = p[1] as f64;
            if v.is_finite() {
                out = Some(out.map_or((v, v), |(l, h)| (l.min(v), h.max(v))));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars(n: usize) -> Vec<Bar> {
        (0..n).map(|i| Bar::new(i as i64, i as f64, i as f64 + 2.0, i as f64 - 1.0, i as f64 + 1.0, 1.0)).collect()
    }

    #[test]
    fn missing_volume_does_not_poison_pyramid_or_extent() {
        let mut input = bars(5);
        for (b, v) in input.iter_mut().zip([f64::NAN, f64::NAN, 0.0, 7.0, f64::NAN]) {
            b.volume = v;
        }
        let p = BarPyramid::build(&input);
        assert!(p.level(1)[0].volume.is_nan());
        assert_eq!(p.level(1)[1].volume, 7.0);
        assert_eq!(p.level(3)[0].volume, 7.0);
        assert_eq!(p.extent(0, 0, 2).unwrap().2, 0.0);
        assert_eq!(p.extent(0, 0, 5).unwrap().2, 7.0);
        input[0].volume = f64::MAX;
        input[1].volume = f64::MAX;
        let p = BarPyramid::build(&input);
        assert_eq!(p.level(0)[0].volume, f32::MAX);
        assert_eq!(p.level(3)[0].volume, f32::MAX);
    }

    #[test]
    fn pyramid_levels_merge_ohlcv() {
        let p = BarPyramid::build(&bars(5));
        assert_eq!(p.levels(), 4); // 5, 3, 2, 1
        let l1 = p.level(1);
        assert_eq!(l1.len(), 3);
        assert_eq!(
            l1[0],
            GpuBar { x: 0.0, w: 2.0, open: 0.0, high: 3.0, low: -1.0, close: 2.0, volume: 2.0, _pad: 0.0 }
        );
        assert_eq!(l1[2].x, 4.0);
        assert_eq!(l1[2].w, 1.0);
        let top = p.level(3);
        assert_eq!(top.len(), 1);
        assert_eq!((top[0].w, top[0].low, top[0].high, top[0].volume), (5.0, -1.0, 6.0, 5.0));
    }

    #[test]
    fn level_choice_bounds_instance_count() {
        let p = BarPyramid::build(&bars(1 << 20));
        let width = 1600.0;
        for bar_px in [0.0015, 0.01, 0.3, 1.0, 1.99, 2.0, 25.0] {
            let k = level_for(bar_px, p.levels());
            let vis = (width / bar_px) as usize;
            let (a, b) = p.instance_range(k, 0, vis.min(p.len0));
            assert!((b - a) as f64 <= width / MIN_INSTANCE_PX + 2.0 || k == 0, "bar_px {bar_px}: {}", b - a);
            if bar_px >= 2.0 {
                assert_eq!(k, 0);
            }
        }
    }

    #[test]
    fn extent_matches_brute_force() {
        let p = BarPyramid::build(&bars(1000));
        let (l, h, _) = p.extent(0, 100, 200).unwrap();
        assert_eq!((l, h), (99.0, 201.0));
        // Coarser levels may widen the range slightly but always contain it.
        let (l3, h3, _) = p.extent(3, 100, 200).unwrap();
        assert!(l3 <= 99.0 && h3 >= 201.0);
    }

    #[test]
    fn line_pyramid() {
        let mut v: Vec<f64> = (0..9).map(|i| i as f64).collect();
        v[0] = f64::NAN;
        let p = LinePyramid::build(&v);
        assert_eq!(p.level_len(1), 5);
        assert_eq!(p.data[p.offsets[1]], [1.5, 1.0]);
        assert_eq!(p.extent(0, 0, 9), Some((1.0, 8.0)));
        let (a, b) = p.segment_range(0, 3, 6);
        assert_eq!((a, b), (2, 7));
        assert_eq!(p.segment_range(0, 0, 0), (0, 1));
    }

    #[test]
    fn marker_pyramid_keeps_sparse_points() {
        let mut v = vec![f64::NAN; 16];
        v[2] = 5.0;
        v[9] = 7.0;
        let lines = LinePyramid::build(&v);
        assert_eq!(lines.extent(1, 0, 16), Some((7.0, 7.0)), "lines keep the later point");
        let marks = LinePyramid::build_markers(&v);
        // Both survive every level where they fall in different groups, at their own bars.
        for k in 0..marks.levels() - 1 {
            assert_eq!(marks.extent(k, 0, 16), Some((5.0, 7.0)), "level {k}");
            let level = &marks.data[marks.offsets[k]..marks.offsets[k + 1]];
            assert!(level.contains(&[2.5, 5.0]) && level.contains(&[9.5, 7.0]), "level {k}");
        }
        assert_eq!(marks.data[marks.offsets[marks.levels() - 1]], [9.5, 7.0]);
    }
}
