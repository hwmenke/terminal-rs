//! Price levels for the chart: prior cash close, 09:30 open, opening-range high and low,
//! and session VWAP. These are prices, not a path against prior sessions.
//!
//! The open is the single 09:30 print. The prior close is the single last regular bar of
//! the previous session. A later bar does not stand in for either. The opening range is
//! the existing 30-minute read: a missing bucket or a bar size that cannot tile 30 minutes
//! leaves those two levels off. Sample bars are refused.

use chrono::NaiveDate;
use tr_core::{Bar, Provenance, Timeframe};

use crate::cash_session::{self, OpenPrint};
use crate::history::calendar::previous_session;
use crate::intraday::{self, regular_opens};
use crate::security;

pub const PRIOR_CLOSE: &str = "Prior close";
pub const OPEN: &str = "Open";
pub const OR_HIGH: &str = "OR high";
pub const OR_LOW: &str = "OR low";
pub const VWAP: &str = "VWAP";

pub const LEVEL_NAMES: [&str; 5] = [PRIOR_CLOSE, OPEN, OR_HIGH, OR_LOW, VWAP];

/// Enough 5-minute bars for today and the previous cash close. Not a statistical window.
pub fn fetch_limit() -> usize {
    400
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Level {
    pub name: &'static str,
    pub price: f64,
}

/// Levels that could be read. Missing ones are named in `notes` and left out of `levels`.
#[derive(Debug, Clone, PartialEq)]
pub struct CashLevels {
    pub symbol: String,
    pub ticker: String,
    pub date: NaiveDate,
    pub timeframe: Timeframe,
    pub provenance: Provenance,
    pub levels: Vec<Level>,
    pub notes: Vec<String>,
}

impl CashLevels {
    pub fn price(&self, name: &str) -> Option<f64> {
        self.levels.iter().find(|level| level.name == name).map(|level| level.price)
    }
}

pub fn build(symbol: &str, tf: Timeframe, bars: &[Bar]) -> Result<CashLevels, String> {
    let sec = security(symbol);
    if symbol.trim().is_empty() || !tr_core::security::looks_like_ticker(&sec.ticker) {
        return Err("MARKS needs a ticker (AAPL MARKS).".into());
    }
    if !cash_session::is_cash(&sec) {
        return Err(format!("{} has no US cash session.", sec.ticker));
    }
    cash_session::require_bar_size(tf, "these levels")?;
    if bars.is_empty() {
        return Err(format!("{} has no regular-session bars. A premarket print is not the open.", sec.ticker));
    }
    Bar::require_verified(bars, "Cash levels")?;
    let by_day = cash_session::sessions(tf, bars);
    let Some((&date, today)) = by_day.iter().next_back() else {
        return Err(format!("{} has no regular-session bars. A premarket print is not the open.", sec.ticker));
    };
    let mut levels = Vec::new();
    let mut notes = Vec::new();
    let prior = previous_session(date);
    match cash_session::cash_close(tf, prior, by_day.get(&prior)) {
        Some(price) => levels.push(Level { name: PRIOR_CLOSE, price }),
        None => notes.push(format!("{} has no cash close for {prior}. An older close did not stand in.", sec.ticker)),
    }
    let open_ts = regular_opens(date, tf).into_iter().next();
    match open_ts.map(|ts| cash_session::open_print(today, ts)) {
        Some(OpenPrint::One(bar)) if cash_session::usable_range(bar) => {
            levels.push(Level { name: OPEN, price: bar.open });
        }
        _ => notes.push(format!("{} has no single 09:30 print on {date}. A later bar did not stand in.", sec.ticker)),
    }
    match intraday::opening_range(tf, bars) {
        Some(range) => {
            levels.push(Level { name: OR_HIGH, price: range.high });
            levels.push(Level { name: OR_LOW, price: range.low });
        }
        None if !tiles_opening_range(tf) => {
            notes.push(format!("{} does not tile the 30-minute opening range.", tf.label()));
        }
        None => notes.push("The opening range is not complete, so those levels are left off.".into()),
    }
    match intraday::session_vwap(tf, bars) {
        Some(price) => levels.push(Level { name: VWAP, price }),
        None => notes.push("Session VWAP needs positive finite volume on the latest regular session.".into()),
    }
    Ok(CashLevels {
        symbol: sec.key(),
        ticker: sec.ticker,
        date,
        timeframe: tf,
        provenance: Bar::provenance(bars),
        levels,
        notes,
    })
}

fn tiles_opening_range(tf: Timeframe) -> bool {
    matches!(tf, Timeframe::M1 | Timeframe::M5 | Timeframe::M15 | Timeframe::M30)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intraday::regular_opens;

    fn px(ts: i64, open: f64, high: f64, low: f64, close: f64, volume: f64) -> Bar {
        Bar::new(ts, open, high, low, close, volume).with_provenance(Provenance::Real)
    }

    fn near(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
    }

    #[test]
    fn levels_are_the_prior_close_open_range_and_vwap() {
        let today = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let prior = previous_session(today);
        let mut bars = vec![px(*regular_opens(prior, Timeframe::M30).last().unwrap(), 80.0, 91.0, 79.0, 90.0, 10.0)];
        let open_ts = regular_opens(today, Timeframe::M30)[0];
        bars.push(px(open_ts, 100.0, 110.0, 90.0, 105.0, 10.0));
        let levels = build("AAPL", Timeframe::M30, &bars).unwrap();
        near(levels.price(PRIOR_CLOSE).unwrap(), 90.0);
        near(levels.price(OPEN).unwrap(), 100.0);
        near(levels.price(OR_HIGH).unwrap(), 110.0);
        near(levels.price(OR_LOW).unwrap(), 90.0);
        near(levels.price(VWAP).unwrap(), (110.0 + 90.0 + 105.0) / 3.0);
        assert!(levels.notes.is_empty(), "{:?}", levels.notes);
        assert_eq!(levels.provenance, Provenance::Real);
    }

    #[test]
    fn a_later_bar_is_not_the_open_and_an_early_close_keeps_its_own_close() {
        let today = NaiveDate::from_ymd_opt(2026, 1, 6).unwrap();
        let prior = previous_session(today);
        let ten = regular_opens(today, Timeframe::M30)[1];
        let bars = vec![
            px(*regular_opens(prior, Timeframe::M30).last().unwrap(), 100.0, 100.0, 100.0, 100.0, 1.0),
            px(ten, 130.0, 131.0, 129.0, 130.0, 1.0),
        ];
        let levels = build("AAPL", Timeframe::M30, &bars).unwrap();
        assert!(levels.price(OPEN).is_none());
        assert!(levels.notes.iter().any(|note| note.contains("later bar did not stand in")), "{:?}", levels.notes);
        near(levels.price(PRIOR_CLOSE).unwrap(), 100.0);

        let session = NaiveDate::from_ymd_opt(2025, 7, 7).unwrap();
        let early = previous_session(session);
        assert_eq!(early, NaiveDate::from_ymd_opt(2025, 7, 3).unwrap());
        let close_ts = *regular_opens(early, Timeframe::M30).last().unwrap();
        let mut early_bars = vec![
            px(close_ts, 100.0, 101.0, 99.0, 100.0, 1.0),
            px(close_ts + 60 * 60, 50.0, 51.0, 49.0, 50.0, 1.0),
            px(regular_opens(session, Timeframe::M30)[0], 110.0, 112.0, 109.0, 111.0, 2.0),
        ];
        let levels = build("SPY", Timeframe::M30, &early_bars).unwrap();
        near(levels.price(PRIOR_CLOSE).unwrap(), 100.0);
        near(levels.price(OPEN).unwrap(), 110.0);

        early_bars.push(px(close_ts, 100.0, 101.0, 99.0, 80.0, 1.0));
        let doubled = build("SPY", Timeframe::M30, &early_bars).unwrap();
        assert!(doubled.price(PRIOR_CLOSE).is_none());
        assert!(doubled.notes.iter().any(|note| note.contains("older close did not stand in")), "{:?}", doubled.notes);
    }

    #[test]
    fn missing_or_unusable_prior_close_does_not_use_an_older_or_earlier_print() {
        let today = NaiveDate::from_ymd_opt(2026, 1, 6).unwrap();
        let prior = previous_session(today);
        let older = previous_session(prior);
        let opens = regular_opens(prior, Timeframe::M30);
        let mut bars = vec![
            px(*regular_opens(older, Timeframe::M30).last().unwrap(), 80.0, 81.0, 79.0, 80.0, 1.0),
            px(opens[opens.len() - 2], 90.0, 91.0, 89.0, 90.0, 1.0),
            px(regular_opens(today, Timeframe::M30)[0], 100.0, 110.0, 90.0, 105.0, 10.0),
        ];
        let missing = build("AAPL", Timeframe::M30, &bars).unwrap();
        assert!(missing.price(PRIOR_CLOSE).is_none());
        assert!(missing.price(OPEN).is_some());
        assert!(missing.price(VWAP).is_some());

        bars.push(px(*opens.last().unwrap(), 90.0, 91.0, 89.0, f64::NAN, 1.0));
        let unusable = build("AAPL", Timeframe::M30, &bars).unwrap();
        assert_eq!(unusable, missing);
    }

    #[test]
    fn an_incomplete_range_sample_bars_and_non_cash_names_stay_honest() {
        let today = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        let prior = previous_session(today);
        let opens = regular_opens(today, Timeframe::M5);
        let mut bars = vec![px(*regular_opens(prior, Timeframe::M5).last().unwrap(), 10.0, 10.0, 10.0, 10.0, 1.0)];
        for ts in opens.into_iter().take(3) {
            bars.push(px(ts, 11.0, 12.0, 10.5, 11.5, 1.0));
        }
        let partial = build("AAPL", Timeframe::M5, &bars).unwrap();
        assert!(partial.price(OPEN).is_some());
        assert!(partial.price(OR_HIGH).is_none());
        assert!(partial.notes.iter().any(|note| note.contains("not complete")), "{:?}", partial.notes);

        let mut hour_bars = vec![px(*regular_opens(prior, Timeframe::H1).last().unwrap(), 10.0, 10.0, 10.0, 10.0, 1.0)];
        hour_bars.push(px(regular_opens(today, Timeframe::H1)[0], 11.0, 12.0, 10.5, 11.5, 1.0));
        let hour = build("AAPL", Timeframe::H1, &hour_bars).unwrap();
        assert!(hour.price(OR_HIGH).is_none());
        assert!(hour.price(OPEN).is_some());
        assert!(hour.notes.iter().any(|note| note.contains("does not tile")), "{:?}", hour.notes);

        let mut sample = bars.clone();
        sample[0].provenance = Provenance::Sample;
        let err = build("AAPL", Timeframe::M5, &sample).unwrap_err();
        assert!(err.contains("SAMPLE"), "{err}");
        let err = build("BTCUSD", Timeframe::M5, &bars).unwrap_err();
        assert!(err.contains("cash session"), "{err}");
        let err = build("AAPL", Timeframe::M5, &[]).unwrap_err();
        assert!(err.contains("premarket"), "{err}");
        let pre = px(regular_opens(today, Timeframe::M5)[0] - 3600, 11.0, 12.0, 10.0, 11.0, 1.0);
        let err = build("AAPL", Timeframe::M5, &[pre]).unwrap_err();
        assert!(err.contains("premarket"), "{err}");
        assert_eq!(fetch_limit(), 400);
    }
}
