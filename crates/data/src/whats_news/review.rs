//! Chart-review context computed from the bars on the screen.
//!
//! Box, strength and volume use the same local rules as SCAN, applied to this
//! chart's interval. They are not SPY-relative, not a published rating, and not
//! a Whats-News HTTP field. Weekly charts therefore show a 20-week box and a
//! 21-week return — never a daily SCAN box mislabelled as weekly.

use super::darvas;
use tr_core::{Bar, Timeframe};

/// Latest box, 21-bar return and volume ratios for one loaded series.
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewContext {
    pub box_top: Option<f64>,
    pub box_bottom: Option<f64>,
    pub box_state: Option<&'static str>,
    pub box_note: &'static str,
    pub return_pct: Option<f64>,
    pub return_label: String,
    pub volume_ratio: Option<f64>,
    pub vol_ratio_5_20: Option<f64>,
    pub volume_note: &'static str,
    pub rsi14: Option<f64>,
}

impl ReviewContext {
    /// Build from the displayed series. Sample or unverified bars are refused.
    pub fn from_bars(bars: &[Bar], tf: Timeframe) -> Result<Self, String> {
        if bars.is_empty() {
            return Err("no bars loaded".into());
        }
        Bar::require_verified(bars, "chart review")?;
        let unit = tf.bar_unit();
        let box_ = darvas(bars);
        let n = bars.len();
        let last = bars[n - 1];
        let return_pct = (n >= 22 && bars[n - 22].close > 0.0)
            .then(|| (last.close / bars[n - 22].close - 1.0) * 100.0)
            .filter(|v| v.is_finite());
        let avg20 = (n >= 21).then(|| bars[n - 21..n - 1].iter().map(|b| b.volume).sum::<f64>() / 20.0);
        let volume_ratio = avg20.filter(|a| *a > 0.0).map(|a| last.volume / a).filter(|v| v.is_finite());
        let current20 = (n >= 20).then(|| bars[n - 20..].iter().map(|b| b.volume).sum::<f64>() / 20.0);
        let vol_ratio_5_20 = match (n >= 20, current20) {
            (true, Some(avg)) if avg > 0.0 => {
                let five = bars[n - 5..].iter().map(|b| b.volume).sum::<f64>() / 5.0;
                Some(five / avg).filter(|v| v.is_finite())
            }
            _ => None,
        };
        Ok(Self {
            box_top: box_.map(|b| b.0),
            box_bottom: box_.map(|b| b.1),
            box_state: box_.map(|b| b.2),
            box_note: if box_.is_some() {
                "20-bar high / 3-bar confirmation on this interval (same rule as SCAN)"
            } else if n < 25 {
                "need 25 bars on this interval for a box"
            } else {
                "no confirmed 20-bar / 3-bar box on this interval"
            },
            return_pct,
            return_label: format!("21-{unit} close return (this series, not SPY-relative)"),
            volume_ratio,
            vol_ratio_5_20,
            volume_note: "this bar / mean of the prior 20; 5/20 uses the latest 5 over the latest 20",
            rsi14: super::scanner::rsi(bars, 14),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tr_core::Provenance;

    fn bar(i: i64, close: f64, high: f64, low: f64, volume: f64, provenance: Provenance) -> Bar {
        Bar::new(i * 86_400, close, high, low, close, volume).with_provenance(provenance)
    }

    fn rising(n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let c = 100.0 + i as f64;
                bar(i as i64, c, c + 1.0, c - 1.0, 1000.0 + i as f64, Provenance::Real)
            })
            .collect()
    }

    #[test]
    fn labels_follow_the_chart_interval_and_refuse_samples() {
        let daily = ReviewContext::from_bars(&rising(30), Timeframe::D1).unwrap();
        assert!(daily.return_label.contains("21-day"));
        assert!(daily.return_label.contains("not SPY-relative"));
        assert!(daily.return_pct.unwrap() > 0.0);
        assert!(daily.volume_ratio.is_some());
        // A steadily rising series never holds a 20-bar high for 3 bars.
        assert_eq!(daily.box_note, "no confirmed 20-bar / 3-bar box on this interval");
        let boxed: Vec<Bar> = (0..30).map(|i| bar(i as i64, 9.0, 10.0, 8.0, 100.0, Provenance::Real)).collect();
        let boxed = ReviewContext::from_bars(&boxed, Timeframe::D1).unwrap();
        assert_eq!(boxed.box_state, Some("DARVAS_BOX"));
        assert_eq!(boxed.box_note, "20-bar high / 3-bar confirmation on this interval (same rule as SCAN)");
        let weekly = ReviewContext::from_bars(&rising(30), Timeframe::W1).unwrap();
        assert!(weekly.return_label.contains("21-week"));
        let sample: Vec<Bar> = rising(30)
            .into_iter()
            .map(|b| Bar::new(b.ts, b.open, b.high, b.low, b.close, b.volume).with_provenance(Provenance::Sample))
            .collect();
        assert!(ReviewContext::from_bars(&sample, Timeframe::D1).unwrap_err().contains("SAMPLE"));
    }

    #[test]
    fn short_history_is_honest_rather_than_padded() {
        let ctx = ReviewContext::from_bars(&rising(10), Timeframe::D1).unwrap();
        assert!(ctx.box_top.is_none());
        assert_eq!(ctx.box_note, "need 25 bars on this interval for a box");
        assert!(ctx.return_pct.is_none());
        assert!(ctx.volume_ratio.is_none());
    }
}
