//! Completed native-bar regime lanes. No clock, fetching, price filling or resampling.
use super::{AdaptiveTrendParams, Ohlcv, adaptive_trend_with, require_adaptive_bars, session_starts};
use crate::{Bar, Provenance, Timeframe};

#[derive(Debug, Clone, PartialEq)]
pub enum LaneStatus {
    Ready,
    Warming { have: usize, need: usize },
    Refused(String),
    Stale { lag_secs: i64 },
    Unavailable(String),
}
#[derive(Debug, Clone)]
pub struct Lane {
    pub tf: Timeframe,
    pub provenance: Provenance,
    pub status: LaneStatus,
    pub state: Vec<i8>,
    pub conf: Vec<f32>,
    pub forming: Option<(i8, f32)>,
    pub since_ts: Option<i64>,
    pub last_bar_ts: Option<i64>,
    pub mrt: Option<f64>,
    /// Metadata of the actual completed native bar behind each cell, for hover.
    pub completed_ts: Vec<Option<i64>>,
    pub since: Vec<Option<i64>>,
    pub age: Vec<usize>,
    pub trails: Vec<f64>,
}
#[derive(Debug, Clone)]
pub struct StackScore {
    pub aligned: u8,
    pub scored: u8,
    pub dir: i8,
    pub provenance: Provenance,
}
#[derive(Debug, Clone)]
pub struct TrendStack {
    pub base_len: usize,
    pub base_last_ts: i64,
    pub lanes: Vec<Lane>,
    pub sigma: Vec<f64>,
    pub mb: Vec<f64>,
    pub confidence: Vec<f64>,
    pub medium: Vec<i8>,
    pub score: Option<StackScore>,
}

pub const LANE_MIN_BARS: usize = 60;
pub fn ladder(tf: Timeframe, archive: bool) -> Vec<Timeframe> {
    use Timeframe::*;
    let frames = match tf {
        M1 => vec![M1, M5, M15, H1],
        M5 => vec![M5, M15, H1, D1],
        M15 | M30 => vec![tf, H1, D1, W1],
        H1 => vec![H1, H4, D1, W1],
        H4 => vec![H4, D1, W1, MN1],
        D1 => vec![D1, W1, MN1],
        W1 => vec![W1, MN1],
        MN1 => vec![MN1],
    };
    frames.into_iter().filter(|t| !archive || !t.is_intraday()).collect()
}

/// Validate native intervals; token validation belongs to the command parser.
pub fn ladder_custom(base: Timeframe, frames: &[Timeframe], archive: bool) -> Result<Vec<Timeframe>, String> {
    if !(2..=4).contains(&frames.len())
        || frames.first() != Some(&base)
        || frames.windows(2).any(|w| w[0] >= w[1])
        || (archive && frames.iter().any(|tf| tf.is_intraday()))
    {
        return Err(
            "TSTACK expects 2–4 ascending native intervals starting at the chart interval; archive: 1D/1W/1M".into()
        );
    }
    Ok(frames.to_vec())
}

/// A native bar j becomes observable only at the open of its successor j+1.
/// The final native bar is never used here, including across session gaps.
pub fn align_completed<T: Copy>(base_ts: &[i64], lane_ts: &[i64], lane: &[T], empty: T) -> Vec<T> {
    let mut next = 1;
    let mut current = empty;
    base_ts
        .iter()
        .map(|t| {
            while next < lane_ts.len() && next - 1 < lane.len() && lane_ts[next] <= *t {
                current = lane[next - 1];
                next += 1;
            }
            current
        })
        .collect()
}

fn empty_lane(tf: Timeframe, len: usize) -> Lane {
    Lane {
        tf,
        provenance: Provenance::Missing,
        status: LaneStatus::Unavailable("No native bars".into()),
        state: vec![0; len],
        conf: vec![f32::NAN; len],
        forming: None,
        since_ts: None,
        last_bar_ts: None,
        mrt: None,
        completed_ts: vec![None; len],
        since: vec![None; len],
        age: vec![0; len],
        trails: vec![f64::NAN; len],
    }
}

