//! Observed, unannualized intraday quadratic variation; never mixes sessions.
use crate::{cash_session, intraday::regular_opens, security};
use chrono::NaiveDate;
use tr_core::{Bar, Provenance, Timeframe};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolQuery {
    pub window: usize,
    pub timeframe: Timeframe,
}
#[derive(Debug, Clone)]
pub struct VolPoint {
    pub ts: i64,
    pub cumulative: Option<f64>,
    pub rolling: Option<f64>,
}
#[derive(Debug, Clone)]
pub struct RealizedVol {
    pub date: NaiveDate,
    pub timeframe: Timeframe,
    pub provenance: Provenance,
    pub source: String,
    pub window: usize,
    pub gaps: usize,
    pub points: Vec<VolPoint>,
}
pub fn fetch_limit(tf: Timeframe) -> usize {
    cash_session::research_fetch_limit(tf)
}
pub fn clock_label(ts: i64) -> String {
    cash_session::clock_label(ts)
}
/// Bare integers count returns, not sessions; a letter bar token overrides the field.
fn return_count_error(error: String) -> String {
    error
        .replace("prior sessions", "returns")
        .replace("session counts", "return counts")
        .replace("session count", "return count")
}
pub fn parse_query(spec: &str, tf_field: &str) -> Result<VolQuery, String> {
    let words = cash_session::BarWords { noun: "volatility", place: "realized volatility", kind: "volatility" };
    let q = cash_session::parse_window(spec, tf_field, words, 2, 120, 20).map_err(return_count_error)?;
    Ok(VolQuery { window: q.window, timeframe: q.timeframe })
}
pub fn build_vol(symbol: &str, bars: &[Bar], query: &VolQuery, source: &str) -> Result<RealizedVol, String> {
    cash_session::require_bar_size(query.timeframe, "realized volatility")?;
    cash_session::require_window(query.window, 2, 120).map_err(return_count_error)?;
    let sec = security(symbol);
    if symbol.trim().is_empty() || !tr_core::security::looks_like_ticker(&sec.ticker) {
        return Err("RVAR needs a ticker (AAPL RVAR).".into());
    }
    if !cash_session::is_cash(&sec) {
        return Err(format!("{} has no US cash session.", sec.ticker));
    }
    Bar::require_verified(bars, "Realized volatility")?;
    let days = cash_session::sessions(query.timeframe, bars);
    let (&date, day) = days.iter().next_back().ok_or("No regular-session bars for RVAR.")?;
    let last = *day.keys().next_back().unwrap();
    let mut previous = None;
    let mut sum = 0.0;
    let mut intact = true;
    let mut returns = std::collections::VecDeque::new();
    let mut points = Vec::new();
    let mut gaps = 0;
    for ts in regular_opens(date, query.timeframe).into_iter().take_while(|ts| *ts <= last) {
        let close = day.get(&ts).filter(|b| b.len() == 1).map(|b| b[0].close).filter(|c| c.is_finite() && *c > 0.0);
        let mut cumulative = None;
        let mut rolling = None;
        if let Some(c) = close {
            if let Some(p) = previous {
                let r = c.ln() - f64::ln(p);
                let square = r * r;
                sum += square;
                returns.push_back(square);
                if returns.len() > query.window {
                    returns.pop_front();
                }
                if intact {
                    cumulative = Some(100.0 * sum.sqrt());
                }
                if returns.len() == query.window {
                    rolling = Some(100.0 * returns.iter().sum::<f64>().sqrt());
                }
            }
            previous = Some(c);
        } else {
            gaps += 1;
            intact = false;
            previous = None;
            returns.clear();
        }
        points.push(VolPoint { ts, cumulative, rolling });
    }
    Ok(RealizedVol {
        date,
        timeframe: query.timeframe,
        provenance: Bar::provenance(bars),
        source: source.into(),
        window: query.window,
        gaps,
        points,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bars() -> Vec<Bar> {
        let date = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
        regular_opens(date, Timeframe::M5)
            .into_iter()
            .take(6)
            .enumerate()
            .map(|(i, ts)| {
                let c = 100.0 * (0.01 * i as f64).exp();
                Bar::new(ts, c, c, c, c, 10.0).with_provenance(Provenance::Real)
            })
            .collect()
    }
    fn build(b: &[Bar]) -> RealizedVol {
        build_vol("AAPL", b, &VolQuery { window: 2, timeframe: Timeframe::M5 }, "unit").unwrap()
    }
    #[test]
    fn known_returns_and_flat_prices() {
        let b = bars();
        let v = build(&b);
        assert!(v.points[0].cumulative.is_none());
        assert!(v.points[1].rolling.is_none());
        assert!((v.points[5].cumulative.unwrap() - 5.0_f64.sqrt()).abs() < 1e-10);
        assert!((v.points[2].rolling.unwrap() - 2.0_f64.sqrt()).abs() < 1e-10);
        let flat: Vec<_> = b
            .into_iter()
            .map(|mut b| {
                b.close = 100.0;
                b
            })
            .collect();
        assert_eq!(build(&flat).points[5].cumulative, Some(0.0));
    }
    #[test]
    fn gaps_duplicates_and_bad_closes_break_returns() {
        for mode in 0..4 {
            let mut b = bars();
            match mode {
                0 => {
                    b.remove(1);
                }
                1 => b.push(b[1]),
                2 => b[1].close = f64::NAN,
                _ => b[1].close = 0.0,
            }
            let v = build(&b);
            assert_eq!(v.gaps, 1);
            assert!(v.points.iter().all(|p| p.cumulative.is_none()));
            assert!(v.points[3].rolling.is_none());
            assert!(v.points[4].rolling.is_some());
        }
    }
    #[test]
    fn latest_session_only_and_order_independent() {
        let mut b = bars();
        let expected = build(&b).points[5].cumulative;
        let mut prior = b[0];
        prior.ts -= 3 * 86400;
        prior.close = 900.0;
        b.push(prior);
        let mut pre = b[0];
        pre.ts -= 3600;
        pre.close = 10000.0;
        b.push(pre);
        b.reverse();
        assert_eq!(build(&b).points[5].cumulative, expected);
        let date = NaiveDate::from_ymd_opt(2026, 1, 6).unwrap();
        b.push(
            Bar::new(regular_opens(date, Timeframe::M5)[0], 50.0, 50.0, 50.0, 50.0, 1.0)
                .with_provenance(Provenance::Cached),
        );
        let v = build(&b);
        assert_eq!(v.date, date);
        assert_eq!(v.points.len(), 1);
        assert!(v.points[0].cumulative.is_none());
    }
    #[test]
    fn rejects_unverified_and_invalid_queries() {
        for provenance in [Provenance::Sample, Provenance::Missing] {
            let mut b = bars();
            b[0].provenance = provenance;
            assert!(build_vol("AAPL", &b, &parse_query("2", "5m").unwrap(), "unit").is_err());
        }
        assert_eq!(parse_query("10 15m", "5m").unwrap().timeframe, Timeframe::M15);
        for spec in ["1", "121", "2 3", "5m 15m", "nonsense", "1D"] {
            assert!(parse_query(spec, "").is_err(), "{spec}");
        }
        assert!(build_vol("BTCUSD", &bars(), &parse_query("", "").unwrap(), "unit").is_err());
        assert!(build_vol("AAPL", &[], &parse_query("", "").unwrap(), "unit").is_err());
    }
}
