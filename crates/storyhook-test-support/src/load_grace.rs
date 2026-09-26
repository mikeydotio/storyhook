//! Contention grace for harness patience: the Rust port of SH-347 (SH-806).
//!
//! The user determination this implements (SH-347, 2026-08-17) reads: relax the
//! timeouts when the machine is under load, up to a maximum of 15 minutes,
//! rather than ending the test. `e2e/load-grace.ts` applies it to the browser
//! suite and `scripts/tests/load_grace.py` (SH-767, SH-766) to the Python
//! harnesses; this module is the same policy for Rust integration tests.
//!
//! **Patience, never proof.** Only a wait for something the test expects to
//! become true may be graced. A ceiling that proves a production deadline did
//! not fire derives from that deadline and is never multiplied
//! (docs/spec/test-tiers.md, the timing-ceiling rule and SH-643).
//!
//! At or below one runnable thread per core every value here is exactly the
//! idle value, so a real defect surfaces as fast as it did before grace
//! existed. The multiplier assumes fair processor sharing; a gate clamped to
//! utility QoS (SH-785) can stretch further than load per core shows, which
//! is why a graced wait can still be extended when it expires.
//!
//! The load average is read in-process: grace computed by spawning a helper
//! would stall exactly when spawns are starved, which is when it is needed.

use std::fmt;
use std::time::{Duration, Instant};

/// SH-347's recorded tolerance for any one graced wait: 15 minutes.
///
/// A recorded human tolerance, not a measurement, and the same value as
/// `PATIENCE_CEILING` in `scripts/tests/load_grace.py` and
/// `MAX_TEST_TIMEOUT_MS` in `e2e/load-grace.ts`.
pub const PATIENCE_CEILING: Duration = Duration::from_secs(15 * 60);

/// The logical cores this process may run on, never zero.
fn cores() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
}

/// Runnable threads per core over the last minute, or `None` when the
/// machine cannot say.
///
/// A one-minute average reacts late to a burst and lingers after one; a late
/// reading costs one wait its extension, a lingering one only extra patience.
pub fn contention() -> Option<f64> {
    let mut averages = [0.0_f64; 1];
    // SAFETY: the pointer addresses `averages`, which holds exactly the one
    // sample requested, and getloadavg writes at most that many.
    let written = unsafe { libc::getloadavg(averages.as_mut_ptr(), 1) };
    (written == 1).then(|| averages[0] / cores() as f64)
}

/// The grace one reading grants: exactly 1 at or below one thread per core,
/// or with no usable reading at all.
fn multiplier(ratio: Option<f64>) -> f64 {
    match ratio {
        Some(ratio) if ratio.is_finite() && ratio > 1.0 => ratio,
        _ => 1.0,
    }
}

/// `base` graced by one reading, within [`PATIENCE_CEILING`].
///
/// The ceiling bounds the result, not the multiplier, as `gracedTestBudget`
/// in `e2e/load-grace.ts` does. A base already above the ceiling is returned
/// unchanged: grace never shortens a wait.
fn graced(base: Duration, ratio: Option<f64>) -> Duration {
    let scaled = Duration::try_from_secs_f64(base.as_secs_f64() * multiplier(ratio))
        .unwrap_or(PATIENCE_CEILING);
    base.max(scaled.min(PATIENCE_CEILING))
}

/// Names one reading and the grace chosen from it.
fn describe(ratio: Option<f64>) -> String {
    let reading = ratio.map_or_else(|| "unavailable".to_string(), |ratio| format!("{ratio:.2}"));
    format!(
        "load-grace: contention={reading} cores={} multiplier={:.2}",
        cores(),
        multiplier(ratio)
    )
}

/// A duration as seconds with two decimals, the unit every reading above uses.
fn seconds(duration: Duration) -> String {
    format!("{:.2}s", duration.as_secs_f64())
}

/// `base` graced by the contention right now, reported on stderr whenever it
/// grants more than `base`.
///
/// For a bound handed over whole, such as a child wait that encloses graced
/// waits of its own; a poll loop uses [`Patience`], which can also extend.
pub fn graced_now(base: Duration) -> Duration {
    let ratio = contention();
    let granted = graced(base, ratio);
    if granted > base {
        eprintln!(
            "{}: {} -> {}",
            describe(ratio),
            seconds(base),
            seconds(granted)
        );
    }
    granted
}

