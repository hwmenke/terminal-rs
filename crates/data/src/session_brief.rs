//! One read of the five cash-session signals for a single name.
//!
//! This is not a chart. Each line is the same on/off state the session alerts use,
//! on 5-minute bars in the worker and on each pane's default window. A line that is
//! not ready stays on the brief as that error. Sample bars are refused before any
//! line is scored. SPY still gets the four name lines; the versus-SPY line says SPY
//! is the benchmark.

use chrono::NaiveDate;
use tr_core::{Bar, Provenance, Timeframe};

use crate::cash_session;
use crate::intraday::{self, Freshness, MarketClock};
use crate::security;
use crate::session_alerts::{self, SessionSignal};

const SIGNALS: [SessionSignal; 5] =
    [SessionSignal::Path, SessionSignal::Pace, SessionSignal::Gap, SessionSignal::Extreme, SessionSignal::Relative];

/// One signal on the brief. `active` is `None` when that line could not be scored.
#[derive(Debug, Clone, PartialEq)]
pub struct BriefLine {
    pub signal: SessionSignal,
    pub active: Option<bool>,
    pub detail: String,
    pub fresh: bool,
}

impl BriefLine {
    pub fn mark(&self) -> &'static str {
        match self.active {
            Some(true) => "On",
            Some(false) => "Off",
            None => "—",
        }
    }
}

/// Five lines for one cash name. Provenance is the name's bars, not SPY's.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionBrief {
    pub symbol: String,
    pub ticker: String,
    pub timeframe: Timeframe,
    pub date: Option<NaiveDate>,
    pub provenance: Provenance,
    /// The latest scored bar is still the current bar. A fill at the open does not make the brief stale.
    pub fresh: bool,
    pub lines: Vec<BriefLine>,
}

