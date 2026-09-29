//! How a lib test declared the production subprocess bounds it reaches
//! (SH-836). Compiled only under test.
//!
//! A production site names its bound and reads it through
//! [`super::Environment::subprocess_bound`]. A shipped build returns the
//! production value. A lib test states, on the `Environment` it builds,
//! which kind of wait each such bound is for it (the SH-806 rule):
//!
//! - **patience** — the test waits for an answer it expects; the bound is
//!   graced by the machine's contention through
//!   `storyhook_test_support::load_grace`, so a gate at utility QoS under
//!   load does not fail a correct answer that a starved spawn delayed;
//! - **proof** — the test shows the production bound itself (it fires, or it
//!   is shared); the bound stays the production value.
//!
//! The policy travels on the `Environment`, never on a thread or a process
//! global, so a daemon or worker thread that production hands the same
//! `Environment` applies the same policy (council D1 on SH-836).

use std::time::Duration;

use storyhook_test_support::load_grace;

/// Parts per thousand in a stored reading: `Environment` derives `Eq`, which
/// an `f64` cannot.
const PER_MILLE: f64 = 1000.0;

/// One test's declaration for every production subprocess bound it reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SubprocessPolicy {
    /// Nothing declared.
    Undeclared,
    /// Every bound is the production value.
    Proof,
    /// Every bound is graced by one contention reading (runnable threads per
    /// core, in parts per thousand), taken when the test declared it.
    Patience { contention_per_mille: u32 },
}

impl SubprocessPolicy {
    /// Patience fixed at `reading`, reported once when it grants grace.
    ///
    /// Fixed rather than resampled at each read (decision D2 on SH-836): a
    /// test derives deadlines and fixture delays from the bound, and
    /// production must read the same bound after it.
    pub(crate) fn patience(reading: Option<f64>) -> Self {
        let contention_per_mille = reading
            .filter(|ratio| ratio.is_finite() && *ratio > 1.0)
            .map_or(1000, |ratio| {
                (ratio * PER_MILLE).round().min(f64::from(u32::MAX)) as u32
            });
        if contention_per_mille > 1000 {
            eprintln!(
                "subprocess patience (SH-836): contention={:.2}; production subprocess bounds \
                 are graced by it for this test",
                f64::from(contention_per_mille) / PER_MILLE
            );
        }
        Self::Patience {
            contention_per_mille,
        }
    }

    /// The bound a production site that names `production` gets.
    pub(crate) fn bound(self, production: Duration) -> Duration {
        match self {
            Self::Undeclared | Self::Proof => production,
            Self::Patience {
                contention_per_mille,
            } => load_grace::graced_by(
                production,
                Some(f64::from(contention_per_mille) / PER_MILLE),
            ),
        }
    }
}

mod tests {
    use super::*;

    const PRODUCTION: Duration = Duration::from_secs(3);

    #[test]
    fn proof_is_the_production_value() {
        assert_eq!(SubprocessPolicy::Proof.bound(PRODUCTION), PRODUCTION);
    }

    #[test]
    fn patience_grants_its_reading_and_nothing_at_idle() {
        for idle in [None, Some(0.0), Some(1.0), Some(f64::NAN)] {
            assert_eq!(
                SubprocessPolicy::patience(idle).bound(PRODUCTION),
                PRODUCTION,
                "{idle:?}"
            );
        }
        let loaded = SubprocessPolicy::patience(Some(3.0));
        assert_eq!(loaded.bound(PRODUCTION), Duration::from_secs(9));
        assert_eq!(
            loaded.bound(PRODUCTION),
            loaded.bound(PRODUCTION),
            "one declaration grants one bound however often it is read"
        );
        assert_eq!(
            SubprocessPolicy::patience(Some(1e12)).bound(PRODUCTION),
            load_grace::PATIENCE_CEILING
        );
    }
}
