//! Intraday scan of a named list. Every number comes from verified 5-minute bars.
//!
//! Relative volume stays blank until a prior regular session has volume at the same
//! offset. A name with no regular-session print, unverified bars, or no US cash
//! session is a hole. Sample bars are never turned into a rank.

use std::collections::HashSet;

use tr_core::{AssetClass, Bar, Provenance};

use crate::intraday::{self, Freshness, MarketClock};

/// 5-minute bars read per name. One cash session is 78 bars; this covers that session
/// and earlier ones so relative volume has a prior day when the provider sent one.
pub const BARS: usize = 400;

pub const LIVE: &str = "Live";
pub const CLOSED: &str = "Session closed";
pub const STALE: &str = "Last bar is stale";
pub const HOLES: &str = "No read";

/// One name whose verified bars support a regular-session print.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub symbol: String,
    pub ticker: String,
    pub close: f64,
    pub vwap: f64,
    pub rvol: Option<f64>,
    pub bar_ts: i64,
    pub freshness: Freshness,
    pub provenance: Provenance,
}

impl Row {
    pub fn side(&self) -> &'static str {
        if self.close >= self.vwap { "above VWAP" } else { "below VWAP" }
    }

    pub fn line(&self) -> String {
        let rvol = self.rvol.map(|v| format!("{v:.2}×")).unwrap_or_else(|| "—".into());
        format!(
            "{:<6}  RVOL {rvol}   {:.2} {} {:.2}  [{}]",
            self.ticker,
            self.close,
            self.side(),
            self.vwap,
            self.provenance.label()
        )
    }
}

/// A name that cannot be ranked. The reason is why, not a guessed volume.
#[derive(Debug, Clone, PartialEq)]
pub struct Hole {
    pub symbol: String,
    pub ticker: String,
    pub reason: String,
}

impl Hole {
    pub fn line(&self) -> String {
        format!("{:<6}  {}", self.ticker, self.reason)
    }
}

/// Ranked reads split by the intraday clock, plus names that produced no read.
#[derive(Debug, Clone, PartialEq)]
pub struct Board {
    pub live: Vec<Row>,
    pub closed: Vec<Row>,
    pub stale: Vec<Row>,
    pub holes: Vec<Hole>,
    /// Sample provider: ranks are not computed.
    pub sample: bool,
}

impl Board {
    pub fn summary(&self) -> String {
        format!(
            "{} live · {} session closed · {} stale · {} no read",
            self.live.len(),
            self.closed.len(),
            self.stale.len(),
            self.holes.len()
        )
    }
}

fn cash(class: AssetClass) -> bool {
    matches!(class, AssetClass::Equity | AssetClass::Etf | AssetClass::Index)
}

/// Session print, VWAP, and relative volume for one symbol. `now` is Unix seconds.
pub fn read_bars(symbol: &str, bars: &[Bar], now: i64) -> Result<Row, String> {
    let sec = crate::security(symbol);
    if !cash(sec.class) {
        return Err("no US cash session".into());
    }
    Bar::require_verified(bars, "intraday scan")?;
    let print =
        intraday::session_print(tr_core::Timeframe::M5, bars).ok_or_else(|| "no regular-session print".to_string())?;
    let freshness = intraday::freshness(MarketClock::for_class(sec.class), tr_core::Timeframe::M5, print.bar_ts, now)
        .ok_or_else(|| "no intraday clock for these bars".to_string())?;
    Ok(Row {
        symbol: sec.key(),
        ticker: sec.ticker,
        close: print.close,
        vwap: print.vwap,
        rvol: intraday::relative_volume(tr_core::Timeframe::M5, bars),
        bar_ts: print.bar_ts,
        freshness,
        provenance: Bar::provenance(bars),
    })
}

