//! Earnings dates for a list of names, taken only from quotes the provider already sent.
//!
//! A name with a quote and no earnings fields is undated. A name with no quote is waiting.
//! Neither case receives a guessed date, and an archive bar date is not an earnings date.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::NaiveDate;

use crate::{Provenance, Quote};

/// One listed name. `symbol` is the quote-map key.
pub struct Listed<'a> {
    pub symbol: &'a str,
    pub ticker: &'a str,
}

/// A provider earnings window for one name.
#[derive(Debug, Clone, PartialEq)]
pub struct EarningsRow {
    pub symbol: String,
    pub ticker: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub label: String,
    pub provenance: Provenance,
    /// Calendar days from today's New York date to `start`. Negative means that date has passed.
    pub days: i64,
}

impl EarningsRow {
    pub fn line(&self) -> String {
        format!("{}   {}   [{}]", self.ticker, self.label, self.provenance.label())
    }
}

/// Names that share a New York start date.
#[derive(Debug, Clone, PartialEq)]
pub struct EarningsDay {
    pub date: NaiveDate,
    pub rows: Vec<EarningsRow>,
}

impl EarningsDay {
    pub fn heading(&self) -> String {
        let days = self.rows.first().map(|r| r.days).unwrap_or(0);
        format!("{} · {}", self.date.format("%Y-%m-%d"), when(days))
    }
}

/// A name that cannot be placed on a date.
#[derive(Debug, Clone, PartialEq)]
pub struct EarningsHole {
    pub symbol: String,
    pub ticker: String,
    /// `None` when no quote has arrived. `Some` when the quote omitted the earnings fields.
    pub provenance: Option<Provenance>,
}

impl EarningsHole {
    pub fn line(&self) -> String {
        match self.provenance {
            Some(p) => format!("{}   no earnings date on the quote   [{}]", self.ticker, p.label()),
            None => format!("{}   no quote yet", self.ticker),
        }
    }
}

/// Watchlist earnings, split by what the quotes actually contained.
#[derive(Debug, Clone, PartialEq)]
pub struct EarningsCalendar {
    /// Start date is today or later, soonest first.
    pub ahead: Vec<EarningsDay>,
    /// Provider date is already before today in New York, most recent first.
    pub passed: Vec<EarningsDay>,
    pub undated: Vec<EarningsHole>,
    pub awaiting: Vec<EarningsHole>,
}

impl EarningsCalendar {
    pub fn summary(&self) -> String {
        let names = |days: &[EarningsDay]| days.iter().map(|d| d.rows.len()).sum::<usize>();
        format!(
            "{} ahead · {} date passed · {} no date · {} no quote",
            names(&self.ahead),
            names(&self.passed),
            self.undated.len(),
            self.awaiting.len()
        )
    }
}

pub const AHEAD: &str = "Ahead";
pub const PASSED: &str = "Date already passed";
pub const UNDATED: &str = "No earnings date";
pub const AWAITING: &str = "No quote yet";

/// `in 3 days`, `today`, `1 day ago`.
pub fn when(days: i64) -> String {
    match days {
        0 => "today".into(),
        1 => "in 1 day".into(),
        n if n > 1 => format!("in {n} days"),
        -1 => "1 day ago".into(),
        n => format!("{} days ago", n.abs()),
    }
}

