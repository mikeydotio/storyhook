//! Classification of one publishing attempt, independent of the OS scheduler.

use std::time::Duration;

/// What one observed publishing attempt establishes.
#[derive(Debug, PartialEq)]
pub(super) enum Churned<T> {
    /// Publications stayed fresh for the complete stretch without a result.
    HeldOut,
    /// The client ended while publications were demonstrably fresh.
    GaveUp(T),
    /// A publishing pause invalidated this entire client attempt.
    Starved {
        /// Longest observed pause, including time spent receiving a result.
        stale: Duration,
    },
    /// The shared retry budget ended without a complete clean stretch.
    Deadline,
}

/// Elapsed observations for a single attempt. No method reads the clock.
pub(super) struct Measurement {
    stale_enough: Duration,
    stretch: Duration,
    budget: Duration,
    published: Duration,
    largest_gap: Duration,
}

impl Measurement {
    /// Starts an attempt at elapsed time zero with the supplied proof bounds.
    pub(super) fn new(stale_enough: Duration, stretch: Duration, budget: Duration) -> Self {
        Self {
            stale_enough,
            stretch,
            budget,
            published: Duration::ZERO,
            largest_gap: Duration::ZERO,
        }
    }

    /// Whether the shared budget forbids starting another publication.
    pub(super) fn expired(&self, now: Duration) -> bool {
        now >= self.budget
    }

    /// Records a publication; a past pause can never become clean again.
    pub(super) fn publish<T>(&mut self, now: Duration) -> Option<Churned<T>> {
        let disturbed = self.observe_gap(now);
        self.published = now;
        disturbed.map(|stale| Churned::Starved { stale })
    }

    /// Classifies a receive, preserving starvation before any result or pass.
    pub(super) fn receive<T>(&mut self, now: Duration, result: Option<T>) -> Option<Churned<T>> {
        if let Some(stale) = self.observe_gap(now) {
            Some(Churned::Starved { stale })
        } else if let Some(result) = result {
            Some(Churned::GaveUp(result))
        } else if self.expired(now) {
            Some(Churned::Deadline)
        } else if now >= self.stretch {
            Some(Churned::HeldOut)
        } else {
            None
        }
    }

    fn observe_gap(&mut self, now: Duration) -> Option<Duration> {
        let gap = now
            .checked_sub(self.published)
            .expect("elapsed observations must not go backwards");
        self.largest_gap = self.largest_gap.max(gap);
        (self.largest_gap >= self.stale_enough).then_some(self.largest_gap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STALE: Duration = Duration::from_millis(225);
    const STRETCH: Duration = Duration::from_secs(2);
    const BUDGET: Duration = Duration::from_secs(20);

    #[test]
    fn a_clean_early_client_result_is_preserved_as_a_failure() {
        let mut measurement = Measurement::new(STALE, STRETCH, BUDGET);
        assert_eq!(measurement.publish::<&str>(Duration::ZERO), None);
        assert_eq!(
            measurement.receive(Duration::ZERO, Some("unexpected transport error")),
            Some(Churned::GaveUp("unexpected transport error"))
        );
    }

    #[test]
    fn both_sides_of_a_publication_obey_the_exact_stale_threshold() {
        for gap in [
            STALE - Duration::from_nanos(1),
            STALE,
            STALE + Duration::from_nanos(1),
        ] {
            for before in [false, true] {
                let mut measurement = Measurement::new(STALE, STRETCH, BUDGET);
                let publication =
                    measurement.publish::<&str>(if before { gap } else { Duration::ZERO });
                let expected = if gap >= STALE {
                    Some(Churned::Starved { stale: gap })
                } else {
                    Some(Churned::GaveUp("result"))
                };
                if before && gap >= STALE {
                    assert_eq!(publication, expected);
                } else {
                    assert_eq!(publication, None);
                }
                assert_eq!(measurement.receive(gap, Some("result")), expected);
            }
        }
    }

    #[test]
    fn resumed_publications_cannot_validate_a_delayed_result() {
        let mut measurement = Measurement::new(STALE, STRETCH, BUDGET);
        assert_eq!(
            measurement.publish::<()>(STALE),
            Some(Churned::Starved { stale: STALE })
        );
        assert_eq!(
            measurement.publish::<()>(STALE + Duration::from_millis(10)),
            Some(Churned::Starved { stale: STALE })
        );
        assert_eq!(
            measurement.receive(STALE + Duration::from_millis(20), Some("late")),
            Some(Churned::Starved { stale: STALE })
        );
    }

    #[test]
    fn only_a_whole_clean_stretch_passes_and_budget_exhaustion_never_does() {
        for budget in [STRETCH - Duration::from_millis(1), STRETCH, BUDGET] {
            let mut measurement = Measurement::new(STALE, STRETCH, budget);
            for n in 0..20 {
                let now = Duration::from_millis(n * 100);
                assert!(!measurement.expired(now));
                assert_eq!(measurement.publish::<()>(now), None);
                assert_eq!(measurement.receive::<()>(now, None), None);
            }
            assert_eq!(
                measurement.receive::<()>(STRETCH, None),
                Some(if budget <= STRETCH {
                    Churned::Deadline
                } else {
                    Churned::HeldOut
                })
            );
        }
    }

    #[test]
    fn a_preexpired_budget_forbids_publication_and_a_late_receive_is_not_a_pass() {
        let mut measurement = Measurement::new(STALE, STRETCH, Duration::ZERO);
        assert!(measurement.expired(Duration::ZERO));
        assert_eq!(
            measurement.receive::<()>(Duration::ZERO, None),
            Some(Churned::Deadline)
        );
        assert_eq!(
            measurement.receive::<()>(BUDGET, None),
            Some(Churned::Starved { stale: BUDGET })
        );
    }
}
