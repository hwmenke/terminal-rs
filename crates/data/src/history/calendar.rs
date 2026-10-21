//! US equity session calendar and completed-session cutoff in New York.
use chrono::{DateTime, Datelike, Duration, NaiveDate, Timelike, Utc, Weekday};

pub fn is_session(d: NaiveDate) -> bool {
    let (y, m, day) = (d.year(), d.month(), d.day());
    if matches!(d.weekday(), Weekday::Sat | Weekday::Sun) {
        return false;
    }
    let observed = |m, day| {
        let h = NaiveDate::from_ymd_opt(y, m, day).unwrap();
        match h.weekday() {
            Weekday::Sat => h.pred_opt().unwrap(),
            Weekday::Sun => h.succ_opt().unwrap(),
            _ => h,
        }
    };
    // NYSE does not observe a Saturday New Year's Day on the preceding Friday.
    let new_year = NaiveDate::from_ymd_opt(y, 1, 1).unwrap();
    if d == new_year
        || (new_year.weekday() == Weekday::Sun && m == 1 && day == 2)
        || d == observed(7, 4)
        || d == observed(12, 25)
        || (y >= 2022 && d == observed(6, 19))
    {
        return false;
    }
    if (m == 1 && d.weekday() == Weekday::Mon && (15..=21).contains(&day))
        || (m == 2 && d.weekday() == Weekday::Mon && (15..=21).contains(&day))
        || (m == 5 && d.weekday() == Weekday::Mon && day + 7 > 31)
        || (m == 9 && d.weekday() == Weekday::Mon && day <= 7)
        || (m == 11 && d.weekday() == Weekday::Thu && (22..=28).contains(&day))
    {
        return false;
    }
    if matches!(
        d.to_string().as_str(),
        "2001-09-11"
            | "2001-09-12"
            | "2001-09-13"
            | "2001-09-14"
            | "2004-06-11"
            | "2007-01-02"
            | "2012-10-29"
            | "2012-10-30"
            | "2018-12-05"
            | "2025-01-09"
    ) {
        return false;
    }
    // Gregorian Easter (Meeus), then Good Friday.
    let a = y % 19;
    let b = y / 100;
    let c = y % 100;
    let h = (19 * a + b - b / 4 - (b - (b + 8) / 25 + 1) / 3 + 15) % 30;
    let l = (32 + 2 * (b % 4) + 2 * (c / 4) - h - c % 4) % 7;
    let k = (a + 11 * h + 22 * l) / 451;
    let easter =
        NaiveDate::from_ymd_opt(y, ((h + l - 7 * k + 114) / 31) as u32, ((h + l - 7 * k + 114) % 31 + 1) as u32)
            .unwrap();
    d != easter - chrono::Duration::days(2)
}

/// Last completed session. Allow 15 minutes after close for Yahoo's daily bar.
/// NY DST rules since 2007; archive freshness concerns the current date.
pub fn last_expected_session(now: DateTime<Utc>) -> NaiveDate {
    let y = now.year();
    let nth_sunday = |month: u32, n: u32| {
        let first = NaiveDate::from_ymd_opt(y, month, 1).unwrap();
        first + Duration::days(((7 - first.weekday().num_days_from_sunday()) % 7 + 7 * (n - 1)) as i64)
    };
    let dst_start = nth_sunday(3, 2).and_hms_opt(7, 0, 0).unwrap().and_utc();
    let dst_end = nth_sunday(11, 1).and_hms_opt(6, 0, 0).unwrap().and_utc();
    let ny = now - Duration::hours(if now >= dst_start && now < dst_end { 4 } else { 5 });
    let d = ny.date_naive();
    let early = (d.month() == 11 && d.weekday() == Weekday::Fri && (23..=29).contains(&d.day()))
        || (d.month() == 12 && d.day() == 24)
        || (d.month() == 7 && d.day() == 3);
    let close = if early { 13 } else { 16 };
    if is_session(d) && (ny.hour() * 60 + ny.minute()) >= close * 60 + 15 { d } else { previous_session(d) }
}
pub fn previous_session(date: NaiveDate) -> NaiveDate {
    let mut d = date.pred_opt().unwrap();
    while !is_session(d) {
        d = d.pred_opt().unwrap();
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    fn expected(now: &str, day: &str) {
        assert_eq!(last_expected_session(now.parse().unwrap()).to_string(), day);
    }
    #[test]
    fn weekends_holidays_dst_and_close() {
        expected("2026-07-06T19:59:00Z", "2026-07-02"); // observed Independence Day
        expected("2026-07-06T20:16:00Z", "2026-07-06");
        expected("2026-04-05T20:30:00Z", "2026-04-02"); // Good Friday + Easter weekend
        expected("2026-01-19T22:00:00Z", "2026-01-16"); // MLK
        expected("2026-06-19T22:00:00Z", "2026-06-18"); // Juneteenth
        expected("2026-12-25T22:00:00Z", "2026-12-24");
        expected("2026-11-27T18:16:00Z", "2026-11-27"); // early close
        expected("2026-11-27T18:00:00Z", "2026-11-25");
        expected("2026-03-09T20:16:00Z", "2026-03-09"); // DST
        expected("2026-11-02T20:30:00Z", "2026-10-30"); // EST: market still open
        expected("2027-01-01T22:00:00Z", "2026-12-31");
        expected("2025-01-09T22:00:00Z", "2025-01-08"); // Carter closure
        assert!(is_session(NaiveDate::from_ymd_opt(2021, 12, 31).unwrap()));
    }
}
