use serde::{Deserialize, Serialize};

/// Origin of prices. Missing metadata is unverified, never implicitly real.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provenance {
    #[serde(alias = "Live", alias = "real", alias = "live")]
    Real,
    #[serde(alias = "cached")]
    Cached,
    #[serde(alias = "sample")]
    Sample,
    #[default]
    #[serde(alias = "missing", alias = "unverified", other)]
    Missing,
}

impl Provenance {
    pub fn label(self) -> &'static str {
        match self {
            Self::Real => "REAL",
            Self::Cached => "CACHED",
            Self::Missing => "UNVERIFIED / MISSING",
            Self::Sample => "WARNING: SAMPLE DATA",
        }
    }

    /// Only previously verified real prices may be tagged Cached.
    pub fn is_verified(self) -> bool {
        matches!(self, Self::Real | Self::Cached)
    }

    /// Never attribute artificial or unverified prices to a real source.
    pub fn source_label<'a>(self, source: &'a str) -> &'a str {
        match self {
            Self::Sample => "SAMPLE DATA",
            Self::Missing => "UNVERIFIED DATA",
            _ => source,
        }
    }

    /// Conservative aggregation: one artificial/unverified input taints the output.
    pub fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::Sample, _) | (_, Self::Sample) => Self::Sample,
            (Self::Missing, _) | (_, Self::Missing) => Self::Missing,
            (Self::Cached, _) | (_, Self::Cached) => Self::Cached,
            _ => Self::Real,
        }
    }

    pub fn require_verified(self, operation: &str) -> Result<(), String> {
        if self.is_verified() {
            Ok(())
        } else {
            Err(format!("{operation} refused: {}. Verified real data is required.", self.label()))
        }
    }
}