/// A wait allowance that is graced by contention when it starts and extended,
/// never shortened, when contention has risen by the time it expires.
///
/// The browser suite extends a Playwright timeout before it fires, because a
/// fired one cannot be resumed; a Rust poll loop can simply look again, so
/// this samples again at each expiry.
pub struct Patience {
    idle: Duration,
    reading: Option<f64>,
    allowance: Duration,
    started: Instant,
    sample: Box<dyn Fn() -> Option<f64>>,
}

impl Patience {
    /// Starts an allowance of `idle`, graced by this machine's contention.
    #[must_use]
    pub fn new(idle: Duration) -> Self {
        Self::starting_at(idle, Instant::now(), contention)
    }

    /// Starts an allowance of `idle` at `started`, graced by `sample`.
    ///
    /// For a test that states the contention it is proving grace against,
    /// such as a floor under the real reading. Never an environment variable
    /// read by every wait: a knob exported in a shell would widen them all.
    #[must_use]
    pub fn starting_at(
        idle: Duration,
        started: Instant,
        sample: impl Fn() -> Option<f64> + 'static,
    ) -> Self {
        let reading = sample();
        let allowance = graced(idle, reading);
        if allowance > idle {
            eprintln!(
                "{}: {} -> {}",
                describe(reading),
                seconds(idle),
                seconds(allowance)
            );
        }
        Self {
            idle,
            reading,
            allowance,
            started,
            sample: Box::new(sample),
        }
    }

    /// Whether the allowance has run out now; see [`Self::expired_at`].
    pub fn expired(&mut self) -> bool {
        self.expired_at(Instant::now())
    }

    /// Whether the allowance has run out at `now`, after sampling again and
    /// extending it if contention has risen since it was granted.
    pub fn expired_at(&mut self, now: Instant) -> bool {
        if now.saturating_duration_since(self.started) < self.allowance {
            return false;
        }
        let reading = (self.sample)();
        let allowance = graced(self.idle, reading);
        if allowance > self.allowance {
            self.allowance = allowance;
            self.reading = reading;
            eprintln!(
                "load-grace: extended a wait to {}; {}",
                seconds(allowance),
                describe(reading)
            );
        }
        now.saturating_duration_since(self.started) >= self.allowance
    }

    /// The allowance currently granted, including any extension.
    #[must_use]
    pub fn allowance(&self) -> Duration {
        self.allowance
    }
}

impl fmt::Display for Patience {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "waited {} of {} (idle {}); {}",
            seconds(self.started.elapsed()),
            seconds(self.allowance),
            seconds(self.idle),
            describe(self.reading)
        )
    }
}

