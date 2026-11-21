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
        let end = if tf.is_intraday() {
            last.ts.checked_add(tf.seconds()).map(|mut end| {
                if equity {
                    let date = tr_core::ny::civil(last.ts).date();
                    if tf == Timeframe::H4 && last.ts == start {
                        // Yahoo resamples :30 H1 opens into UTC H4 buckets and
                        // stamps the bucket start. Its final H1 can end past
                        // the bucket boundary; wait for that constituent too.
                        end = crate::intraday::regular_opens(date, Timeframe::H1)
                            .into_iter()
                            .filter(|&open| (last.ts..end).contains(&open))
                            .last()
                            .map(|open| open + Timeframe::H1.seconds())
                            .unwrap_or(end);
                    }
                    let close = crate::intraday::close_minute(date);
                    let cash_close = tr_core::ny::to_utc(date.and_hms_opt(close / 60, close % 60, 0).unwrap());
                    end = end.min(cash_close);
                }
                end
            })
        } else if tf == Timeframe::MN1 {
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
    fn live_intraday_uses_refresh_inside_or_past_actual_interval() {
        let b = bars("2026-11-11T15:32:00Z");
        assert_eq!(closed_len(Timeframe::M5, &b, AssetClass::Equity, Some(ts("2026-11-11T15:34:59Z"))), 0);
        assert_eq!(closed_len(Timeframe::M5, &b, AssetClass::Equity, Some(ts("2026-11-11T15:35:00Z"))), 0);
        assert_eq!(closed_len(Timeframe::M5, &b, AssetClass::Equity, Some(ts("2026-11-11T15:36:59Z"))), 0);
        assert_eq!(closed_len(Timeframe::M5, &b, AssetClass::Equity, Some(ts("2026-11-11T15:37:00Z"))), 1);
    }
    #[test]
    fn cash_hourly_open_waits_for_the_full_hour_in_est_and_edt() {
        for class in [AssetClass::Equity, AssetClass::Etf, AssetClass::Index] {
            // 09:30 New York is 14:30 UTC in November, 13:30 UTC in July.
            for (bar, end) in
                [("2026-11-11T14:30:00Z", "2026-11-11T15:30:00Z"), ("2026-07-06T13:30:00Z", "2026-07-06T14:30:00Z")]
            {
                let b = bars(bar);
                let end = ts(end);
                for (now, expected) in [(end - 15 * 60, 0), (end - 1, 0), (end, 1), (end + 60, 1)] {
                    assert_eq!(closed_len(Timeframe::H1, &b, class, Some(now)), expected, "{class:?} {bar} {now}");
                }
            }
        }
    }
    #[test]
    fn cash_hourly_last_bar_closes_at_regular_or_early_session_close() {
        for class in [AssetClass::Equity, AssetClass::Etf, AssetClass::Index] {
            for (bar, end) in [
                ("2026-11-11T20:30:00Z", "2026-11-11T21:00:00Z"), // 15:30–16:00 EST
                ("2026-11-27T17:30:00Z", "2026-11-27T18:00:00Z"), // 12:30–13:00 EST
                ("2026-07-06T19:30:00Z", "2026-07-06T20:00:00Z"), // 15:30–16:00 EDT
            ] {
                let b = bars(bar);
                assert_eq!(closed_len(Timeframe::H1, &b, class, Some(ts(end) - 1)), 0, "{class:?} {bar}");
                assert_eq!(closed_len(Timeframe::H1, &b, class, Some(ts(end))), 1, "{class:?} {bar}");
            }
        }
    }
    #[test]
    fn cash_h4_actual_starts_close_after_four_hours_or_at_session_close() {
        for (bar, end) in [
            ("2026-11-11T14:30:00Z", "2026-11-11T18:30:00Z"), // 09:30–13:30 EST
            ("2026-11-11T18:30:00Z", "2026-11-11T21:00:00Z"), // 13:30–16:00 EST
            ("2026-11-27T14:30:00Z", "2026-11-27T18:00:00Z"), // 09:30–13:00 EST
        ] {
            assert_eq!(count(Timeframe::H4, bar, end), 1);
            assert_eq!(closed_len(Timeframe::H4, &bars(bar), AssetClass::Equity, Some(ts(end) - 1)), 0);
        }
    }
    #[test]
    fn issue_103_afternoon_half_hour_bars_stay_forming_until_cash_close() {
        for class in [AssetClass::Equity, AssetClass::Etf, AssetClass::Index] {
            for (tf, bar) in [
                (Timeframe::H1, "2026-11-11T20:30:00Z"), // 15:30 ET
                (Timeframe::H4, "2026-11-11T18:30:00Z"), // 13:30 ET
            ] {
                let b = bars(bar);
                for now in ["2026-11-11T20:45:00Z", "2026-11-11T20:59:59Z"] {
                    assert_eq!(closed_len(tf, &b, class, Some(ts(now))), 0, "{class:?} {tf:?} {now}");
                }
                for now in ["2026-11-11T21:00:00Z", "2026-11-11T21:01:00Z"] {
                    assert_eq!(closed_len(tf, &b, class, Some(ts(now))), 1, "{class:?} {tf:?} {now}");
                }
            }
        }
    }
    #[test]
    fn yahoo_h4_resampled_hours_wait_for_the_last_constituent_to_close() {
        for (first, hours, expected) in [
            (
                "2026-11-11T14:30:00Z",
                7,
                vec![
                    ("2026-11-11T12:00:00Z", "2026-11-11T16:30:00Z"),
                    ("2026-11-11T16:00:00Z", "2026-11-11T20:30:00Z"),
                    ("2026-11-11T20:00:00Z", "2026-11-11T21:00:00Z"),
                ],
            ),
            (
                "2026-11-27T14:30:00Z",
                4,
                vec![
                    ("2026-11-27T12:00:00Z", "2026-11-27T16:30:00Z"),
                    ("2026-11-27T16:00:00Z", "2026-11-27T18:00:00Z"),
                ],
            ),
            (
                "2026-07-06T13:30:00Z",
                7,
                vec![
                    ("2026-07-06T12:00:00Z", "2026-07-06T16:30:00Z"),
                    ("2026-07-06T16:00:00Z", "2026-07-06T20:00:00Z"),
                ],
            ),
        ] {
            // Same session filtering and UTC-grid resampling as YahooProvider.
            let hourly = crate::intraday::regular_only(
                (0..hours).map(|i| Bar::new(ts(first) + i * 3600, 1.0, 1.0, 1.0, 1.0, 1.0)).collect(),
            );
            let h4 = Timeframe::H4.resample(&hourly);
            assert_eq!(h4.len(), expected.len());
            for (b, (start, end)) in h4.iter().zip(expected) {
                assert_eq!(b.ts, ts(start));
                for class in [AssetClass::Equity, AssetClass::Etf, AssetClass::Index] {
                    assert_eq!(closed_len(Timeframe::H4, std::slice::from_ref(b), class, Some(ts(end) - 1)), 0);
                    assert_eq!(closed_len(Timeframe::H4, std::slice::from_ref(b), class, Some(ts(end))), 1);
                }
            }
        }
    }
    #[test]
    fn hourly_successor_closes_previous_bar_even_without_a_new_refresh() {
        let mut b = bars("2026-11-11T14:30:00Z");
        let refresh = Some(ts("2026-11-11T15:15:00Z"));
        assert_eq!(closed_len(Timeframe::H1, &b, AssetClass::Equity, refresh), 0);
        b.extend(bars("2026-11-11T15:30:00Z"));
        assert_eq!(closed_len(Timeframe::H1, &b, AssetClass::Equity, refresh), 1);
        assert_eq!(closed_len(Timeframe::H1, &b, AssetClass::Equity, None), 1);
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
            (Timeframe::H1, "2026-11-11T21:00:00Z", "2026-11-11T22:00:00Z"),
            (Timeframe::H4, "2026-11-11T20:00:00Z", "2026-11-12T00:00:00Z"),
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
