//! Story complexity measures reasoning demands independently of priority.
use serde::{Deserialize, Serialize};

use crate::error::AppError;

/// The three complexity levels accepted by every story mutation surface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Complexity {
    /// Local work with a known approach.
    Low,
    /// Bounded work across components; also the unassessed fallback.
    #[default]
    Medium,
    /// Uncertain work or interacting invariants.
    High,
}

impl Complexity {
    /// All levels, in ascending order.
    pub const ALL: [Self; 3] = [Self::Low, Self::Medium, Self::High];

    /// Parses a complexity choice or returns an actionable validation error.
    pub fn parse(raw: &str) -> Result<Self, AppError> {
        match raw {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            _ => Err(AppError::Validation(format!(
                "invalid complexity `{raw}`; use low, medium, or high. Read story help complexity-rubric"
            ))),
        }
    }

    /// Stable CLI and JSON spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}