pub fn trend_stack(
    base: &[Bar],
    base_tf: Timeframe,
    lanes: Vec<(Timeframe, Result<Vec<Bar>, String>)>,
    p: AdaptiveTrendParams,
) -> Result<TrendStack, String> {
    require_adaptive_bars(base, "Trend Stack")?;
    let base_ts: Vec<_> = base.iter().map(|b| b.ts).collect();
    let calculate = |bars: &[Bar], tf: Timeframe| {
        let mut params = p;
        params.session_atr = tf.is_intraday();
        params.session_reset = p.session_reset && tf == base_tf && tf.is_intraday();
        let starts = tf.is_intraday().then(|| {
            let ts: Vec<_> = bars.iter().map(|b| b.ts).collect();
            let mut starts = session_starts(&ts);
            if params.session_reset {
                for i in 1..ts.len() {
                    let wall = crate::ny::civil(ts[i]);
                    let open = crate::ny::to_utc(wall.date().and_hms_opt(9, 30, 0).unwrap());
                    starts[i] |= ts[i - 1] < open && ts[i] >= open;
                }
            }
            starts
        });
        adaptive_trend_with(&Ohlcv::new(bars), params, starts.as_deref())
    };
    let base_trend = calculate(base, base_tf);
    let mut all = Vec::with_capacity(lanes.len() + 1);
    // The base is exactly the pane's bars; no provider or display-cache substitution.
    for (tf, result) in
        std::iter::once((base_tf, Ok(base.to_vec()))).chain(lanes.into_iter().filter(|(tf, _)| *tf != base_tf))
    {
        let mut lane = empty_lane(tf, base.len());
        match result {
            Err(error) => {
                lane.status = if error.contains("SAMPLE") || error.contains("UNVERIFIED") || error.contains("refused") {
                    LaneStatus::Refused(error)
                } else {
                    LaneStatus::Unavailable(error)
                }
            }
            Ok(mut bars) => {
                // Refuse tainted input before deduplication can discard its metadata.
                lane.provenance = Bar::provenance(&bars);
                if !bars.is_empty()
                    && let Err(error) = require_adaptive_bars(&bars, "Trend Stack")
                {
                    lane.status = LaneStatus::Refused(error);
                    all.push(lane);
                    continue;
                }
                // Stable sort, then keep the last duplicate. Never fill a hole.
                bars.sort_by_key(|b| b.ts);
                let mut unique: Vec<Bar> = Vec::with_capacity(bars.len());
                for bar in bars {
                    if unique.last().is_some_and(|b| b.ts == bar.ts) {
                        *unique.last_mut().unwrap() = bar;
                    } else {
                        unique.push(bar);
                    }
                }
                let bars = unique;
                lane.provenance = Bar::provenance(&bars);
                match require_adaptive_bars(&bars, "Trend Stack") {
                    Err(error) => {
                        lane.status = if bars.is_empty() {
                            LaneStatus::Warming { have: 0, need: LANE_MIN_BARS }
                        } else {
                            LaneStatus::Refused(error)
                        }
                    }
                    Ok(()) => {
                        let t = if tf == base_tf { base_trend.clone() } else { calculate(&bars, tf) };
                        let ts: Vec<_> = bars.iter().map(|b| b.ts).collect();
                        let conf: Vec<_> = t.confidence.iter().map(|v| *v as f32).collect();
                        lane.state = align_completed(&base_ts, &ts, &t.medium_state, 0);
                        lane.conf = align_completed(&base_ts, &ts, &conf, f32::NAN);
                        lane.completed_ts =
                            align_completed(&base_ts, &ts, &ts.iter().copied().map(Some).collect::<Vec<_>>(), None);
                        lane.trails = align_completed(&base_ts, &ts, &t.mrt, f64::NAN);
                        let mut since = Vec::with_capacity(bars.len());
                        let mut age = Vec::with_capacity(bars.len());
                        let mut start = 0;
                        for i in 0..bars.len() {
                            if i == 0 || t.medium_state[i] != t.medium_state[i - 1] {
                                start = i;
                            }
                            since.push((t.medium_state[i] != 0).then_some(ts[start]));
                            age.push(i - start + 1);
                        }
                        lane.since = align_completed(&base_ts, &ts, &since, None);
                        lane.age = align_completed(&base_ts, &ts, &age, 0);
                        if p.session_reset && tf == base_tf && tf.is_intraday() {
                            for i in 0..base.len() {
                                if !t.confidence[i].is_finite() {
                                    lane.state[i] = 0;
                                    lane.conf[i] = f32::NAN;
                                    lane.completed_ts[i] = None;
                                    lane.since[i] = None;
                                    lane.age[i] = 0;
                                    lane.trails[i] = f64::NAN;
                                }
                            }
                        }
                        lane.forming = t.medium_state.last().copied().zip(conf.last().copied());
                        lane.last_bar_ts = ts.last().copied();
                        lane.since_ts = lane.since.last().copied().flatten();
                        lane.mrt = lane.trails.last().copied().filter(|v| v.is_finite());
                        lane.status = if bars.len() < LANE_MIN_BARS || !lane.conf.last().is_some_and(|v| v.is_finite())
                        {
                            LaneStatus::Warming { have: bars.len(), need: LANE_MIN_BARS }
                        } else {
                            LaneStatus::Ready
                        };
                    }
                }
            }
        }
        all.push(lane);
    }
    let score = score(&all);
    Ok(TrendStack {
        base_len: base.len(),
        base_last_ts: *base_ts.last().unwrap(),
        lanes: all,
        sigma: base_trend.sigma,
        mb: base_trend.mb,
        confidence: base_trend.confidence,
        medium: base_trend.medium_state,
        score,
    })
}

