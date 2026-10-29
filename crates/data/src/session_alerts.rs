//! Alerts on the cash-session reads already drawn by the research panes.
//!
//! Each rule watches one name on verified 5-minute bars (the caller chooses the
//! bar size; the worker uses 5-minute). The first read only seeds state. A later
//! read fires when the condition turns on. Sample bars are refused by the
//! underlying reads. A band that still has fewer than five prior sessions is an
//! error, not a silent miss.

use chrono::NaiveDate;
use tr_core::{Bar, Provenance, Timeframe};

use crate::gap_fill::{self, GapDirection, GapQuery};
use crate::intraday::{Freshness, MarketClock};
use crate::relative_path::{self, RelativeQuery};
use crate::security;
use crate::session_extreme::{self, ExtremeQuery};
use crate::session_path::{self, PathQuery};
use crate::volume_pace::{self, PaceQuery};

/// Which session read the rule watches. The window is that pane's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum SessionSignal {
    /// Today's percent from the 09:30 open leaves the prior p25–p75 band.
    Path,
    /// Cumulative regular-session volume is above the prior p75.
    Pace,
    /// A gap up or gap down has traded through the prior cash close.
    Gap,
    /// The running high or low printed before the usual minute, and that minute has passed.
    Extreme,
    /// This name's percent from its open, minus SPY's, leaves the prior band.
    Relative,
}

impl SessionSignal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Pace => "pace",
            Self::Gap => "gap",
            Self::Extreme => "extreme",
            Self::Relative => "relative",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Path => "outside the usual session path",
            Self::Pace => "above the usual volume pace",
            Self::Gap => "gap fill",
            Self::Extreme => "unusually early session high or low",
            Self::Relative => "outside the usual path versus SPY",
        }
    }

    /// Command token after `ALERT`. Pane aliases are accepted. `HILO` is not.
    pub fn parse(token: &str) -> Option<Self> {
        match token.trim().to_ascii_uppercase().as_str() {
            "PATH" | "SPATH" => Some(Self::Path),
            "PACE" | "VPACE" | "VOLPACE" | "CUMVOL" => Some(Self::Pace),
            "GAP" | "GFILL" | "GAPFILL" => Some(Self::Gap),
            "XTRM" | "EXTREME" => Some(Self::Extreme),
            "RPATH" | "RELPATH" => Some(Self::Relative),
            _ => None,
        }
    }
}

/// One evaluated session condition. `active` is the current on/off state.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionTick {
    pub symbol: String,
    pub signal: SessionSignal,
    pub active: bool,
    pub detail: String,
    pub bar_ts: i64,
    pub session_date: NaiveDate,
    pub provenance: Provenance,
    pub fresh: bool,
}

/// How many 5-minute bars a session alert asks the provider for.
pub fn fetch_limit() -> usize {
    crate::cash_session::research_fetch_limit(Timeframe::M5)
}

/// Read one signal from verified bars. `spy_bars` is required for [`SessionSignal::Relative`].
pub fn read(
    signal: SessionSignal,
    symbol: &str,
    tf: Timeframe,
    bars: &[Bar],
    spy_bars: Option<&[Bar]>,
    now: i64,
) -> Result<SessionTick, String> {
    let sec = security(symbol);
    let (active, detail, bar_ts, session_date, provenance) = match signal {
        SessionSignal::Path => path_state(&sec.key(), tf, bars)?,
        SessionSignal::Pace => pace_state(&sec.key(), tf, bars)?,
        SessionSignal::Gap => gap_state(&sec.key(), tf, bars)?,
        SessionSignal::Extreme => extreme_state(&sec.key(), tf, bars)?,
        SessionSignal::Relative => {
            let spy = spy_bars.ok_or_else(|| "Relative path needs SPY bars.".to_string())?;
            relative_state(&sec.key(), tf, bars, spy)?
        }
    };
    let fresh = matches!(
        crate::intraday::freshness(MarketClock::for_class(sec.class), tf, bar_ts, now),
        Some(Freshness::Current)
    );
    Ok(SessionTick { symbol: sec.key(), signal, active, detail, bar_ts, session_date, provenance, fresh })
}

