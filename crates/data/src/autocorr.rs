//! Signed return autocorrelation: net log movement / total absolute log movement.
use crate::{cash_session, intraday::regular_opens, security};
use chrono::NaiveDate;
use tr_core::{Bar, Provenance, Timeframe};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutocorrQuery {
    pub window: usize,
    pub timeframe: Timeframe,
}
#[derive(Debug, Clone)]
pub struct AutocorrPoint {
    pub ts: i64,
    pub cumulative: Option<f64>,
    pub rolling: Option<f64>,
}
#[derive(Debug, Clone)]
pub struct Autocorr {
    pub date: NaiveDate,
    pub timeframe: Timeframe,
    pub provenance: Provenance,
    pub source: String,
    pub window: usize,
    pub gaps: usize,
    pub points: Vec<AutocorrPoint>,
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
pub fn parse_query(spec: &str, tf_field: &str) -> Result<AutocorrQuery, String> {
    let words = cash_session::BarWords { noun: "autocorr", place: "return autocorrelation", kind: "autocorr" };
    let q = cash_session::parse_window(spec, tf_field, words, 3, 120, 20).map_err(return_count_error)?;
    Ok(AutocorrQuery { window: q.window, timeframe: q.timeframe })
}
pub fn build_autocorr(symbol: &str, bars: &[Bar], query: &AutocorrQuery, source: &str) -> Result<Autocorr, String> {
    cash_session::require_bar_size(query.timeframe, "return autocorrelation")?;
    cash_session::require_window(query.window, 3, 120).map_err(return_count_error)?;
    let sec = security(symbol);
    if symbol.trim().is_empty() || !tr_core::security::looks_like_ticker(&sec.ticker) {
        return Err("ACOR needs a ticker (AAPL ACOR).".into());
    }
    if !cash_session::is_cash(&sec) {
        return Err(format!("{} has no US cash session.", sec.ticker));
    }
    Bar::require_verified(bars, "Return autocorrelation")?;
    let days = cash_session::sessions(query.timeframe, bars);
    let (&date, day) = days.iter().next_back().ok_or("No regular-session bars for ACOR.")?;
    let last = *day.keys().next_back().unwrap();
    let mut previous = None;
    let mut session_returns = Vec::new();
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
                session_returns.push(r);
                returns.push_back(r);
                if returns.len() > query.window {
                    returns.pop_front();
                }
                if intact {
                    cumulative = serial_correlation(&session_returns);
                }
                if returns.len() == query.window {
                    rolling = serial_correlation(returns.make_contiguous());
                }
            }
            previous = Some(c);
        } else {
            gaps += 1;
            intact = false;
            previous = None;
            returns.clear();
        }
        points.push(AutocorrPoint { ts, cumulative, rolling });
    }
    Ok(Autocorr {
        date,
        timeframe: query.timeframe,
        provenance: Bar::provenance(bars),
        source: source.into(),
        window: query.window,
        gaps,
        points,
    })
}

/// Correlate r[0..n-1] with r[1..n]. Constant or short sequences are undefined.
fn serial_correlation(returns: &[f64]) -> Option<f64> {
    if returns.len() < 3 {
        return None;
    }
    let n = (returns.len() - 1) as f64;
    let x = &returns[..returns.len() - 1];
    let y = &returns[1..];
    let mx = x.iter().sum::<f64>() / n;
    let my = y.iter().sum::<f64>() / n;
    let mut covariance = 0.0;
    let mut vx = 0.0;
    let mut vy = 0.0;
    for (&a, &b) in x.iter().zip(y) {
        covariance += (a - mx) * (b - my);
        vx += (a - mx).powi(2);
        vy += (b - my).powi(2);
    }
    let denominator = vx.sqrt() * vy.sqrt();
    if denominator > 0.0 { Some((covariance / denominator).clamp(-1.0, 1.0)) } else { None }
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
                let c = 100.0 * (0.001 * (i * i) as f64).exp();
                Bar::new(ts, c, c, c, c, 10.0).with_provenance(Provenance::Real)
            })
            .collect()
    }
    fn build(b: &[Bar]) -> Autocorr {
        build_autocorr("AAPL", b, &AutocorrQuery { window: 3, timeframe: Timeframe::M5 }, "unit").unwrap()
    }
    #[test]
    fn known_serial_correlations() {
        assert_eq!(serial_correlation(&[1.0, 2.0]), None);
        assert_eq!(serial_correlation(&[0.0; 5]), None);
        assert_eq!(serial_correlation(&[1.0; 5]), None);
        assert!((serial_correlation(&[1.0, 2.0, 3.0, 4.0]).unwrap() - 1.0).abs() < 1e-12);
        assert!((serial_correlation(&[1.0, -1.0, 1.0, -1.0]).unwrap() + 1.0).abs() < 1e-12);
        let mut b = bars();
        let mut log_price = 100.0_f64.ln();
        for (bar, r) in b.iter_mut().zip([0.0, 0.01, -0.01, 0.01, -0.01, 0.01]) {
            log_price += r;
            bar.close = log_price.exp();
        }
        let result = build(&b);
        assert!(result.points[2].rolling.is_none());
        assert!((result.points[3].rolling.unwrap() + 1.0).abs() < 1e-10);
        assert!((result.points[5].cumulative.unwrap() + 1.0).abs() < 1e-10);
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
            assert!(v.points[5].rolling.is_some());
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
            assert!(build_autocorr("AAPL", &b, &parse_query("3", "5m").unwrap(), "unit").is_err());
        }
        assert_eq!(parse_query("10 15m", "5m").unwrap().timeframe, Timeframe::M15);
        for spec in ["1", "2", "121", "3 4", "5m 15m", "nonsense", "1D"] {
            assert!(parse_query(spec, "").is_err(), "{spec}");
        }
        assert!(build_autocorr("BTCUSD", &bars(), &parse_query("", "").unwrap(), "unit").is_err());
        assert!(build_autocorr("AAPL", &[], &parse_query("", "").unwrap(), "unit").is_err());
    }
}