/// Build the calendar for `names` from `quotes` as of `now` (Unix seconds).
/// Duplicate symbols keep the first listing. Order inside a day is by ticker.
pub fn calendar(names: &[Listed<'_>], quotes: &HashMap<String, Quote>, now: i64) -> EarningsCalendar {
    let today = tr_core::ny::civil(now).date();
    let mut seen = HashSet::new();
    let mut ahead: BTreeMap<NaiveDate, Vec<EarningsRow>> = BTreeMap::new();
    let mut passed: BTreeMap<NaiveDate, Vec<EarningsRow>> = BTreeMap::new();
    let mut undated = Vec::new();
    let mut awaiting = Vec::new();
    for name in names {
        if !seen.insert(name.symbol) {
            continue;
        }
        let ticker = if name.ticker.is_empty() { name.symbol } else { name.ticker };
        let Some(quote) = quotes.get(name.symbol) else {
            awaiting.push(EarningsHole { symbol: name.symbol.into(), ticker: ticker.into(), provenance: None });
            continue;
        };
        let Some((start, end)) = quote.stats.earnings_window() else {
            undated.push(EarningsHole {
                symbol: name.symbol.into(),
                ticker: ticker.into(),
                provenance: Some(quote.provenance),
            });
            continue;
        };
        let days = start.signed_duration_since(today).num_days();
        let row = EarningsRow {
            symbol: name.symbol.into(),
            ticker: ticker.into(),
            start,
            end,
            label: quote.stats.earnings_label().unwrap_or_else(|| format!("ERN {start}")),
            provenance: quote.provenance,
            days,
        };
        let bucket = if days >= 0 { &mut ahead } else { &mut passed };
        bucket.entry(start).or_default().push(row);
    }
    let days = |map: BTreeMap<NaiveDate, Vec<EarningsRow>>, newest_first: bool| {
        let mut days: Vec<EarningsDay> = map
            .into_iter()
            .map(|(date, mut rows)| {
                rows.sort_by(|a, b| a.ticker.cmp(&b.ticker));
                EarningsDay { date, rows }
            })
            .collect();
        if newest_first {
            days.reverse();
        }
        days
    };
    let by_ticker = |rows: &mut Vec<EarningsHole>| rows.sort_by(|a, b| a.ticker.cmp(&b.ticker));
    by_ticker(&mut undated);
    by_ticker(&mut awaiting);
    EarningsCalendar { ahead: days(ahead, false), passed: days(passed, true), undated, awaiting }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QuoteStats;

    fn ny(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
        tr_core::ny::to_utc(chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, min, 0).unwrap())
    }

    fn quote(symbol: &str, provenance: Provenance, ts: Option<i64>, start: Option<i64>, end: Option<i64>) -> Quote {
        Quote {
            provenance,
            symbol: symbol.into(),
            last: 10.0,
            prev_close: 9.0,
            ts: 1,
            spark: vec![],
            stats: QuoteStats { earnings_ts: ts, earnings_start: start, earnings_end: end, ..QuoteStats::default() },
        }
    }

    #[test]
    fn calendar_groups_provider_dates_and_leaves_holes_blank() {
        // Wednesday 2026-10-21, during the cash session.
        let now = ny(2026, 10, 21, 10, 0);
        let ahead = ny(2026, 10, 29, 16, 0);
        let window_end = ny(2026, 10, 30, 16, 0);
        let today = ny(2026, 10, 21, 16, 0);
        let passed = ny(2026, 10, 15, 16, 0);
        // Midnight UTC on the 30th is still the 29th in New York.
        let midnight_utc =
            chrono::NaiveDate::from_ymd_opt(2026, 10, 30).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
        let mut quotes = HashMap::new();
        quotes.insert(
            "MSFT US Equity".into(),
            quote("MSFT US Equity", Provenance::Cached, Some(ahead), Some(ahead), Some(window_end)),
        );
        quotes.insert("AAPL US Equity".into(), quote("AAPL US Equity", Provenance::Real, Some(ahead), None, None));
        quotes.insert("NVDA US Equity".into(), quote("NVDA US Equity", Provenance::Real, Some(today), None, None));
        quotes.insert("IBM US Equity".into(), quote("IBM US Equity", Provenance::Real, Some(passed), None, None));
        quotes
            .insert("ORCL US Equity".into(), quote("ORCL US Equity", Provenance::Real, Some(midnight_utc), None, None));
        quotes.insert("AMD US Equity".into(), quote("AMD US Equity", Provenance::Sample, Some(0), None, None));
        quotes.insert("INTC US Equity".into(), quote("INTC US Equity", Provenance::Real, None, None, None));
        let names = [
            Listed { symbol: "MSFT US Equity", ticker: "MSFT" },
            Listed { symbol: "AAPL US Equity", ticker: "AAPL" },
            Listed { symbol: "AAPL US Equity", ticker: "AAPL" },
            Listed { symbol: "NVDA US Equity", ticker: "NVDA" },
            Listed { symbol: "IBM US Equity", ticker: "IBM" },
            Listed { symbol: "ORCL US Equity", ticker: "ORCL" },
            Listed { symbol: "AMD US Equity", ticker: "AMD" },
            Listed { symbol: "INTC US Equity", ticker: "INTC" },
            Listed { symbol: "TSLA US Equity", ticker: "TSLA" },
        ];
        let cal = calendar(&names, &quotes, now);
        assert_eq!(cal.summary(), "4 ahead · 1 date passed · 2 no date · 1 no quote");
        assert_eq!(cal.ahead.len(), 2);
        assert_eq!(cal.ahead[0].heading(), "2026-10-21 · today");
        assert_eq!(cal.ahead[0].rows[0].ticker, "NVDA");
        assert_eq!(cal.ahead[1].date.to_string(), "2026-10-29");
        assert_eq!(cal.ahead[1].heading(), "2026-10-29 · in 8 days");
        assert_eq!(
            cal.ahead[1].rows.iter().map(|r| r.ticker.as_str()).collect::<Vec<_>>(),
            vec!["AAPL", "MSFT", "ORCL"]
        );
        assert_eq!(cal.ahead[1].rows[0].label, "ERN 2026-10-29");
        assert_eq!(cal.ahead[1].rows[1].label, "ERN 2026-10-29–2026-10-30");
        assert_eq!(cal.ahead[1].rows[1].line(), "MSFT   ERN 2026-10-29–2026-10-30   [CACHED]");
        assert!(cal.ahead[1].rows[2].label.starts_with("ERN 2026-10-29"));
        assert_eq!(cal.passed.len(), 1);
        assert_eq!(cal.passed[0].heading(), "2026-10-15 · 6 days ago");
        assert_eq!(cal.passed[0].rows[0].ticker, "IBM");
        assert_eq!(cal.undated.iter().map(|h| h.ticker.as_str()).collect::<Vec<_>>(), vec!["AMD", "INTC"]);
        assert!(cal.undated.iter().all(|h| h.provenance.is_some()));
        assert!(cal.undated[0].line().contains("no earnings date"));
        assert_eq!(cal.awaiting.len(), 1);
        assert_eq!(cal.awaiting[0].line(), "TSLA   no quote yet");
        assert!(cal.undated.iter().chain(cal.awaiting.iter()).all(|h| !h.line().contains("ERN")));
    }
}
