//! Pure Trend Wall rows. Cells are observations, never fabricated price bars.
use chrono::Timelike;
use tr_core::indicators::{LaneStatus, Ohlcv, StackScore, TrendStack, atr_session, session_starts};
use tr_core::{Bar, Provenance, Timeframe};

use crate::intraday;

pub const MAX_SYMBOLS: usize = 24;
pub const SESSION_SLOTS: usize = 78;
pub const MIN_VOTES: usize = 5;

#[derive(Debug, Clone)]
pub struct Cell {
    pub state: i8,
    pub confidence: f32,
    pub native_ts: i64,
    pub provenance: Provenance,
}
#[derive(Debug, Clone)]
pub struct Ribbon {
    pub tf: Timeframe,
    pub status: LaneStatus,
    pub last_bar_ts: Option<i64>,
    pub cells: Vec<Option<Cell>>,
    pub forming: Option<(usize, i8, f32)>,
}
#[derive(Debug, Clone)]
pub struct Row {
    pub symbol: String,
    pub status: LaneStatus,
    pub score: Option<StackScore>,
    pub clock: Vec<i64>,
    pub lanes: Vec<Ribbon>,
    pub flip: Option<(usize, i8)>,
    pub cushion: f64,
    pub vwap: f64,
    pub rvol: f64,
    pub provenance: Provenance,
    pub last_bar_ts: Option<i64>,
    pub daily: bool,
    pub note: Option<String>,
}
impl Row {
    pub fn failure(symbol: impl Into<String>, status: LaneStatus, daily: bool) -> Self {
        Self {
            symbol: symbol.into(),
            status,
            score: None,
            clock: Vec::new(),
            lanes: Vec::new(),
            flip: None,
            cushion: f64::NAN,
            vwap: f64::NAN,
            rvol: f64::NAN,
            provenance: Provenance::Missing,
            last_bar_ts: None,
            daily,
            note: None,
        }
    }
    pub fn scored(&self) -> bool {
        self.status == LaneStatus::Ready
            && self.provenance.is_verified()
            && self.score.as_ref().is_some_and(|s| s.scored > 0 && s.provenance.is_verified())
    }
    /// Re-evaluate on the UI's existing tick too, so an in-flight refresh cannot leave a live vote behind.
    pub fn age(&mut self, now: i64) -> bool {
        if !self.provenance.is_verified() || matches!(self.status, LaneStatus::Refused(_)) {
            return false;
        }
        let lag = self
            .lanes
            .iter()
            .filter_map(|lane| {
                let ts = lane.last_bar_ts?;
                if lane.tf == Timeframe::D1 {
                    return daily_lag(ts, now);
                }
                match intraday::freshness(intraday::MarketClock::UsCash, lane.tf, ts, now) {
                    Some(intraday::Freshness::Stale { lag_secs }) => Some(lag_secs),
                    _ => None,
                }
            })
            .max()
            .or_else(|| {
                self.last_bar_ts.and_then(|ts| {
                    if !self.daily && tr_core::ny::date_key(ts) != tr_core::ny::date_key(now) {
                        Some(now.saturating_sub(ts).max(0))
                    } else {
                        None
                    }
                })
            });
        if let Some(lag_secs) = lag {
            let changed = !matches!(self.status, LaneStatus::Stale { .. });
            self.status = LaneStatus::Stale { lag_secs };
            self.clear_score();
            self.clear_ribbon();
            return changed;
        }
        false
    }
    pub fn clear_ribbon(&mut self) {
        for lane in &mut self.lanes {
            lane.cells.clear();
            lane.forming = None;
        }
    }
    pub fn failed_refresh(&mut self, error: String, now: i64) {
        self.note = Some(error);
        self.age(now);
        if matches!(self.status, LaneStatus::Stale { .. } | LaneStatus::Refused(_)) {
            self.clear_score();
            self.clear_ribbon();
        }
    }
    pub fn clear_score(&mut self) {
        self.score = None;
        self.flip = None;
        self.cushion = f64::NAN;
        self.vwap = f64::NAN;
        self.rvol = f64::NAN;
    }
}

