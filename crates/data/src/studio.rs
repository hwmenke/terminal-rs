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
                "Regression Lab · multi-factor OLS with a purged holdout and HAC inference · STUDIO lab factors=SPY,TLT (Save residual freezes a fit for RESID)"
            }
            Self::Resid => {
                "Residual tickers · frozen fit scored on live data, never refit · STUDIO resid open <id>"
            }
            Self::Attr => {
                "Attribution · actual = drift + factor contributions + residual from a saved frozen model, never refit · STUDIO attr model=<id> obs=20"
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

/// Shared signed analytical values. These never masquerade as positive-price bars.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Transform {
    Level,
    Change { h: usize },
    LogReturn { h: usize },
    LogLevel,
}
impl Transform {
    /// Return horizon in bars (1 for the level transforms).
    pub fn horizon(self) -> usize {
        match self {
            Self::Change { h } | Self::LogReturn { h } => h,
            Self::Level | Self::LogLevel => 1,
        }
    }
    /// Replaces an unsupported `h` (hand-edited layout) with 1; supported values are 1, 5, 20.
    pub fn with_valid_horizon(self) -> Self {
        let ok = |h: usize| if [1, 5, 20].contains(&h) { h } else { 1 };
        match self {
            Self::Change { h } => Self::Change { h: ok(h) },
            Self::LogReturn { h } => Self::LogReturn { h: ok(h) },
            other => other,
        }
    }
    pub fn is_level(self) -> bool {
        matches!(self, Self::Level | Self::LogLevel)
    }
    pub fn short(self) -> String {
        match self {
            Self::Level => "level".into(),
            Self::LogLevel => "loglevel".into(),
            Self::Change { h } => format!("change h={h}"),
            Self::LogReturn { h } => format!("logret h={h}"),
        }
    }
    /// Parses `level|change|logreturn|loglevel`; `h` must be 1, 5 or 20 for the h transforms.
    pub fn parse(name: &str, h: usize) -> Result<Self, String> {
        if ![1, 5, 20].contains(&h) {
            // Caller-neutral; Relate and the Lab prefix their own name.
            return Err("h must be 1, 5 or 20".into());
        }
        match name.to_ascii_lowercase().as_str() {
            "level" => Ok(Self::Level),
            "loglevel" => Ok(Self::LogLevel),
            "change" => Ok(Self::Change { h }),
            "logreturn" | "logret" => Ok(Self::LogReturn { h }),
            _ => Err("transform must be level, change, logreturn or loglevel".into()),
        }
    }
}
/// Rolling β is the legacy windowed fit; expanding β is descriptive only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum BetaMode {
    #[default]
    Rolling,
    Expanding,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum MatrixMode {
    #[default]
    Pairwise,
    Common,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SeriesUnit {
    Native(String),
    Index100,
    Z,
    BpPerBar,
    /// Residual in units of the *saved training* σ of a frozen fit (RESID).
    ZFrozen,
}
impl SeriesUnit {
    pub fn label(&self) -> &str {
        match self {
            Self::Native(s) => s,
            Self::Index100 => "index (base=100)",
            Self::Z => "z (prior sample σ)",
            Self::ZFrozen => "z (saved training σ)",
            Self::BpPerBar => "bp/bar",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeriesReceipt {
    pub source: String,
    pub provenance: tr_core::Provenance,
    pub observed_at: Option<i64>,
    pub closed_thru: i64,
    pub input_digest: u64,
    pub gaps: Vec<(i64, i64)>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScalarSeries {
    pub ts: Vec<i64>,
    pub values: Vec<Option<f64>>,
    pub break_before: Vec<bool>,
    pub unit: SeriesUnit,
    pub receipt: SeriesReceipt,
}
