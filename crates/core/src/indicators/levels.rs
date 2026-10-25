//! Price levels: session keys and pivot points.

use chrono::Datelike;

use crate::Timeframe;

use super::{NAN, Ohlcv};

/// The session a bar at `ts` belongs to on `tf`, for VWAP and pivots: the UTC day
/// intraday, the month on daily bars, the year above that.
pub fn session_key(tf: Timeframe) -> impl Fn(i64) -> i64 {
    move |ts| {
        let d = chrono::DateTime::from_timestamp(ts, 0).unwrap_or_default();
        match tf {
            t if t.is_intraday() => ts.div_euclid(86_400),
            Timeframe::D1 => (d.year() * 12 + d.month() as i32) as i64,
            _ => d.year() as i64,
        }
    }
}

/// Honest VWAP label for the session [`session_key`] actually uses on `tf`.
/// Daily bars reset each calendar month, not each session; weekly and monthly
/// bars reset each calendar year. This is not a session-anchored daily VWAP.
pub fn vwap_anchor_label(tf: Timeframe) -> &'static str {
    match tf {
        t if t.is_intraday() => "Session VWAP",
        Timeframe::D1 => "Monthly VWAP",
        _ => "Yearly VWAP",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PivotKind {
    Classic,
    Fibonacci,
    Camarilla,
    Woodie,
    Demark,
}

/// Open, high, low and close of one session.
#[derive(Debug, Clone, Copy)]
struct Session {
    open: f64,
    high: f64,
    low: f64,
    close: f64,
}

impl PivotKind {
    /// Level names, in the order [`pivots`] returns them.
    pub fn names(self) -> &'static [&'static str] {
        match self {
            PivotKind::Classic | PivotKind::Fibonacci | PivotKind::Woodie => &["P", "R1", "S1", "R2", "S2", "R3", "S3"],
            PivotKind::Camarilla => &["P", "R1", "S1", "R2", "S2", "R3", "S3", "R4", "S4"],
            PivotKind::Demark => &["P", "R1", "S1"],
        }
    }

    /// Levels from the previous session `s`, given the current session's `open`.
    fn levels(self, s: Session, open: f64) -> Vec<f64> {
        let Session { open: o, high: h, low: l, close: c } = s;
        let range = h - l;
        let classic =
            |p: f64| vec![p, 2.0 * p - l, 2.0 * p - h, p + range, p - range, h + 2.0 * (p - l), l - 2.0 * (h - p)];
        match self {
            PivotKind::Classic => classic((h + l + c) / 3.0),
            PivotKind::Woodie => classic((h + l + 2.0 * open) / 4.0),
            PivotKind::Fibonacci => {
                let p = (h + l + c) / 3.0;
                vec![
                    p,
                    p + 0.382 * range,
                    p - 0.382 * range,
                    p + 0.618 * range,
                    p - 0.618 * range,
                    p + range,
                    p - range,
                ]
            }
            PivotKind::Camarilla => {
                let p = (h + l + c) / 3.0;
                let k = 1.1 * range;
                vec![
                    p,
                    c + k / 12.0,
                    c - k / 12.0,
                    c + k / 6.0,
                    c - k / 6.0,
                    c + k / 4.0,
                    c - k / 4.0,
                    c + k / 2.0,
                    c - k / 2.0,
                ]
            }
            PivotKind::Demark => {
                let x = if c < o {
                    h + 2.0 * l + c
                } else if c > o {
                    2.0 * h + l + c
                } else {
                    h + l + 2.0 * c
                };
                vec![x / 4.0, x / 2.0 - l, x / 2.0 - h]
            }
        }
    }
}