pub fn score(lanes: &[Lane]) -> Option<StackScore> {
    let base = lanes.first()?;
    if base.status != LaneStatus::Ready || !base.provenance.is_verified() {
        return None;
    }
    let dir = *base.state.last()?;
    let mut result = StackScore { aligned: 0, scored: 0, dir, provenance: Provenance::Real };
    for lane in lanes.iter().filter(|l| l.status == LaneStatus::Ready && l.provenance.is_verified()) {
        if let Some(state) = lane.state.last() {
            result.scored += 1;
            result.aligned += u8::from(*state == dir);
            result.provenance = result.provenance.combine(lane.provenance);
        }
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bars(n: usize, step: i64, provenance: Provenance) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let c = if i < 40 { 100.0 + (i as f64 * 0.4).sin() * 0.4 } else { 100.0 + (i - 40) as f64 * 1.2 };
                Bar::new(i as i64 * step, c, c + 0.2, c - 0.2, c, 1.0).with_provenance(provenance)
            })
            .collect()
    }
    fn stack(base: &[Bar], lanes: Vec<(Timeframe, Result<Vec<Bar>, String>)>) -> TrendStack {
        trend_stack(base, Timeframe::M5, lanes, Default::default()).unwrap()
    }
    #[test]
    fn session_anchor_resets_at_0930_and_keeps_higher_lanes_continuous() {
        let day = chrono::NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        let open = crate::ny::to_utc(day.and_hms_opt(9, 30, 0).unwrap());
        let next = crate::ny::to_utc((day + chrono::Duration::days(1)).and_hms_opt(8, 0, 0).unwrap());
        let mut base = bars(174, 300, Provenance::Real);
        for (i, b) in base.iter_mut().enumerate() {
            b.ts = if i < 78 { open + i as i64 * 300 } else { next + (i - 78) as i64 * 300 };
        }
        let native = bars(200, 3600, Provenance::Real);
        let plain = stack(&base, vec![(Timeframe::H1, Ok(native.clone()))]);
        let anchored = trend_stack(
            &base,
            Timeframe::M5,
            vec![(Timeframe::H1, Ok(native))],
            AdaptiveTrendParams { session_reset: true, ..Default::default() },
        )
        .unwrap();
        let reset = 96; // 18 premarket prints after 08:00, then 09:30.
        let fresh = stack(&base[reset..], vec![]);
        for (got, want) in
            [(&anchored.mb, &fresh.mb), (&anchored.sigma, &fresh.sigma), (&anchored.confidence, &fresh.confidence)]
        {
            assert_eq!(
                got[reset..].iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                want.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
            );
        }
        assert!(anchored.lanes[0].conf[reset..reset + 20].iter().all(|v| v.is_nan()));
        assert!(anchored.lanes[0].completed_ts[reset..reset + 20].iter().all(Option::is_none));
        assert_eq!(anchored.lanes[1].state, plain.lanes[1].state);
        assert_eq!(
            anchored.lanes[1].conf.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            plain.lanes[1].conf.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn custom_ladder_rejects_descending_duplicate_and_sub_base() {
        use Timeframe::*;
        assert_eq!(ladder_custom(M5, &[M5, M30, H4], false).unwrap(), vec![M5, M30, H4]);
        for frames in [vec![M5, H1, M30], vec![M5, M5], vec![M1, M5], vec![M5], vec![M5, M15, M30, H1, H4]] {
            assert!(ladder_custom(M5, &frames, false).is_err());
        }
        assert!(ladder_custom(M5, &[M5, D1], true).is_err());
        assert!(ladder_custom(D1, &[D1, W1, MN1], true).is_ok());
    }

    #[test]
    fn ladder_matches_the_documented_frames_and_archive_stays_daily_or_higher() {
        use Timeframe::*;
        let expected = [
            vec![M1, M5, M15, H1],
            vec![M5, M15, H1, D1],
            vec![M15, H1, D1, W1],
            vec![M30, H1, D1, W1],
            vec![H1, H4, D1, W1],
            vec![H4, D1, W1, MN1],
            vec![D1, W1, MN1],
            vec![W1, MN1],
            vec![MN1],
        ];
        for (tf, want) in Timeframe::ALL.into_iter().zip(expected) {
            assert_eq!(ladder(tf, false), want);
            assert!(ladder(tf, true).iter().all(|t| !t.is_intraday()));
        }
    }
    #[test]
    fn higher_lane_uses_completed_bars_only() {
        let base = bars(900, 300, Provenance::Real);
        let mut hourly = bars(76, 3600, Provenance::Real);
        let first = stack(&base, vec![(Timeframe::H1, Ok(hourly.clone()))]);
        let last = hourly.last_mut().unwrap();
        last.close = 1.0;
        last.open = 1.0;
        last.high = 1.2;
        last.low = 0.8;
        let second = stack(&base, vec![(Timeframe::H1, Ok(hourly))]);
        assert_eq!(first.lanes[1].state, second.lanes[1].state);
        assert_eq!(first.lanes[1].completed_ts, second.lanes[1].completed_ts);
        assert_eq!(
            first.lanes[1].conf.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            second.lanes[1].conf.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
        assert_ne!(first.lanes[1].forming, second.lanes[1].forming);
    }
    #[test]
    fn truncating_the_future_does_not_change_past_cells() {
        let base = bars(1000, 300, Provenance::Real);
        let hourly = bars(85, 3600, Provenance::Real);
        let full = stack(&base, vec![(Timeframe::H1, Ok(hourly.clone()))]);
        let cut = stack(&base[..900], vec![(Timeframe::H1, Ok(hourly[..76].to_vec()))]);
        for i in 0..2 {
            assert_eq!(full.lanes[i].state[..900], cut.lanes[i].state);
            assert_eq!(
                full.lanes[i].conf[..900].iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                cut.lanes[i].conf.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
            );
            assert_eq!(full.lanes[i].completed_ts[..900], cut.lanes[i].completed_ts);
            assert_eq!(full.lanes[i].since[..900], cut.lanes[i].since);
        }
    }
    #[test]
    fn cells_before_the_first_completed_lane_bar_are_empty() {
        assert_eq!(align_completed(&[0, 5, 10, 15, 20, 25], &[10, 20, 30], &[1, 2, 3], 0), vec![0, 0, 0, 0, 1, 1]);
    }
    #[test]
    fn native_timestamp_duplicates_keep_last_without_hiding_unverified_rows() {
        let base = bars(1000, 300, Provenance::Real);
        let native = bars(80, 3600, Provenance::Real);
        let mut duplicate = native.clone();
        duplicate.reverse();
        duplicate.push(native[30]);
        let expected = stack(&base, vec![(Timeframe::H1, Ok(native.clone()))]);
        let actual = stack(&base, vec![(Timeframe::H1, Ok(duplicate.clone()))]);
        assert_eq!(expected.lanes[1].state, actual.lanes[1].state);
        duplicate.push(native[30].with_provenance(Provenance::Sample));
        duplicate.push(native[30]);
        let refused = stack(&base, vec![(Timeframe::H1, Ok(duplicate))]);
        assert!(matches!(refused.lanes[1].status, LaneStatus::Refused(_)));
    }

    #[test]
    fn sample_base_refuses_the_whole_stack() {
        assert!(
            trend_stack(&bars(100, 300, Provenance::Sample), Timeframe::M5, vec![], Default::default())
                .unwrap_err()
                .contains("SAMPLE")
        );
    }
    #[test]
    fn unverified_or_sample_lane_is_refused_and_not_scored() {
        for p in [Provenance::Sample, Provenance::Missing] {
            let t = stack(&bars(1000, 300, Provenance::Real), vec![(Timeframe::H1, Ok(bars(80, 3600, p)))]);
            assert!(matches!(t.lanes[1].status, LaneStatus::Refused(_)));
            assert_eq!(t.score.as_ref().unwrap().scored, 1);
            assert!(t.score.unwrap().provenance.is_verified());
        }
    }
    #[test]
    fn missing_lane_is_unavailable_and_never_borrowed_from_the_base() {
        let t = stack(&bars(100, 300, Provenance::Real), vec![(Timeframe::D1, Err("offline".into()))]);
        assert!(matches!(t.lanes[1].status, LaneStatus::Unavailable(_)));
        assert!(t.lanes[1].state.iter().all(|s| *s == 0));
        assert!(t.lanes[1].conf.iter().all(|c| c.is_nan()));
    }
    #[test]
    fn warming_lane_is_not_scored() {
        let t = stack(&bars(100, 300, Provenance::Real), vec![(Timeframe::H1, Ok(bars(38, 3600, Provenance::Real)))]);
        assert!(matches!(t.lanes[1].status, LaneStatus::Warming { have: 38, need: 60 }));
        assert_eq!(t.score.unwrap().scored, 1);
    }
    #[test]
    fn single_ready_lane_never_reports_full_alignment_arrow() {
        let t = stack(&bars(100, 300, Provenance::Real), vec![]);
        assert_eq!(t.score.unwrap().scored, 1);
    }
    #[test]
    fn trend_stack_never_emits_a_price_outside_input_bars() {
        let base = bars(1000, 300, Provenance::Real);
        let native = bars(85, 3600, Provenance::Real);
        let t = stack(&base, vec![(Timeframe::H1, Ok(native.clone()))]);
        for (l, input) in t.lanes.iter().zip([&base, &native]) {
            assert_eq!(l.state.len(), base.len());
            assert_eq!(l.conf.len(), base.len());
            assert!(l.completed_ts.iter().flatten().all(|ts| input.iter().any(|b| b.ts == *ts)));
            let source = adaptive_trend_with(
                &Ohlcv::new(input),
                AdaptiveTrendParams { session_atr: true, ..Default::default() },
                Some(&session_starts(&input.iter().map(|b| b.ts).collect::<Vec<_>>())),
            );
            if let Some(mrt) = l.mrt {
                assert!(source.mrt.contains(&mrt));
            }
        }
    }
}
