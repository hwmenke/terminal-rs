//! Studio view inputs and routes. No market loading or analytical computation.
use serde::{Deserialize, Serialize};
use tr_core::Timeframe;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StudioContext {
    pub symbol: String,
    pub tf: Timeframe,
    pub as_of: Option<i64>,
    /// Half-open UTC canvas selection. Does not silently filter analytical inputs.
    pub selected_span: Option<(i64, i64)>,
}
impl StudioContext {
    pub fn contains(&self, ts: i64) -> bool {
        self.selected_span.is_some_and(|(start, end)| start <= ts && ts < end)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum StudioLens {
    #[default]
    Dist,
    Relate,
    When,
    Season,
    Backtest,
}
impl StudioLens {
    pub const ALL: [Self; 5] = [Self::Dist, Self::Relate, Self::When, Self::Season, Self::Backtest];
    pub fn code(self) -> &'static str {
        match self {
            Self::Dist => "dist",
            Self::Relate => "relate",
            Self::When => "when",
            Self::Season => "season",
            Self::Backtest => "backtest",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Dist => "Distribution",
            Self::Relate => "Relate",
            Self::When => "When",
            Self::Season => "Season",
            Self::Backtest => "Backtest",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.code().eq_ignore_ascii_case(s))
    }
    pub fn select(&mut self, next: Self) -> bool {
        if *self == next {
            false
        } else {
            *self = next;
            true
        }
    }
    pub fn cycle(self, direction: i32) -> Self {
        let at = Self::ALL.iter().position(|l| *l == self).unwrap() as i32;
        Self::ALL[(at + direction).rem_euclid(5) as usize]
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StudioRoute {
    Quick,
    Lab,
    Resid,
    Attr,
}
impl StudioRoute {
    pub fn code(self) -> &'static str {
        match self {
            Self::Quick => "QUICK",
            Self::Lab => "LAB",
            Self::Resid => "RESID",
            Self::Attr => "ATTR",
        }
    }
    pub fn reason(self) -> &'static str {
        match self {
            Self::Quick => {
                "QUICK / Answers is unavailable: the evidence-bearing answer screen has not been implemented."
            }
            Self::Lab => {
                "LAB is unavailable: multi-factor fitting and frozen model persistence have not been implemented."
            }
            Self::Resid => {
                "RESID is unavailable: frozen residual scoring and its signed scalar-series canvas have not been implemented."
            }
            Self::Attr => {
                "ATTR is unavailable: move attribution and actual = intercept + factors + residual reconciliation have not been implemented."
            }
        }
    }
}
/// Parses only the shell's explicitly supported inputs; never accepts a silent fallback.
pub fn parse_spec(spec: &str) -> Result<(StudioLens, Timeframe, String), String> {
    let mut lens = StudioLens::Dist;
    let mut tf = Timeframe::D1;
    let mut benchmark = "SPY".to_string();
    let (mut has_lens, mut has_tf, mut has_benchmark) = (false, false, false);
    for word in spec.split_whitespace() {
        if let Some((key, value)) = word.split_once('=') {
            if key.eq_ignore_ascii_case("lens") && !has_lens {
                lens = StudioLens::parse(value).ok_or("STUDIO lens must be dist, relate, when, season or backtest")?;
                has_lens = true;
            } else if key.eq_ignore_ascii_case("benchmark")
                && !has_benchmark
                && !value.is_empty()
                && value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '^' | '='))
            {
                benchmark = value.to_ascii_uppercase();
                has_benchmark = true;
            } else {
                return Err(format!("Unknown or repeated STUDIO input: {word}"));
            }
        } else if !has_tf {
            tf = word.parse().map_err(|_| format!("Unknown STUDIO input: {word}"))?;
            if tf != Timeframe::D1 && !tf.is_intraday() {
                return Err("STUDIO supports daily or intraday intervals".into());
            }
            has_tf = true;
        } else {
            return Err(format!("Unknown or repeated STUDIO input: {word}"));
        }
    }
    Ok((lens, tf, benchmark))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn studio_selection_is_half_open() {
        let c = StudioContext { selected_span: Some((20, 40)), ..Default::default() };
        assert_eq!([10, 20, 30, 40].into_iter().filter(|t| c.contains(*t)).collect::<Vec<_>>(), [20, 30]);
    }
    #[test]
    fn studio_spec_rejects_unknown_or_repeated_inputs() {
        for spec in ["lens=oops", "5m 1h", "lens=dist lens=season", "1w", "source=sample", "benchmark="] {
            assert!(parse_spec(spec).is_err(), "{spec}");
        }
        assert_eq!(
            parse_spec("5m lens=relate benchmark=qqq").unwrap(),
            (StudioLens::Relate, Timeframe::M5, "QQQ".into())
        );
    }
}
