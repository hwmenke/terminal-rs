//! Pure span-to-bar mapping for linked charts (Time Link ghost cursor and Lens).
//!
//! Everything is in UTC seconds, so daylight-saving changes never move a mapping.
//! Nothing here creates a price: it only says which already-loaded bars cover a time.
use std::ops::Range;

use tr_core::{AssetClass, Bar, Timeframe};

/// True for the US cash-session classes whose intraday bars end at the cash close.
pub fn cash_class(class: AssetClass) -> bool {
    matches!(class, AssetClass::Equity | AssetClass::Etf | AssetClass::Index)
}

/// End of the bar that opens at `ts`. For equity intraday bars that open before that
/// date's cash close, the end is capped at the close (the rule `closed_len` uses), so the
/// last bar of a half day does not claim time after 13:00.
pub fn bar_end(tf: Timeframe, ts: i64, equity: bool) -> i64 {
    let end = ts.saturating_add(tf.seconds());
    if !(equity && tf.is_intraday()) {
        return end;
    }
    let date = tr_core::ny::civil(ts).date();
    let close = crate::intraday::close_minute(date);
    let Some(wall) = date.and_hms_opt(close / 60, close % 60, 0) else { return end };
    let cash_close = tr_core::ny::to_utc(wall);
    if ts < cash_close { end.min(cash_close) } else { end }
}

/// Bars of `bars` (interval `tf`) that overlap the half-open span `[start, end)`:
/// `lo` is the first bar whose end is after `start`, `hi` the first bar that opens at
/// or after `end`. An empty range means no loaded bar covers that time.
pub fn bars_in_span(bars: &[Bar], tf: Timeframe, equity: bool, span: (i64, i64)) -> Range<usize> {
    let (start, end) = span;
    if end <= start {
        return 0..0;
    }
    let lo = bars.partition_point(|b| bar_end(tf, b.ts, equity) <= start);
    let hi = bars.partition_point(|b| b.ts < end);
    if lo >= hi { lo.min(bars.len())..lo.min(bars.len()) } else { lo..hi }
}

/// The span `[open, end)` of bar `i`, or `None` past the end of the series.
pub fn span_of(bars: &[Bar], tf: Timeframe, equity: bool, i: usize) -> Option<(i64, i64)> {
    let b = bars.get(i)?;
    Some((b.ts, bar_end(tf, b.ts, equity)))
}