/// Pivot levels for each bar from the previous session's prices (`session_of` maps a
/// timestamp to its session); the first session has none.
pub fn pivots(o: &Ohlcv, session_of: impl Fn(i64) -> i64, kind: PivotKind) -> Vec<(&'static str, Vec<f64>)> {
    let names = kind.names();
    let len = o.len();
    let mut out = vec![vec![NAN; len]; names.len()];
    let keys: Vec<i64> = o.ts.iter().map(|&t| session_of(t)).collect();
    let mut cur: Option<Session> = None;
    let mut levels: Option<Vec<f64>> = None;
    for i in 0..len {
        let bar = Session { open: o.open[i], high: o.high[i], low: o.low[i], close: o.close[i] };
        match cur.as_mut() {
            Some(s) if keys[i] == keys[i - 1] => {
                s.high = s.high.max(bar.high);
                s.low = s.low.min(bar.low);
                s.close = bar.close;
            }
            prev => {
                levels = prev.map(|p| kind.levels(*p, bar.open));
                cur = Some(bar);
            }
        }
        if let Some(l) = &levels {
            for (series, v) in out.iter_mut().zip(l) {
                series[i] = *v;
            }
        }
    }
    names.iter().copied().zip(out).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Bar;

    #[test]
    fn session_keys() {
        let day = 86_400;
        let k = session_key(Timeframe::M5);
        assert_eq!(k(10), k(day - 1));
        assert_ne!(k(day - 1), k(day));
        // 2026-01-31 and 2026-02-01 fall in different months on daily bars.
        let m = session_key(Timeframe::D1);
        assert_ne!(m(1_769_817_600), m(1_769_904_000));
        assert_eq!(session_key(Timeframe::W1)(1_769_817_600), 2026);
        assert_eq!(vwap_anchor_label(Timeframe::M5), "Session VWAP");
        assert_eq!(vwap_anchor_label(Timeframe::D1), "Monthly VWAP");
        assert_eq!(vwap_anchor_label(Timeframe::W1), "Yearly VWAP");
        assert_eq!(vwap_anchor_label(Timeframe::MN1), "Yearly VWAP");
    }

    #[test]
    fn classic_pivots_from_the_previous_session() {
        let day = 86_400;
        let bars = [
            Bar::new(0, 10.0, 12.0, 8.0, 11.0, 1.0),
            Bar::new(60, 11.0, 15.0, 9.0, 13.0, 1.0),
            Bar::new(day, 13.0, 14.0, 12.0, 13.5, 1.0),
            Bar::new(day + 60, 13.5, 14.5, 12.5, 14.0, 1.0),
            Bar::new(2 * day, 14.0, 15.0, 13.0, 14.0, 1.0),
        ];
        let o = Ohlcv::new(&bars);
        let p = pivots(&o, |ts| ts.div_euclid(day), PivotKind::Classic);
        let names: Vec<&str> = p.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["P", "R1", "S1", "R2", "S2", "R3", "S3"]);
        // Day 1: H 15, L 8, C 13 → P = 12, R1 = 16, S1 = 9.
        let (pp, r1, s1) = (&p[0].1, &p[1].1, &p[2].1);
        assert!(pp[0].is_nan() && pp[1].is_nan());
        assert_eq!((pp[2], r1[2], s1[2]), (12.0, 16.0, 9.0));
        // Every bar of day 2 carries its levels; day 3 uses H 14.5, L 12, C 14.
        assert_eq!((pp[3], r1[3], s1[3]), (12.0, 16.0, 9.0));
        assert!((pp[4] - 40.5 / 3.0).abs() < 1e-12);
        let dm = pivots(&o, |ts| ts.div_euclid(day), PivotKind::Demark);
        // Day 1 closed above its open: X = 2H + L + C = 51.
        assert_eq!((dm[0].1[2], dm[1].1[2], dm[2].1[2]), (12.75, 17.5, 10.5));
        let w = pivots(&o, |ts| ts.div_euclid(day), PivotKind::Woodie);
        assert_eq!(w[0].1[2], (15.0 + 8.0 + 2.0 * 13.0) / 4.0);
        assert_eq!(pivots(&o, |ts| ts.div_euclid(day), PivotKind::Camarilla).len(), 9);
    }
}