fn path_state(symbol: &str, tf: Timeframe, bars: &[Bar]) -> Result<(bool, String, i64, NaiveDate, Provenance), String> {
    let path = session_path::build_path(
        symbol,
        bars,
        &PathQuery { window: session_path::DEFAULT_WINDOW, timeframe: tf },
        "session alert",
    )?;
    let point = path
        .points
        .iter()
        .rev()
        .find(|point| point.today.is_some() && point.p25.is_some() && point.p75.is_some())
        .ok_or_else(|| "The path band is not ready.".to_string())?;
    let today = point.today.unwrap();
    let p25 = point.p25.unwrap();
    let p75 = point.p75.unwrap();
    let active = today < p25 || today > p75;
    let detail = format!("left the usual session path at {today:+.2}% (band {p25:+.2}% to {p75:+.2}%)");
    Ok((active, detail, point.ts, path.date, path.provenance))
}

fn pace_state(symbol: &str, tf: Timeframe, bars: &[Bar]) -> Result<(bool, String, i64, NaiveDate, Provenance), String> {
    let pace = volume_pace::build_pace(
        symbol,
        bars,
        &PaceQuery { window: volume_pace::DEFAULT_WINDOW, timeframe: tf },
        "session alert",
    )?;
    let point = pace
        .points
        .iter()
        .rev()
        .find(|point| point.today.is_some() && point.p75.is_some())
        .ok_or_else(|| "The pace band is not ready.".to_string())?;
    let today = point.today.unwrap();
    let p75 = point.p75.unwrap();
    let active = today > p75;
    let detail = format!("cumulative volume is above the usual pace ({today:.0} vs {p75:.0})");
    Ok((active, detail, point.ts, pace.date, pace.provenance))
}

fn gap_state(symbol: &str, tf: Timeframe, bars: &[Bar]) -> Result<(bool, String, i64, NaiveDate, Provenance), String> {
    let gap = gap_fill::build_gap(
        symbol,
        bars,
        &GapQuery { window: gap_fill::DEFAULT_WINDOW, timeframe: tf },
        "session alert",
    )?;
    let bar_ts = gap.fill_ts.or(gap.today_last_ts).ok_or_else(|| "no print today".to_string())?;
    let (active, detail) = match gap.direction {
        GapDirection::Flat => (false, "the open equals the prior close".to_string()),
        GapDirection::Up | GapDirection::Down => {
            let verb = if gap.filled { "filled" } else { "is still open" };
            (gap.filled, format!("{} of {:+.2}% {verb}", gap.direction.label(), gap.gap_pct))
        }
    };
    Ok((active, detail, bar_ts, gap.date, gap.provenance))
}

fn extreme_state(
    symbol: &str,
    tf: Timeframe,
    bars: &[Bar],
) -> Result<(bool, String, i64, NaiveDate, Provenance), String> {
    let extreme = session_extreme::build_extreme(
        symbol,
        bars,
        &ExtremeQuery { window: session_extreme::DEFAULT_WINDOW, timeframe: tf },
        "session alert",
    )?;
    let bar_ts = extreme.today_last_ts.ok_or_else(|| "no print today".to_string())?;
    let minute = crate::cash_session::minute_offset(bar_ts).ok_or_else(|| "outside the cash session".to_string())?;
    let (Some(median_high), Some(median_low), Some(high_min), Some(low_min)) =
        (extreme.median_high_minute, extreme.median_low_minute, extreme.today_high_minute, extreme.today_low_minute)
    else {
        return Err("The session extreme is not ready.".into());
    };
    let early_high = minute >= median_high && high_min < median_high;
    let early_low = minute >= median_low && low_min < median_low;
    let detail = if early_high {
        format!(
            "session high printed at {}, earlier than the usual {}",
            session_extreme::minute_label(high_min),
            session_extreme::minute_label(median_high)
        )
    } else if early_low {
        format!(
            "session low printed at {}, earlier than the usual {}",
            session_extreme::minute_label(low_min),
            session_extreme::minute_label(median_low)
        )
    } else {
        "the high and the low are still inside the usual minutes".into()
    };
    Ok((early_high || early_low, detail, bar_ts, extreme.date, extreme.provenance))
}