/// The time window covered by bars `lo..hi`: first open to last end.
pub fn window_of(bars: &[Bar], tf: Timeframe, equity: bool, lo: usize, hi: usize) -> Option<(i64, i64)> {
    if lo >= hi || hi > bars.len() {
        return None;
    }
    Some((bars[lo].ts, bar_end(tf, bars[hi - 1].ts, equity)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn ny(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
        tr_core::ny::to_utc(NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, min, 0).unwrap())
    }
    fn series(date: (i32, u32, u32), tf: Timeframe, skip: &[usize]) -> Vec<Bar> {
        crate::intraday::regular_opens(NaiveDate::from_ymd_opt(date.0, date.1, date.2).unwrap(), tf)
            .into_iter()
            .enumerate()
            .filter(|(i, _)| !skip.contains(i))
            .map(|(_, ts)| Bar::new(ts, 10.0, 10.5, 9.5, 10.2, 100.0))
            .collect()
    }

    #[test]
    fn fifteen_minute_span_covers_three_five_minute_bars() {
        let five = series((2026, 11, 10), Timeframe::M5, &[]);
        let s = ny(2026, 11, 10, 10, 0);
        let r = bars_in_span(&five, Timeframe::M5, true, (s, bar_end(Timeframe::M15, s, true)));
        assert_eq!(r.len(), 3);
        assert_eq!(five[r.start].ts, s);
        assert_eq!(five[r.end - 1].ts, ny(2026, 11, 10, 10, 10));
    }

    #[test]
    fn one_minute_span_selects_containing_fifteen_minute_bar() {
        let fifteen = series((2026, 11, 10), Timeframe::M15, &[]);
        let s = ny(2026, 11, 10, 10, 7);
        let r = bars_in_span(&fifteen, Timeframe::M15, true, (s, s + 60));
        assert_eq!(r.len(), 1);
        assert_eq!(fifteen[r.start].ts, ny(2026, 11, 10, 10, 0));
    }

    #[test]
    fn span_over_missing_bucket_is_empty() {
        // 10:00 5m bucket (index 6) is missing; nothing is filled in.
        let five = series((2026, 11, 10), Timeframe::M5, &[6]);
        let s = ny(2026, 11, 10, 10, 1);
        assert!(bars_in_span(&five, Timeframe::M5, true, (s, s + 60)).is_empty());
        // The 15m span still finds the two present buckets, not three.
        let s = ny(2026, 11, 10, 10, 0);
        assert_eq!(bars_in_span(&five, Timeframe::M5, true, (s, s + 900)).len(), 2);
    }

    #[test]
    fn span_before_first_or_after_last_bar_is_empty() {
        let five = series((2026, 11, 10), Timeframe::M5, &[]);
        let before = ny(2026, 11, 10, 9, 0);
        assert!(bars_in_span(&five, Timeframe::M5, true, (before, before + 300)).is_empty());
        let after = ny(2026, 11, 10, 16, 5);
        assert!(bars_in_span(&five, Timeframe::M5, true, (after, after + 300)).is_empty());
        assert!(bars_in_span(&[], Timeframe::M5, true, (after, after + 300)).is_empty());
    }

    #[test]
    fn half_day_last_bar_does_not_claim_time_after_the_close() {
        // 2026-11-27 is the Friday after Thanksgiving: the cash session ends at 13:00.
        let date = (2026, 11, 27);
        let fifteen = series(date, Timeframe::M15, &[]);
        let last = *fifteen.last().unwrap();
        assert_eq!(last.ts, ny(2026, 11, 27, 12, 45));
        assert_eq!(bar_end(Timeframe::M15, last.ts, true), ny(2026, 11, 27, 13, 0));
        // An hourly bar opening at 12:30 is cut at 13:00, so a 13:00 post-close print maps nowhere.
        let hourly = series(date, Timeframe::H1, &[]);
        assert_eq!(hourly.last().unwrap().ts, ny(2026, 11, 27, 12, 30));
        let post = ny(2026, 11, 27, 13, 0);
        assert!(bars_in_span(&hourly, Timeframe::H1, true, (post, post + 60)).is_empty());
        // The 12:45 15m bar maps to the 1m bars 12:45–12:59 only, even with a 13:00 print present.
        let mut ones = series(date, Timeframe::M1, &[]);
        ones.push(Bar::new(post, 10.0, 10.0, 10.0, 10.0, 1.0));
        let span = span_of(&fifteen, Timeframe::M15, true, fifteen.len() - 1).unwrap();
        let r = bars_in_span(&ones, Timeframe::M1, true, span);
        assert_eq!(r.len(), 15);
        assert_eq!(ones[r.end - 1].ts, ny(2026, 11, 27, 12, 59));
    }

    #[test]
    fn mapping_is_unchanged_across_dst_start_and_end() {
        // DST starts 2026-03-08 and ends 2026-11-01: sessions either side map identically.
        for date in [(2026, 3, 6), (2026, 3, 9), (2026, 10, 30), (2026, 11, 2)] {
            let five = series(date, Timeframe::M5, &[]);
            let fifteen = series(date, Timeframe::M15, &[]);
            let open = ny(date.0, date.1, date.2, 9, 30);
            assert_eq!(fifteen[0].ts, open, "{date:?}");
            let r = bars_in_span(&five, Timeframe::M5, true, span_of(&fifteen, Timeframe::M15, true, 0).unwrap());
            assert_eq!(r, 0..3, "{date:?}");
            let last = fifteen.len() - 1;
            let r = bars_in_span(&five, Timeframe::M5, true, span_of(&fifteen, Timeframe::M15, true, last).unwrap());
            assert_eq!(r, five.len() - 3..five.len(), "{date:?}");
            assert_eq!(bar_end(Timeframe::M15, fifteen[last].ts, true), ny(date.0, date.1, date.2, 16, 0));
        }
    }
}