/// Polls `ready` every `poll` until it yields, or panics naming `what` and the
/// patience that ran out.
///
/// Each pass observes before it judges the clock, so a condition that became
/// true while the waiter was starved still counts (SH-766).
pub fn wait_for<T>(
    mut patience: Patience,
    poll: Duration,
    what: impl FnOnce() -> String,
    mut ready: impl FnMut() -> Option<T>,
) -> T {
    loop {
        if let Some(value) = ready() {
            return value;
        }
        if patience.expired() {
            panic!("{}: {patience}", what());
        }
        std::thread::sleep(poll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    const IDLE: Duration = Duration::from_millis(20);

    /// A sampler that replays `readings`, repeating the last one.
    fn replay(readings: &[Option<f64>]) -> impl Fn() -> Option<f64> + 'static {
        let queue = Rc::new(RefCell::new(readings.to_vec()));
        move || {
            let mut queue = queue.borrow_mut();
            if queue.len() > 1 {
                queue.remove(0)
            } else {
                queue[0]
            }
        }
    }

    #[test]
    fn an_idle_or_unknown_reading_leaves_every_base_unchanged() {
        for ratio in [
            None,
            Some(0.0),
            Some(0.5),
            Some(1.0),
            Some(f64::NAN),
            Some(-3.0),
        ] {
            assert_eq!(multiplier(ratio), 1.0, "{ratio:?}");
            assert_eq!(graced(IDLE, ratio), IDLE, "{ratio:?}");
        }
        assert_eq!(
            multiplier(Some(f64::INFINITY)),
            1.0,
            "a nonsense reading grants nothing"
        );
    }

    #[test]
    fn grace_tracks_contention_above_one_thread_per_core() {
        assert_eq!(multiplier(Some(4.8)), 4.8);
        assert_eq!(
            graced(Duration::from_secs(10), Some(3.5)),
            Duration::from_secs(35)
        );
    }

    #[test]
    fn the_ceiling_bounds_the_result_and_never_shortens_a_base() {
        let ten_minutes = Duration::from_secs(10 * 60);
        assert_eq!(graced(ten_minutes, Some(4.0)), PATIENCE_CEILING);
        assert_eq!(graced(Duration::from_secs(1), Some(1e12)), PATIENCE_CEILING);
        let twenty_minutes = Duration::from_secs(20 * 60);
        assert_eq!(graced(twenty_minutes, Some(4.0)), twenty_minutes);
        assert_eq!(PATIENCE_CEILING, Duration::from_secs(15 * 60));
    }

    #[test]
    fn patience_extends_when_contention_rises_and_never_shrinks() {
        let started = Instant::now();
        let mut patience =
            Patience::starting_at(IDLE, started, replay(&[Some(1.0), Some(5.0), Some(0.5)]));
        assert_eq!(patience.allowance(), IDLE);
        assert!(
            !patience.expired_at(started + IDLE / 2),
            "inside the idle allowance"
        );
        assert!(
            !patience.expired_at(started + IDLE + IDLE / 4),
            "contention rose to 5 by expiry, so the wait is extended"
        );
        assert_eq!(patience.allowance(), IDLE * 5);
        assert!(
            patience.expired_at(started + IDLE * 6),
            "contention fell to 0.5, and a granted extension is never retracted"
        );
        assert_eq!(patience.allowance(), IDLE * 5);
    }

    #[test]
    fn a_patience_names_its_wait_and_its_reading() {
        let started = Instant::now();
        let mut patience = Patience::starting_at(IDLE, started, replay(&[Some(2.5)]));
        assert!(patience.expired_at(started + IDLE * 3));
        let text = patience.to_string();
        for part in [
            "waited",
            "of 0.05s",
            "idle 0.02s",
            "contention=2.50",
            "multiplier=2.50",
        ] {
            assert!(text.contains(part), "{part:?} missing from {text:?}");
        }
        let unknown = Patience::starting_at(IDLE, started, replay(&[None])).to_string();
        assert!(unknown.contains("contention=unavailable"), "{unknown}");
    }

    #[test]
    fn wait_for_returns_what_became_ready_under_grace() {
        let started = Instant::now();
        // Ready after three idle allowances: only a graced patience outlives that.
        let value = wait_for(
            Patience::starting_at(IDLE, started, replay(&[Some(20.0)])),
            Duration::from_millis(5),
            || "a value that arrives late".into(),
            || (started.elapsed() >= IDLE * 3).then_some(7),
        );
        assert_eq!(value, 7);
    }

    #[test]
    fn wait_for_at_idle_panics_naming_the_wait_and_the_reading() {
        let failure = std::panic::catch_unwind(|| {
            wait_for(
                Patience::starting_at(IDLE, Instant::now(), replay(&[None])),
                Duration::from_millis(5),
                || "the fixture that never publishes".into(),
                || None::<()>,
            )
        })
        .expect_err("an idle wait for something that never happens must fail");
        let message = failure
            .downcast_ref::<String>()
            .expect("wait_for panics with a formatted message");
        assert!(
            message.starts_with("the fixture that never publishes: waited"),
            "{message}"
        );
        assert!(message.contains("contention=unavailable"), "{message}");
    }

    #[test]
    fn this_machine_reports_a_usable_reading() {
        assert!(cores() >= 1);
        let ratio = contention().expect("macOS and Linux both provide getloadavg");
        assert!(ratio.is_finite() && ratio >= 0.0, "{ratio}");
        assert!(describe(Some(ratio)).starts_with("load-grace: contention="));
    }
}
