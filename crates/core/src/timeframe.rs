use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::Bar;

/// Chart intervals from 1 minute to 1 month.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize)]
pub enum Timeframe {
    M1,
    M5,
    M15,
    M30,
    H1,
    H4,
    #[default]
    D1,
    W1,
    MN1,
}

impl Timeframe {
    pub const ALL: [Timeframe; 9] = [
        Timeframe::M1,
        Timeframe::M5,
        Timeframe::M15,
        Timeframe::M30,
        Timeframe::H1,
        Timeframe::H4,
        Timeframe::D1,
        Timeframe::W1,
        Timeframe::MN1,
    ];

    /// Nominal duration in seconds (months count as 30 days).
    pub fn seconds(self) -> i64 {
        match self {
            Timeframe::M1 => 60,
            Timeframe::M5 => 300,
            Timeframe::M15 => 900,
            Timeframe::M30 => 1800,
            Timeframe::H1 => 3600,
            Timeframe::H4 => 14_400,
            Timeframe::D1 => 86_400,
            Timeframe::W1 => 604_800,
            Timeframe::MN1 => 2_592_000,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Timeframe::M1 => "1m",
            Timeframe::M5 => "5m",
            Timeframe::M15 => "15m",
            Timeframe::M30 => "30m",
            Timeframe::H1 => "1H",
            Timeframe::H4 => "4H",
            Timeframe::D1 => "1D",
            Timeframe::W1 => "1W",
            Timeframe::MN1 => "1M",
        }
    }

    pub fn is_intraday(self) -> bool {
        self < Timeframe::D1
    }

    /// Unit name for a single bar of this interval, used in review HUD copy.
    pub fn bar_unit(self) -> &'static str {
        match self {
            Timeframe::D1 => "day",
            Timeframe::W1 => "week",
            Timeframe::MN1 => "month",
            _ => "bar",
        }
    }

    /// Start of the bucket containing `ts` (weeks start Monday, months on the 1st, UTC).
    pub fn bucket_start(self, ts: i64) -> i64 {
        match self {
            Timeframe::W1 => {
                let d = day_of(ts);
                let monday = d - Duration::days(d.weekday().num_days_from_monday() as i64);
                midnight(monday)
            }
            Timeframe::MN1 => {
                let d = day_of(ts);
                midnight(NaiveDate::from_ymd_opt(d.year(), d.month(), 1).expect("valid month start"))
            }
            tf => ts.div_euclid(tf.seconds()) * tf.seconds(),
        }
    }

    /// Aggregate bars of a finer timeframe into this timeframe.
    pub fn resample(self, bars: &[Bar]) -> Vec<Bar> {
        let mut out: Vec<Bar> = Vec::with_capacity(bars.len() / 2 + 1);
        for b in bars {
            let start = self.bucket_start(b.ts);
            match out.last_mut() {
                Some(cur) if cur.ts == start => {
                    cur.high = cur.high.max(b.high);
                    cur.low = cur.low.min(b.low);
                    cur.close = b.close;
                    cur.volume = match (cur.volume_value(), b.volume_value()) {
                        (Some(a), Some(b)) => a + b,
                        (a, b) => a.or(b).unwrap_or(f64::NAN),
                    };
                    cur.provenance = cur.provenance.combine(b.provenance);
                }
                _ => out.push(Bar { ts: start, volume: b.volume_value().unwrap_or(f64::NAN), ..*b }),
            }
        }
        out
    }
}

fn day_of(ts: i64) -> NaiveDate {
    DateTime::<Utc>::from_timestamp(ts, 0).map(|d| d.date_naive()).unwrap_or_default()
}

fn midnight(d: NaiveDate) -> i64 {
    d.and_hms_opt(0, 0, 0).expect("midnight").and_utc().timestamp()
}

impl fmt::Display for Timeframe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for Timeframe {
    type Err = String;

    /// Accepts TradingView/Bloomberg-ish spellings: `1`, `1m`, `5`, `60`, `1h`, `4H`, `D`, `1D`, `W`, `M`, `1MO`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = s.trim();
        // A bare or trailing uppercase `M` means month; lowercase `m` means minute.
        if t == "M" || t == "1M" || t.eq_ignore_ascii_case("1mo") || t.eq_ignore_ascii_case("mo") {
            return Ok(Timeframe::MN1);
        }
        Ok(match t.to_ascii_lowercase().as_str() {
            "1" | "1m" | "1min" => Timeframe::M1,
            "5" | "5m" | "5min" => Timeframe::M5,
            "15" | "15m" | "15min" => Timeframe::M15,
            "30" | "30m" | "30min" => Timeframe::M30,
            "60" | "1h" | "h" => Timeframe::H1,
            "240" | "4h" => Timeframe::H4,
            "d" | "1d" | "day" => Timeframe::D1,
            "w" | "1w" | "week" => Timeframe::W1,
            "month" => Timeframe::MN1,
            _ => return Err(format!("unknown timeframe '{s}'")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_spellings() {
        assert_eq!("1m".parse::<Timeframe>().unwrap(), Timeframe::M1);
        assert_eq!("1M".parse::<Timeframe>().unwrap(), Timeframe::MN1);
        assert_eq!("4H".parse::<Timeframe>().unwrap(), Timeframe::H4);
        assert_eq!("D".parse::<Timeframe>().unwrap(), Timeframe::D1);
        assert_eq!("240".parse::<Timeframe>().unwrap(), Timeframe::H4);
        assert!("7x".parse::<Timeframe>().is_err());
    }

    #[test]
    fn week_and_month_buckets() {
        // 2026-10-07 is a Wednesday.
        let wed = NaiveDate::from_ymd_opt(2026, 10, 7).unwrap().and_hms_opt(15, 30, 0).unwrap().and_utc().timestamp();
        let mon = NaiveDate::from_ymd_opt(2026, 10, 5).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
        let first = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
        assert_eq!(Timeframe::W1.bucket_start(wed), mon);
        assert_eq!(Timeframe::MN1.bucket_start(wed), first);
        assert_eq!(Timeframe::H1.bucket_start(wed), wed - 30 * 60);
    }

    #[test]
    fn resample_sums_only_known_volume() {
        let bars: Vec<_> = [f64::NAN, 3.0, f64::NAN, 0.0, f64::NAN, f64::NAN]
            .into_iter()
            .enumerate()
            .map(|(i, v)| Bar::new(i as i64 * 60, 10.0, 12.0, 9.0, 11.0, v))
            .collect();
        let result = Timeframe::M5.resample(&bars);
        assert_eq!(result[0].volume_value(), Some(3.0));
        assert_eq!(result[1].volume_value(), None);
        let zero = Timeframe::M5.resample(&bars[2..4]);
        assert_eq!(zero[0].volume_value(), Some(0.0));
    }

    #[test]
    fn resample_minutes_to_hour() {
        let bars: Vec<Bar> =
            (0..120).map(|i| Bar::new(i * 60, i as f64, i as f64 + 1.0, i as f64 - 1.0, i as f64 + 0.5, 1.0)).collect();
        let h = Timeframe::H1.resample(&bars);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0], Bar::new(0, 0.0, 60.0, -1.0, 59.5, 60.0));
        assert_eq!(h[1].ts, 3600);
        assert_eq!(h[1].open, 60.0);
    }
}