/// Score path, pace, gap, extreme, and versus SPY. `spy_bars` may be missing:
/// only the versus-SPY line fails then.
pub fn build(
    symbol: &str,
    tf: Timeframe,
    bars: &[Bar],
    spy_bars: Option<&[Bar]>,
    now: i64,
) -> Result<SessionBrief, String> {
    let sec = security(symbol);
    if symbol.trim().is_empty() || !tr_core::security::looks_like_ticker(&sec.ticker) {
        return Err("BRIEF needs a ticker (AAPL BRIEF).".into());
    }
    if !cash_session::is_cash(&sec) {
        return Err(format!("{} has no US cash session.", sec.ticker));
    }
    cash_session::require_bar_size(tf, "this brief")?;
    if bars.is_empty() {
        return Err(
            "Session brief refused: no regular-session bars were returned. Sample prices are never used.".into()
        );
    }
    Bar::require_verified(bars, "Session brief")?;
    let mut lines = Vec::with_capacity(SIGNALS.len());
    let mut date = None;
    let mut latest = None;
    for signal in SIGNALS {
        match session_alerts::read(signal, &sec.key(), tf, bars, spy_bars, now) {
            Ok(tick) => {
                if date.is_none() {
                    date = Some(tick.session_date);
                }
                latest = Some(latest.map_or(tick.bar_ts, |ts: i64| ts.max(tick.bar_ts)));
                lines.push(BriefLine { signal, active: Some(tick.active), detail: tick.detail, fresh: tick.fresh });
            }
            Err(detail) => lines.push(BriefLine { signal, active: None, detail, fresh: false }),
        }
    }
    let fresh = latest.is_some_and(|ts| {
        matches!(intraday::freshness(MarketClock::for_class(sec.class), tf, ts, now), Some(Freshness::Current))
    });
    Ok(SessionBrief {
        symbol: sec.key(),
        ticker: sec.ticker,
        timeframe: tf,
        date,
        provenance: Bar::provenance(bars),
        fresh,
        lines,
    })
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

    fn series(days: usize, last_close: f64, last_volume: f64) -> Vec<Bar> {
        let dates = sessions(days);
        let mut bars = Vec::new();
        for (i, date) in dates.iter().enumerate() {
            let last = i + 1 == dates.len();
            bars.extend(day_move(
                *date,
                100.0,
                if last { last_close } else { 102.0 },
                if last { last_volume } else { 1_000.0 },
            ));
        }
        bars
    }

    fn line<'a>(brief: &'a SessionBrief, signal: SessionSignal) -> &'a BriefLine {
        brief.lines.iter().find(|line| line.signal == signal).unwrap()
    }

    #[test]
    fn five_lines_follow_the_alert_reads_and_a_missing_spy_only_blanks_one() {
        let name = series(6, 110.0, 5_000.0);
        let spy = series(6, 102.0, 1_000.0);
        let now = name.last().unwrap().ts;
        let brief = build("AAPL", Timeframe::M30, &name, Some(&spy), now).unwrap();
        assert_eq!(brief.lines.len(), 5);
        assert_eq!(brief.ticker, "AAPL");
        assert_eq!(brief.provenance, Provenance::Real);
        assert!(brief.fresh);
        assert_eq!(brief.date, Some(sessions(6).pop().unwrap()));
        assert!(line(&brief, SessionSignal::Path).fresh);

        let path = line(&brief, SessionSignal::Path);
        assert_eq!(path.active, Some(true));
        assert!(path.detail.contains("usual session path"), "{}", path.detail);
        assert!(path.detail.contains("+10.00%"), "{}", path.detail);

        let pace = line(&brief, SessionSignal::Pace);
        assert_eq!(pace.active, Some(true), "{}", pace.detail);
        assert!(pace.detail.contains("usual pace"), "{}", pace.detail);

        let gap = line(&brief, SessionSignal::Gap);
        assert_eq!(gap.active, Some(true), "{}", gap.detail);
        assert!(gap.detail.contains("filled"), "{}", gap.detail);

        let extreme = line(&brief, SessionSignal::Extreme);
        assert_eq!(extreme.active, Some(false), "{}", extreme.detail);
        assert!(extreme.detail.contains("usual minutes"), "{}", extreme.detail);

        let relative = line(&brief, SessionSignal::Relative);
        assert_eq!(relative.active, Some(true), "{}", relative.detail);
        assert!(relative.detail.contains("versus SPY"), "{}", relative.detail);

        let quiet = series(6, 102.0, 1_000.0);
        let inside = build("AAPL", Timeframe::M30, &quiet, Some(&spy), quiet.last().unwrap().ts).unwrap();
        assert_eq!(line(&inside, SessionSignal::Path).active, Some(false));
        assert_eq!(line(&inside, SessionSignal::Pace).active, Some(false));

        let partial = build("AAPL", Timeframe::M30, &name, None, now).unwrap();
        assert_eq!(line(&partial, SessionSignal::Path).active, Some(true));
        let missing = line(&partial, SessionSignal::Relative);
        assert_eq!(missing.active, None);
        assert!(missing.detail.contains("SPY"), "{}", missing.detail);
    }

    #[test]
    fn a_short_line_stays_on_the_brief_and_sample_or_non_cash_is_refused() {
        let short = series(4, 110.0, 5_000.0);
        let brief = build("MSFT", Timeframe::M30, &short, Some(&short), short.last().unwrap().ts).unwrap();
        assert_eq!(brief.lines.len(), 5);
        let path = line(&brief, SessionSignal::Path);
        assert_eq!(path.active, None);
        assert!(path.detail.contains("not ready"), "{}", path.detail);
        let pace = line(&brief, SessionSignal::Pace);
        assert_eq!(pace.active, None);
        assert!(pace.detail.contains("not ready"), "{}", pace.detail);
        assert!(line(&brief, SessionSignal::Gap).active.is_some(), "a gap only needs the prior close");

        let mut sample = short.clone();
        sample[0].provenance = Provenance::Sample;
        let err = build("AAPL", Timeframe::M30, &sample, Some(&short), short.last().unwrap().ts).unwrap_err();
        assert!(err.contains("SAMPLE"), "{err}");

        let err = build("BTCUSD", Timeframe::M30, &short, None, short.last().unwrap().ts).unwrap_err();
        assert!(err.contains("cash session"), "{err}");
        let err = build("AAPL", Timeframe::M30, &[], None, 0).unwrap_err();
        assert!(err.contains("no regular-session bars"), "{err}");
        let err = build("  ", Timeframe::M30, &short, None, 0).unwrap_err();
        assert!(err.contains("BRIEF needs a ticker"), "{err}");

        let spy = series(6, 102.0, 1_000.0);
        let bench = build("SPY", Timeframe::M30, &spy, Some(&spy), spy.last().unwrap().ts).unwrap();
        assert_eq!(line(&bench, SessionSignal::Path).active, Some(false));
        let relative = line(&bench, SessionSignal::Relative);
        assert_eq!(relative.active, None);
        assert!(relative.detail.contains("benchmark"), "{}", relative.detail);
    }
}
