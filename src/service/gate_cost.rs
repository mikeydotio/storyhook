//! Durable observations of verification cost. These records grant no authority.

pub mod current;
pub mod journal;
#[cfg(test)]
mod tests;
pub mod view;

/// Admission-to-verdict limit; observation does not enforce termination.
pub const BUDGET_MS: u64 = 900_000;

/// A restart-safe elapsed-time checkpoint, independent of gate progress.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Elapsed {
    /// Last trustworthy elapsed lower bound, in milliseconds.
    pub milliseconds: u64,
    /// UTC time paired with the last checkpoint.
    pub checkpoint_at: String,
    /// Whether elapsed time contains a reconstructed UTC gap.
    pub estimated: bool,
    /// Missing or inconsistent clock evidence.
    pub diagnostic: Option<String>,
    /// First observed budget breach; a later sample cannot clear it.
    pub breached_at: Option<String>,
}

impl Elapsed {
    /// Starts accounting at admission.
    pub fn new(at: &str) -> Self {
        Self {
            milliseconds: 0,
            checkpoint_at: at.into(),
            estimated: false,
            diagnostic: None,
            breached_at: None,
        }
    }

    /// Records monotonic elapsed milliseconds from admission, never a delta.
    pub fn observe(&mut self, elapsed_ms: u64, at: &str) {
        if elapsed_ms < self.milliseconds {
            self.diagnostic =
                Some("elapsed clock moved backward; retained prior lower bound".into());
        } else {
            self.milliseconds = elapsed_ms;
            self.checkpoint_at = at.into();
        }
        self.check_breach(at);
    }

    /// Adds a UTC restart gap to the last persisted checkpoint.
    pub fn restart(&mut self, at: &str) {
        self.estimated = true;
        match utc_milliseconds(&self.checkpoint_at, at) {
            Some(gap) => match self.milliseconds.checked_add(gap) {
                Some(elapsed) => {
                    self.milliseconds = elapsed;
                    self.checkpoint_at = at.into();
                }
                None => self.diagnostic = Some("restart elapsed time overflow".into()),
            },
            None => {
                self.diagnostic = Some(format!(
                    "cannot reconstruct restart gap from {} to {at}; retained prior lower bound",
                    self.checkpoint_at
                ))
            }
        }
        self.check_breach(at);
    }

    fn check_breach(&mut self, at: &str) {
        if self.milliseconds >= BUDGET_MS && self.breached_at.is_none() {
            self.breached_at = Some(at.into());
        }
    }
}

/// Nonnegative UTC interval, unavailable for malformed or reversed timestamps.
pub fn utc_milliseconds(start: &str, end: &str) -> Option<u64> {
    let start = chrono::DateTime::parse_from_rfc3339(start).ok()?;
    let end = chrono::DateTime::parse_from_rfc3339(end).ok()?;
    if end < start {
        return None;
    }
    u64::try_from((end - start).num_milliseconds()).ok()
}
