//! US Eastern civil time (`America/New_York`).
//!
//! DST follows the rules in force since 2007: second Sunday in March at 07:00 UTC
//! through the first Sunday in November at 06:00 UTC. The offset is a whole number of
//! hours, so minute bars that divide an hour land on the same instants as a UTC clock.
//! This module does not decide whether the cash session is open.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};

/// `n`th Sunday of `month` (1 = first).
pub fn nth_sunday(year: i32, month: u32, n: u32) -> NaiveDate {
    let first = NaiveDate::from_ymd_opt(year, month, 1).expect("month");
    let lead = (7 - first.weekday().num_days_from_sunday()) % 7;
    first + Duration::days((lead + 7 * (n - 1)) as i64)
}

/// Hours behind UTC at this instant. 4 during EDT, 5 during EST.
pub fn offset_hours_at(ts: i64) -> i64 {
    let utc = DateTime::<Utc>::from_timestamp(ts, 0).unwrap_or_default();
    let start = nth_sunday(utc.year(), 3, 2).and_hms_opt(7, 0, 0).unwrap().and_utc();
    let end = nth_sunday(utc.year(), 11, 1).and_hms_opt(6, 0, 0).unwrap().and_utc();
    if utc >= start && utc < end { 4 } else { 5 }
}

/// Hours behind UTC for a New York civil date outside the ambiguous fallback hour.
/// Weekdays are unambiguous: the transition itself falls on Sunday.
pub fn offset_hours_on(date: NaiveDate) -> i64 {
    let start = nth_sunday(date.year(), 3, 2);
    let end = nth_sunday(date.year(), 11, 1);
    if date > start && date < end { 4 } else { 5 }
}

/// Wall-clock New York time for a UTC instant.
pub fn civil(ts: i64) -> NaiveDateTime {
    let utc = DateTime::<Utc>::from_timestamp(ts, 0).unwrap_or_default().naive_utc();
    utc - Duration::hours(offset_hours_at(ts))
}

/// UTC instant for a New York civil time. The fallback hour (01:00–02:00 on the
/// first Sunday in November) is outside the cash session and is treated as EST.
pub fn to_utc(wall: NaiveDateTime) -> i64 {
    let utc = wall + Duration::hours(offset_hours_on(wall.date()));
    Utc.from_utc_datetime(&utc).timestamp()
}

/// `yyyymmdd` in New York, for session resets that must not follow UTC midnight.
pub fn date_key(ts: i64) -> i64 {
    let d = civil(ts).date();
    d.year() as i64 * 10_000 + d.month() as i64 * 100 + d.day() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
        NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, min, 0).unwrap().and_utc().timestamp()
    }

    #[test]
    fn dst_and_standard_offsets_round_trip_the_cash_open() {
        // 2026-03-09 is the first session after the second Sunday in March.
        let edt = utc(2026, 3, 9, 13, 30);
        assert_eq!(offset_hours_at(edt), 4);
        let edt_wall = civil(edt);
        assert_eq!((edt_wall.hour(), edt_wall.minute()), (9, 30));
        assert_eq!(edt_wall.date().to_string(), "2026-03-09");
        assert_eq!(to_utc(edt_wall), edt);
        // 2026-11-02 is the first session after clocks fall back.
        let est = utc(2026, 11, 2, 14, 30);
        assert_eq!(offset_hours_at(est), 5);
        let est_wall = civil(est);
        assert_eq!((est_wall.hour(), est_wall.minute()), (9, 30));
        assert_eq!(est_wall.date().to_string(), "2026-11-02");
        assert_eq!(to_utc(est_wall), est);
        // UTC midnight is still the previous evening in New York.
        let late = utc(2026, 11, 2, 3, 30);
        let late_wall = civil(late);
        assert_eq!(late_wall.date().to_string(), "2026-11-01");
        assert_eq!((late_wall.hour(), late_wall.minute()), (22, 30));
        assert_ne!(date_key(late), date_key(est));
        assert_eq!(date_key(edt), date_key(utc(2026, 3, 9, 19, 0)));
    }
}