/// Daily timestamps identify the exchange date (archive/Yahoo daily opens are UTC midnight).
/// Today's daily print is required only after the cash close; holidays/weekends use the prior session.
fn daily_lag(ts: i64, now: i64) -> Option<i64> {
    let wall = tr_core::ny::civil(now);
    let date = wall.date();
    let due = if crate::history::calendar::is_session(date)
        && wall.hour() * 60 + wall.minute() >= intraday::close_minute(date)
    {
        date
    } else {
        crate::history::calendar::previous_session(date)
    };
    let last = chrono::DateTime::from_timestamp(ts, 0)?.date_naive();
    (last < due).then(|| due.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp().saturating_sub(ts).max(0))
}

pub fn build_row(symbol: &str, stack: &TrendStack, base: &[Bar], now: i64) -> Row {
    let daily = stack.lanes.first().is_some_and(|l| l.tf == Timeframe::D1);
    // Check the full input, before slicing could discard tainted metadata.
    if let Err(e) = Bar::require_verified(base, "Trend Wall") {
        return Row::failure(symbol, LaneStatus::Refused(e), daily);
    }
    let mut row = Row::failure(symbol, LaneStatus::Ready, daily);
    row.provenance = Bar::provenance(base);
    row.last_bar_ts = base.last().map(|b| b.ts);
    row.clock = if daily {
        base.iter().rev().take(120).map(|b| b.ts).collect::<Vec<_>>().into_iter().rev().collect()
    } else {
        let date = tr_core::ny::civil(now).date();
        let open = tr_core::ny::to_utc(date.and_hms_opt(9, 30, 0).unwrap());
        (0..SESSION_SLOTS).map(|i| open + i as i64 * 300).collect()
    };
    row.status = stack
        .lanes
        .iter()
        .find_map(|l| {
            if !l.provenance.is_verified() && matches!(l.status, LaneStatus::Ready | LaneStatus::Stale { .. }) {
                Some(LaneStatus::Refused(format!("{} · {}", l.tf.label(), l.provenance.label())))
            } else if matches!(l.status, LaneStatus::Refused(_)) {
                Some(l.status.clone())
            } else {
                None
            }
        })
        .or_else(|| stack.lanes.iter().find(|l| matches!(l.status, LaneStatus::Stale { .. })).map(|l| l.status.clone()))
        .or_else(|| {
            stack.lanes.iter().find(|l| matches!(l.status, LaneStatus::Unavailable(_))).map(|l| l.status.clone())
        })
        .or_else(|| {
            stack.lanes.iter().find(|l| matches!(l.status, LaneStatus::Warming { .. })).map(|l| l.status.clone())
        })
        .unwrap_or(LaneStatus::Ready);
    for lane in &stack.lanes {
        let valid = lane.provenance.is_verified() && lane.status == LaneStatus::Ready;
        let cells = row
            .clock
            .iter()
            .map(|ts| {
                // Exact base timestamp lookup is deliberate: a missing base bar leaves every lane empty.
                let i = base.binary_search_by_key(ts, |b| b.ts).ok()?;
                if !valid || (!daily && *ts > now) {
                    return None;
                }
                let confidence = *lane.conf.get(i)?;
                if !confidence.is_finite() {
                    return None;
                }
                Some(Cell {
                    state: *lane.state.get(i)?,
                    confidence,
                    native_ts: (*lane.completed_ts.get(i)?)?,
                    provenance: lane.provenance,
                })
            })
            .collect();
        let forming =
            (valid && lane.tf > stack.lanes[0].tf).then_some(lane.forming).flatten().and_then(|(state, conf)| {
                // The separate cap is at the last actual base print, like the chart's lane strip.
                let ts = base.last()?.ts;
                let at = row.clock.iter().position(|t| *t == ts)?;
                (conf.is_finite() && (daily || ts <= now)).then_some((at, state, conf))
            });
        row.lanes.push(Ribbon {
            tf: lane.tf,
            status: lane.status.clone(),
            last_bar_ts: lane.last_bar_ts,
            cells,
            forming,
        });
    }
    if row.status == LaneStatus::Ready {
        row.score = stack.score.clone();
        if let Some(lane) = stack.lanes.first() {
            row.flip = lane.age.last().copied().zip(lane.state.last().copied()).filter(|(_, d)| *d != 0);
            if !daily {
                let o = Ohlcv::new(base);
                let a = atr_session(&o, 14, &session_starts(&o.ts)).last().copied().unwrap_or(f64::NAN);
                if a.is_finite()
                    && a > 0.0
                    && let Some(last) = base.last()
                {
                    row.cushion = lane.mrt.map(|m| (last.close - m) / a).unwrap_or(f64::NAN);
                    row.vwap =
                        intraday::session_vwap(Timeframe::M5, base).map(|v| (last.close - v) / a).unwrap_or(f64::NAN);
                }
                row.rvol = intraday::relative_volume(Timeframe::M5, base).unwrap_or(f64::NAN);
            }
        }
    }
    row.age(now);
    if matches!(row.status, LaneStatus::Stale { .. } | LaneStatus::Refused(_)) {
        row.clear_ribbon();
    }
    if !row.scored() {
        row.clear_score();
    }
    row
}