fn sort_rows(rows: &mut [Row]) {
    rows.sort_by(|a, b| match (a.rvol, b.rvol) {
        (Some(x), Some(y)) => y.total_cmp(&x).then_with(|| a.ticker.cmp(&b.ticker)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.ticker.cmp(&b.ticker),
    });
}

/// Place each read. Duplicate symbols keep the first result. Unverified rows become holes.
pub fn build(items: Vec<(String, Result<Row, String>)>, sample: bool) -> Board {
    let mut seen = HashSet::new();
    let mut live = Vec::new();
    let mut closed = Vec::new();
    let mut stale = Vec::new();
    let mut holes = Vec::new();
    for (symbol, result) in items {
        let sec = crate::security(&symbol);
        let key = sec.key();
        if !seen.insert(key.clone()) {
            continue;
        }
        match result {
            Ok(row) if row.provenance.is_verified() => match row.freshness {
                Freshness::Current => live.push(row),
                Freshness::Closed => closed.push(row),
                Freshness::Stale { .. } => stale.push(row),
            },
            Ok(row) => holes.push(Hole {
                symbol: key,
                ticker: sec.ticker,
                reason: format!("refused {}", row.provenance.label()),
            }),
            Err(reason) => holes.push(Hole { symbol: key, ticker: sec.ticker, reason }),
        }
    }
    sort_rows(&mut live);
    sort_rows(&mut closed);
    sort_rows(&mut stale);
    holes.sort_by(|a, b| a.ticker.cmp(&b.ticker));
    Board { live, closed, stale, holes, sample }
}

/// What to record when the provider is sample. No history call, no rank.
pub fn sample_reason(symbol: &str) -> String {
    if cash(crate::security(symbol).class) { "sample bars are not ranked".into() } else { "no US cash session".into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
        tr_core::ny::to_utc(chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, min, 0).unwrap())
    }

    fn bar(ts: i64, close: f64, volume: f64) -> Bar {
        Bar::new(ts, close, close + 1.0, close - 1.0, close, volume).with_provenance(Provenance::Real)
    }

    fn session() -> Vec<Bar> {
        vec![
            bar(wall(2026, 3, 6, 9, 30), 11.0, 100.0),
            bar(wall(2026, 3, 6, 9, 35), 11.0, 100.0),
            bar(wall(2026, 3, 9, 9, 30), 11.0, 300.0),
            bar(wall(2026, 3, 9, 9, 35), 21.0, 100.0),
            bar(wall(2026, 3, 9, 8, 0), 1.0, 1_000.0),
        ]
    }

    #[test]
    fn live_rank_uses_session_rvol_and_leaves_a_missing_prior_blank() {
        let now = wall(2026, 3, 9, 9, 36);
        let row = read_bars("AAPL", &session(), now).unwrap();
        assert!(matches!(row.freshness, Freshness::Current));
        assert!((row.close - 21.0).abs() < 1e-9);
        assert!((row.rvol.unwrap() - 2.0).abs() < 1e-9);
        assert!(row.line().contains("RVOL 2.00×"));
        assert!(row.line().contains("above VWAP"));
        assert!(row.line().contains("[REAL]"));

        let today = session().into_iter().filter(|b| b.ts >= wall(2026, 3, 9, 0, 0)).collect::<Vec<_>>();
        let bare = read_bars("MSFT", &today, now).unwrap();
        assert!(bare.rvol.is_none());
        assert!(bare.line().contains("RVOL —"));
        assert!(!bare.line().contains("RVOL 0"));

        let prior = bar(wall(2026, 3, 6, 9, 35), 10.0, 50.0);
        let low = Bar::new(wall(2026, 3, 9, 9, 35), 12.0, 20.0, 18.0, 12.0, 100.0).with_provenance(Provenance::Real);
        let below = read_bars("IBM", &[prior, low], now).unwrap();
        assert_eq!(below.side(), "below VWAP");

        let mut items =
            vec![("AAPL".into(), Ok(row)), ("MSFT".into(), Ok(bare)), ("AAPL".into(), Err("second listing".into()))];
        let below_row = below;
        items.insert(1, ("IBM".into(), Ok(below_row)));
        let board = build(items, false);
        assert_eq!(board.live.iter().map(|r| r.ticker.as_str()).collect::<Vec<_>>(), ["AAPL", "IBM", "MSFT"]);
        assert!(board.closed.is_empty() && board.stale.is_empty() && board.holes.is_empty());
        assert!(board.summary().starts_with("3 live"));
    }

    #[test]
    fn closed_and_stale_sessions_stay_out_of_the_live_rank() {
        let closed_bars = vec![bar(wall(2026, 3, 5, 15, 55), 10.0, 100.0), bar(wall(2026, 3, 6, 15, 55), 12.0, 200.0)];
        let closed = read_bars("AAPL", &closed_bars, wall(2026, 3, 7, 12, 0)).unwrap();
        assert!(matches!(closed.freshness, Freshness::Closed));
        assert!((closed.rvol.unwrap() - 2.0).abs() < 1e-9);

        let stale = read_bars("NVDA", &session(), wall(2026, 3, 9, 10, 7)).unwrap();
        assert!(matches!(stale.freshness, Freshness::Stale { .. }));
        assert!((stale.rvol.unwrap() - 2.0).abs() < 1e-9);

        let board = build(vec![("AAPL".into(), Ok(closed)), ("NVDA".into(), Ok(stale))], false);
        assert!(board.live.is_empty());
        assert_eq!(board.closed.len(), 1);
        assert_eq!(board.stale.len(), 1);
        assert_eq!(board.stale[0].ticker, "NVDA");
    }

    #[test]
    fn sample_zero_volume_and_non_cash_names_are_holes() {
        let now = wall(2026, 3, 9, 9, 36);
        let zero = [bar(wall(2026, 3, 9, 9, 30), 11.0, 0.0)];
        let err = read_bars("AAPL", &zero, now).unwrap_err();
        assert!(err.contains("no regular-session print"), "{err}");

        let sample = [bar(wall(2026, 3, 9, 9, 35), 21.0, 100.0).with_provenance(Provenance::Sample)];
        let err = read_bars("AAPL", &sample, now).unwrap_err();
        assert!(err.contains("SAMPLE"), "{err}");

        let fx = read_bars("EURUSD", &session(), now).unwrap_err();
        assert_eq!(fx, "no US cash session");
        assert!(!fx.chars().any(|c| c.is_ascii_digit()));

        let board = build(
            vec![
                ("AAPL".into(), read_bars("AAPL", &sample, now)),
                ("EURUSD".into(), Err(sample_reason("EURUSD"))),
                ("MSFT".into(), Err(sample_reason("MSFT"))),
            ],
            true,
        );
        assert!(board.sample);
        assert!(board.live.is_empty() && board.closed.is_empty() && board.stale.is_empty());
        assert!(board.holes.iter().any(|h| h.ticker == "AAPL" && h.reason.contains("SAMPLE")));
        assert!(board.holes.iter().any(|h| h.line().contains("no US cash session")));
        assert!(board.holes.iter().any(|h| h.reason == "sample bars are not ranked"));
        assert!(board.holes.iter().all(|h| !h.line().contains("RVOL")));
        let index = read_bars("SPX Index", &session(), now).unwrap();
        assert_eq!(index.ticker, "SPX");
        assert!(matches!(index.freshness, Freshness::Current));
    }
}
