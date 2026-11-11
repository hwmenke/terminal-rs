//! The completed prefix of a bar series, using the data's observation clock.
use crate::history::calendar::{last_expected_session, next_session};
use chrono::{DateTime, Months, Utc};
use tr_core::{AssetClass, Bar, Timeframe};

/// Successors close all preceding bars. Without an observation time, the last
/// bar stays pending. Live callers must pass their last refresh, not wall time.
pub fn closed_len(tf: Timeframe, bars: &[Bar], class: AssetClass, as_of: Option<i64>) -> usize {
    let Some(last) = bars.last() else { return 0 };
    let pending = bars.len() - 1;
    let Some(now) = as_of.and_then(|t| DateTime::<Utc>::from_timestamp(t, 0)) else { return pending };
    let Some(stamp) = DateTime::<Utc>::from_timestamp(last.ts, 0) else { return pending };
    let start = tf.bucket_start(last.ts);
    let equity = matches!(class, AssetClass::Equity | AssetClass::Etf | AssetClass::Index);
    let closed = if !tf.is_intraday() && equity {
        let expected = last_expected_session(now);
        match tf {
            Timeframe::D1 => stamp.date_naive() <= expected,
            Timeframe::W1 | Timeframe::MN1 => {
                let next = next_session(expected).and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
                tf.bucket_start(next) > start
            }
            _ => unreachable!(),
        }
    } else {
        let end = if tf == Timeframe::MN1 {
            DateTime::<Utc>::from_timestamp(start, 0)
                .and_then(|d| d.checked_add_months(Months::new(1)))
                .map(|d| d.timestamp())
        } else {
            start.checked_add(tf.seconds())
        };
        end.is_some_and(|end| end <= now.timestamp())
    };
    pending + usize::from(closed)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ts(s: &str) -> i64 {
        s.parse::<DateTime<Utc>>().unwrap().timestamp()
    }
    fn bars(s: &str) -> Vec<Bar> {
        vec![Bar::new(ts(s), 1.0, 1.0, 1.0, 1.0, 1.0)]
    }
    fn count(tf: Timeframe, bar: &str, now: &str) -> usize {
        closed_len(tf, &bars(bar), AssetClass::Equity, Some(ts(now)))
    }
    #[test]
    fn archive_today_daily_keeps_post_close_grace() {
        for class in [AssetClass::Equity, AssetClass::Etf, AssetClass::Index] {
            let b = bars("2026-11-11T00:00:00Z");
            assert_eq!(closed_len(Timeframe::D1, &b, class, Some(ts("2026-11-11T21:14:59Z"))), 0);
            assert_eq!(closed_len(Timeframe::D1, &b, class, Some(ts("2026-11-11T21:15:00Z"))), 1);
        }
        // DST uses the same New York cutoff.
        assert_eq!(count(Timeframe::D1, "2026-07-06T00:00:00Z", "2026-07-06T20:14:59Z"), 0);
        assert_eq!(count(Timeframe::D1, "2026-07-06T00:00:00Z", "2026-07-06T20:15:00Z"), 1);
    }
    #[test]
    fn archive_week_midweek_and_friday_close() {
        let bar = "2026-11-09T00:00:00Z";
        assert_eq!(count(Timeframe::W1, bar, "2026-11-11T22:00:00Z"), 0);
        assert_eq!(count(Timeframe::W1, bar, "2026-11-13T21:14:59Z"), 0);
        assert_eq!(count(Timeframe::W1, bar, "2026-11-13T21:15:00Z"), 1);
        assert_eq!(count(Timeframe::W1, bar, "2026-11-18T12:00:00Z"), 1);
    }
    #[test]
    fn archive_month_midmonth_and_last_session() {
        let bar = "2026-01-01T00:00:00Z";
        assert_eq!(count(Timeframe::MN1, bar, "2026-01-15T22:00:00Z"), 0);
        assert_eq!(count(Timeframe::MN1, bar, "2026-01-30T21:14:59Z"), 0);
        assert_eq!(count(Timeframe::MN1, bar, "2026-01-30T21:15:00Z"), 1);
    }
    #[test]
    fn holiday_shortened_week_closes_on_thursday() {
        let bar = "2026-03-30T00:00:00Z";
        assert_eq!(count(Timeframe::W1, bar, "2026-04-02T20:14:59Z"), 0);
        assert_eq!(count(Timeframe::W1, bar, "2026-04-02T20:15:00Z"), 1);
        // Observed Independence Day on Friday also ends the week on Thursday.
        assert_eq!(count(Timeframe::W1, "2026-06-29T00:00:00Z", "2026-07-02T20:15:00Z"), 1);
        assert_eq!(count(Timeframe::D1, "2026-11-27T00:00:00Z", "2026-11-27T18:15:00Z"), 1);
    }
    #[test]
    fn live_intraday_uses_refresh_inside_or_past_bucket() {
        let b = bars("2026-11-11T15:32:00Z");
        assert_eq!(closed_len(Timeframe::M5, &b, AssetClass::Equity, Some(ts("2026-11-11T15:34:59Z"))), 0);
        assert_eq!(closed_len(Timeframe::M5, &b, AssetClass::Equity, Some(ts("2026-11-11T15:35:00Z"))), 1);
    }
    #[test]
    fn stale_partial_stays_pending_and_successor_closes_predecessor() {
        let mut b = bars("2026-11-11T15:30:00Z");
        let stale_refresh = Some(ts("2026-11-11T15:32:00Z"));
        for _ in 0..2 {
            assert_eq!(closed_len(Timeframe::M5, &b, AssetClass::Equity, stale_refresh), 0);
        }
        assert_eq!(closed_len(Timeframe::M5, &b, AssetClass::Equity, None), 0);
        b.extend(bars("2026-11-11T15:35:00Z"));
        assert_eq!(closed_len(Timeframe::M5, &b, AssetClass::Equity, None), 1);
        assert_eq!(closed_len(Timeframe::M5, &[], AssetClass::Equity, None), 0);
    }
    #[test]
    fn crypto_uses_utc_bucket_ends_including_calendar_months() {
        for (tf, bar, end) in [
            (Timeframe::H1, "2026-11-11T15:00:00Z", "2026-11-11T16:00:00Z"),
            (Timeframe::D1, "2026-11-11T00:00:00Z", "2026-11-12T00:00:00Z"),
            (Timeframe::W1, "2026-11-09T00:00:00Z", "2026-11-16T00:00:00Z"),
            (Timeframe::MN1, "2026-02-01T00:00:00Z", "2026-03-01T00:00:00Z"),
        ] {
            let b = bars(bar);
            assert_eq!(closed_len(tf, &b, AssetClass::Crypto, Some(ts(end) - 1)), 0);
            assert_eq!(closed_len(tf, &b, AssetClass::Crypto, Some(ts(end))), 1);
        }
    }
}