#[derive(Debug, Clone, PartialEq)]
pub struct Vote {
    pub tf: Timeframe,
    pub long: usize,
    pub denominator: usize,
}
pub fn breadth(rows: &[Row]) -> Vec<Vote> {
    let mut votes: Vec<Vote> = Vec::new();
    for row in rows.iter().filter(|r| r.scored()) {
        for lane in &row.lanes {
            let Some(cell) = lane.cells.iter().rev().find_map(|c| c.as_ref()) else { continue };
            if !cell.provenance.is_verified() {
                continue;
            }
            let i = votes.iter().position(|v| v.tf == lane.tf).unwrap_or_else(|| {
                votes.push(Vote { tf: lane.tf, long: 0, denominator: 0 });
                votes.len() - 1
            });
            votes[i].denominator += 1;
            votes[i].long += usize::from(cell.state > 0);
        }
    }
    votes.retain(|v| v.denominator >= MIN_VOTES);
    votes
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Sort {
    #[default]
    Stack,
    Flip,
    Cushion,
    List,
}
impl Sort {
    pub fn next(self) -> Self {
        match self {
            Self::Stack => Self::Flip,
            Self::Flip => Self::Cushion,
            Self::Cushion => Self::List,
            Self::List => Self::Stack,
        }
    }
}
pub fn sorted_indices(rows: &[Row], mode: Sort) -> Vec<usize> {
    let mut indices: Vec<_> = (0..rows.len()).collect();
    indices.sort_by(|&a, &b| {
        let (ra, rb) = (&rows[a], &rows[b]);
        rb.scored()
            .cmp(&ra.scored())
            .then_with(|| {
                if !ra.scored() {
                    return a.cmp(&b);
                }
                match mode {
                    Sort::Stack => {
                        let ratio = |r: &Row| r.score.as_ref().map(|s| s.aligned as f64 / s.scored as f64).unwrap();
                        ratio(rb).total_cmp(&ratio(ra))
                    }
                    Sort::Flip => {
                        ra.flip.map(|f| f.0).unwrap_or(usize::MAX).cmp(&rb.flip.map(|f| f.0).unwrap_or(usize::MAX))
                    }
                    Sort::Cushion => match (ra.cushion.is_finite(), rb.cushion.is_finite()) {
                        (true, true) => rb.cushion.total_cmp(&ra.cushion),
                        (a, b) => b.cmp(&a),
                    },
                    Sort::List => a.cmp(&b),
                }
            })
            .then(a.cmp(&b))
    });
    indices
}

#[cfg(test)]
mod tests {
    use super::*;
    use tr_core::indicators::trend_stack;
    fn fixture() -> (Vec<Bar>, i64) {
        let d = chrono::NaiveDate::from_ymd_opt(2026, 11, 10).unwrap();
        let open = tr_core::ny::to_utc(d.and_hms_opt(9, 30, 0).unwrap());
        let bars = (0..78)
            .map(|i| {
                let c = 100.0 + i as f64;
                Bar::new(open + i * 300, c, c + 1.0, c - 1.0, c, 100.0).with_provenance(Provenance::Real)
            })
            .collect();
        (bars, open + 77 * 300)
    }
    fn row(base: &[Bar], now: i64) -> Row {
        build_row("AAPL", &trend_stack(base, Timeframe::M5, vec![], Default::default()).unwrap(), base, now)
    }
    #[test]
    fn failed_refresh_past_freshness_emits_no_ribbon_cells() {
        let (bars, _) = fixture();
        let mut current = row(&bars[..70], bars[69].ts + 300);
        assert!(current.scored());
        current.failed_refresh("offline".into(), bars[69].ts + 300);
        assert!(current.scored());
        assert!(current.note.as_deref() == Some("offline"));
        assert!(current.lanes[0].cells.iter().any(Option::is_some));
        current.failed_refresh("offline again".into(), bars[69].ts + 1800);
        assert!(matches!(current.status, LaneStatus::Stale { .. }));
        assert!(current.score.is_none());
        assert!(current.lanes.iter().all(|l| l.cells.is_empty() && l.forming.is_none()));
    }

    #[test]
    fn row_slots_follow_the_session_clock_and_leave_missing_bars_empty() {
        let (mut bars, now) = fixture();
        let missing = [10, 23, 50];
        bars = bars.into_iter().enumerate().filter(|(i, _)| !missing.contains(i)).map(|(_, b)| b).collect();
        let r = row(&bars, now);
        assert_eq!(r.clock.len(), 78);
        for i in missing {
            assert!(r.lanes[0].cells[i].is_none());
        }
        assert!(r.lanes[0].cells[70].is_some());
        let r = row(&bars[..65], now - 10 * 300);
        assert!(r.lanes[0].cells[70..].iter().all(Option::is_none));
    }
    #[test]
    fn higher_lane_cell_appears_only_after_its_bar_completes() {
        let (bars, now) = fixture();
        let start = bars[0].ts - 80 * 3600;
        let native: Vec<_> = (0..87)
            .map(|i| Bar::new(start + i * 3600, 100.0, 101.0, 99.0, 100.0, 1.0).with_provenance(Provenance::Real))
            .collect();
        let stack = trend_stack(&bars, Timeframe::M5, vec![(Timeframe::H1, Ok(native))], Default::default()).unwrap();
        let r = build_row("AAPL", &stack, &bars, now);
        assert_eq!(r.lanes[1].cells[11].as_ref().unwrap().native_ts, bars[0].ts - 3600);
        assert_eq!(r.lanes[1].cells[12].as_ref().unwrap().native_ts, bars[0].ts);
    }
    #[test]
    fn refused_and_stale_rows_have_no_score_and_no_breadth_vote() {
        let (bars, now) = fixture();
        let mut stack = trend_stack(&bars, Timeframe::M5, vec![], Default::default()).unwrap();
        for status in [LaneStatus::Refused("unverified".into()), LaneStatus::Stale { lag_secs: 840 }] {
            stack.lanes[0].status = status;
            let r = build_row("AAPL", &stack, &bars, now);
            assert!(r.score.is_none());
            assert!(breadth(&vec![r; 6]).is_empty());
        }
        let mut tainted = bars.clone();
        tainted[0] = tainted[0].with_provenance(Provenance::Missing);
        assert!(matches!(build_row("AAPL", &stack, &tainted, now).status, LaneStatus::Refused(_)));
    }
    #[test]
    fn breadth_hidden_below_min_votes_and_reports_denominator() {
        let (bars, now) = fixture();
        let mut r = row(&bars, now);
        // Control the regime observations; price fixtures need not trigger a particular ADTREND regime.
        for cell in r.lanes[0].cells.iter_mut().flatten() {
            cell.state = 1;
        }
        assert!(r.scored());
        assert!(breadth(&vec![r.clone(); 4]).is_empty());
        let votes = breadth(&vec![r; 7]);
        assert_eq!(votes[0].denominator, 7);
        assert_eq!(votes[0].long, 7);
    }
    #[test]
    fn cushion_vwap_rvol_are_nan_not_zero_when_inputs_missing() {
        let (mut bars, now) = fixture();
        for b in &mut bars {
            b.volume = f64::NAN;
        }
        let mut stack = trend_stack(&bars, Timeframe::M5, vec![], Default::default()).unwrap();
        stack.lanes[0].mrt = None;
        let r = build_row("AAPL", &stack, &bars, now);
        assert!(r.cushion.is_nan());
        assert!(r.vwap.is_nan());
        assert!(r.rvol.is_nan());
    }
    #[test]
    fn freshness_tick_removes_votes_without_extending_observed_cells() {
        let (bars, now) = fixture();
        let mut r = row(&bars, now);
        assert!(r.scored());
        assert!(r.lanes[0].cells.iter().any(Option::is_some));
        r.lanes.push(Ribbon {
            tf: Timeframe::H1,
            status: LaneStatus::Ready,
            last_bar_ts: Some(bars[0].ts),
            cells: vec![None; 78],
            forming: None,
        });
        assert!(r.age(now));
        assert!(!r.scored());
        assert!(r.score.is_none());
        assert!(breadth(&vec![r.clone(); 5]).is_empty());
        assert!(r.lanes[0].cells.is_empty());
        assert!(!r.age(now + 1)); // Updating the age label does not rebuild unchanged geometry.
    }
    #[test]
    fn old_daily_observations_drop_ribbons_and_never_score() {
        let (mut bars, _) = fixture();
        let end = chrono::NaiveDate::from_ymd_opt(2026, 11, 10).unwrap();
        for (i, bar) in bars.iter_mut().enumerate() {
            bar.ts = (end - chrono::Duration::days(77 - i as i64)).and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
        }
        let stack = trend_stack(&bars, Timeframe::D1, vec![], Default::default()).unwrap();
        let now = tr_core::ny::to_utc(end.and_hms_opt(17, 0, 0).unwrap());
        let current = build_row("AAPL", &stack, &bars, now);
        assert!(current.scored());
        let stale = build_row("AAPL", &stack, &bars, now + 3 * 86400);
        assert!(matches!(stale.status, LaneStatus::Stale { .. }));
        assert!(stale.score.is_none());
        assert!(breadth(&vec![stale.clone(); 6]).is_empty());
        assert!(stale.lanes[0].cells.is_empty());
        // Closed weekends do not demand a nonexistent Saturday or Sunday daily bar.
        let friday = chrono::NaiveDate::from_ymd_opt(2026, 11, 13).unwrap();
        let daily_ts = friday.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
        let sunday = tr_core::ny::to_utc((friday + chrono::Duration::days(2)).and_hms_opt(12, 0, 0).unwrap());
        assert_eq!(daily_lag(daily_ts, sunday), None);
    }
    #[test]
    fn sort_puts_unscored_rows_last_in_every_mode() {
        let (bars, now) = fixture();
        let good = row(&bars, now);
        let bad = Row::failure("BAD", LaneStatus::Unavailable("offline".into()), false);
        let mut stale = good.clone();
        stale.status = LaneStatus::Stale { lag_secs: 840 };
        let mut refused = good.clone();
        refused.status = LaneStatus::Refused("UNVERIFIED".into());
        let warming = Row::failure("NEW", LaneStatus::Warming { have: 10, need: 60 }, false);
        for mode in [Sort::Stack, Sort::Flip, Sort::Cushion, Sort::List] {
            assert_eq!(
                sorted_indices(&[bad.clone(), stale.clone(), good.clone(), refused.clone(), warming.clone()], mode),
                vec![2, 0, 1, 3, 4]
            );
        }
    }
}