fn relative_state(
    symbol: &str,
    tf: Timeframe,
    bars: &[Bar],
    spy_bars: &[Bar],
) -> Result<(bool, String, i64, NaiveDate, Provenance), String> {
    let path = relative_path::build_relative(
        symbol,
        bars,
        spy_bars,
        &RelativeQuery { window: relative_path::DEFAULT_WINDOW, timeframe: tf },
        "session alert",
    )?;
    let point = path
        .points
        .iter()
        .rev()
        .find(|point| point.today.is_some() && point.p25.is_some() && point.p75.is_some())
        .ok_or_else(|| "The path versus SPY is not ready.".to_string())?;
    let today = point.today.unwrap();
    let p25 = point.p25.unwrap();
    let p75 = point.p75.unwrap();
    let active = today < p25 || today > p75;
    let detail = format!("left the usual path versus SPY at {today:+.2} pp (band {p25:+.2} to {p75:+.2} pp)");
    Ok((active, detail, point.ts, path.date, path.provenance.combine(Bar::provenance(spy_bars))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::calendar::is_session;
    use crate::intraday::regular_opens;
    use chrono::Duration;

    fn sessions(n: usize) -> Vec<NaiveDate> {
        let mut date = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let mut out = Vec::new();
        while out.len() < n {
            if is_session(date) {
                out.push(date);
            }
            date += Duration::days(1);
        }
        out
    }

    fn px(ts: i64, open: f64, close: f64, volume: f64) -> Bar {
        let high = open.max(close) + 0.1;
        let low = (open.min(close) - 0.1).max(0.01);
        Bar::new(ts, open, high, low, close, volume).with_provenance(Provenance::Real)
    }

    fn day_move(date: NaiveDate, open: f64, close: f64, volume: f64) -> Vec<Bar> {
        regular_opens(date, Timeframe::M30).into_iter().map(|ts| px(ts, open, close, volume)).collect()
    }

    fn rising(date: NaiveDate) -> Vec<Bar> {
        regular_opens(date, Timeframe::M30)
            .into_iter()
            .enumerate()
            .map(|(i, ts)| {
                let px = 100.0 + i as f64;
                Bar::new(ts, px, px + 0.5, px - 0.2, px + 0.4, 1_000.0).with_provenance(Provenance::Real)
            })
            .collect()
    }

    fn falling(date: NaiveDate, bars: usize) -> Vec<Bar> {
        regular_opens(date, Timeframe::M30)
            .into_iter()
            .take(bars)
            .enumerate()
            .map(|(i, ts)| {
                let px = 200.0 - i as f64;
                Bar::new(ts, px, px + 0.2, px - 0.5, px - 0.4, 1_000.0).with_provenance(Provenance::Real)
            })
            .collect()
    }

    fn now_of(bars: &[Bar]) -> i64 {
        bars.last().unwrap().ts
    }

    #[test]
    fn path_pace_and_gap_turn_on_only_for_the_named_condition() {
        let dates = sessions(6);
        let today = *dates.last().unwrap();
        let mut calm = Vec::new();
        let mut hot = Vec::new();
        let mut loud = Vec::new();
        let mut quiet = Vec::new();
        for (i, date) in dates.iter().enumerate() {
            let last = i + 1 == dates.len();
            calm.extend(day_move(*date, 100.0, if last { 110.0 } else { 102.0 }, 1_000.0));
            hot.extend(day_move(*date, 100.0, 102.0, 1_000.0));
            loud.extend(day_move(*date, 100.0, 100.0, if last { 5_000.0 } else { 1_000.0 }));
            quiet.extend(day_move(*date, 100.0, 100.0, 1_000.0));
        }
        let outside = read(SessionSignal::Path, "AAPL", Timeframe::M30, &calm, None, now_of(&calm)).unwrap();
        assert!(outside.active);
        assert!(outside.fresh);
        assert!(outside.detail.contains("usual session path"));
        assert!(outside.detail.contains("+10.00%"));
        let inside = read(SessionSignal::Path, "AAPL", Timeframe::M30, &hot, None, now_of(&hot)).unwrap();
        assert!(!inside.active);

        let fast = read(SessionSignal::Pace, "AAPL", Timeframe::M30, &loud, None, now_of(&loud)).unwrap();
        assert!(fast.active, "{}", fast.detail);
        assert!(fast.detail.contains("usual pace"));
        let usual = read(SessionSignal::Pace, "AAPL", Timeframe::M30, &quiet, None, now_of(&quiet)).unwrap();
        assert!(!usual.active);

        let prior = dates[dates.len() - 2];
        let mut filled_bars = day_move(prior, 100.0, 100.0, 1_000.0);
        let open = regular_opens(today, Timeframe::M30)[0];
        filled_bars.push(Bar::new(open, 102.0, 103.0, 99.0, 101.0, 1_000.0).with_provenance(Provenance::Real));
        let filled = read(SessionSignal::Gap, "AAPL", Timeframe::M30, &filled_bars, None, open).unwrap();
        assert!(filled.active, "{}", filled.detail);
        assert!(filled.detail.contains("filled"));

        let mut open_gap = day_move(prior, 100.0, 100.0, 1_000.0);
        open_gap.push(Bar::new(open, 102.0, 103.0, 101.5, 102.5, 1_000.0).with_provenance(Provenance::Real));
        let still = read(SessionSignal::Gap, "AAPL", Timeframe::M30, &open_gap, None, open).unwrap();
        assert!(!still.active);
        assert!(still.detail.contains("still open"));

        let mut flat = day_move(prior, 100.0, 100.0, 1_000.0);
        flat.extend(day_move(today, 100.0, 101.0, 1_000.0));
        let no_gap = read(SessionSignal::Gap, "AAPL", Timeframe::M30, &flat, None, now_of(&flat)).unwrap();
        assert!(!no_gap.active);
        assert!(no_gap.detail.contains("prior close"));
    }

    #[test]
    fn a_short_history_is_not_ready_and_sample_bars_are_refused() {
        let dates = sessions(4);
        let bars: Vec<Bar> = dates.iter().flat_map(|date| day_move(*date, 100.0, 110.0, 1_000.0)).collect();
        let err = read(SessionSignal::Path, "AAPL", Timeframe::M30, &bars, None, now_of(&bars)).unwrap_err();
        assert!(err.contains("not ready"), "{err}");
        let err = read(SessionSignal::Pace, "AAPL", Timeframe::M30, &bars, None, now_of(&bars)).unwrap_err();
        assert!(err.contains("not ready"), "{err}");

        let mut sample = day_move(dates[0], 100.0, 101.0, 1_000.0);
        sample[0].provenance = Provenance::Sample;
        let err = read(SessionSignal::Path, "AAPL", Timeframe::M30, &sample, None, now_of(&sample)).unwrap_err();
        assert!(err.contains("SAMPLE"), "{err}");
        let err = read(SessionSignal::Pace, "BTCUSD", Timeframe::M30, &bars, None, now_of(&bars)).unwrap_err();
        assert!(err.contains("cash session"), "{err}");
    }

    #[test]
    fn an_early_extreme_waits_until_the_usual_minute_has_passed() {
        let dates = sessions(6);
        let today = *dates.last().unwrap();
        let mut bars = Vec::new();
        for date in dates.iter().take(5) {
            bars.extend(rising(*date));
        }
        let mut partial = bars.clone();
        partial.extend(falling(today, 2));
        let waiting = read(SessionSignal::Extreme, "AAPL", Timeframe::M30, &partial, None, now_of(&partial)).unwrap();
        assert!(!waiting.active, "{}", waiting.detail);

        bars.extend(falling(today, usize::MAX));
        let early = read(SessionSignal::Extreme, "AAPL", Timeframe::M30, &bars, None, now_of(&bars)).unwrap();
        assert!(early.active, "{}", early.detail);
        assert!(early.detail.contains("earlier than the usual"));
        assert!(early.fresh);

        let short: Vec<Bar> = sessions(4).iter().flat_map(|date| rising(*date)).collect();
        let err = read(SessionSignal::Extreme, "AAPL", Timeframe::M30, &short, None, now_of(&short)).unwrap_err();
        assert!(err.contains("not ready"), "{err}");
    }

    #[test]
    fn relative_path_uses_spy_and_refuses_the_benchmark_itself() {
        let dates = sessions(6);
        let mut name = Vec::new();
        let mut spy = Vec::new();
        for (i, date) in dates.iter().enumerate() {
            let last = i + 1 == dates.len();
            name.extend(day_move(*date, 100.0, if last { 110.0 } else { 102.0 }, 1_000.0));
            spy.extend(day_move(*date, 100.0, 102.0, 1_000.0));
        }
        let outside = read(SessionSignal::Relative, "AAPL", Timeframe::M30, &name, Some(&spy), now_of(&name)).unwrap();
        assert!(outside.active, "{}", outside.detail);
        assert!(outside.detail.contains("versus SPY"));
        let err = read(SessionSignal::Relative, "SPY", Timeframe::M30, &spy, Some(&spy), now_of(&spy)).unwrap_err();
        assert!(err.contains("benchmark"), "{err}");
        let err = read(SessionSignal::Relative, "AAPL", Timeframe::M30, &name, None, now_of(&name)).unwrap_err();
        assert!(err.contains("SPY"), "{err}");
        assert_eq!(SessionSignal::parse("spath"), Some(SessionSignal::Path));
        assert_eq!(SessionSignal::parse("CUMVOL"), Some(SessionSignal::Pace));
        assert_eq!(SessionSignal::parse("HILO"), None);
        assert_eq!(fetch_limit(), 5_000);
    }
}
