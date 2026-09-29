//! Read-only output evidence for one owned verification attempt. This never
//! writes the progress journal or renews a subprocess deadline.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::Deserialize;

/// The verifier's binding to the unique log it created before running a gate.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct OutputReference {
    /// UUID of the active verifier attempt, not just its submission generation.
    pub attempt_id: String,
    /// Exact log path supplied by the process that owns its output descriptors.
    pub path: PathBuf,
    /// Device containing the originally created log.
    pub dev: u64,
    /// Inode of the originally created log.
    pub ino: u64,
    /// RFC3339 time when output capture was registered, before execution.
    pub at: String,
}

/// Output observation is independent of structured progress and gate verdicts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputObservation {
    /// The known gate execution is terminal; its capture is historical.
    NotCapturing,
    /// Seconds since the last observed write, or capture start if still empty.
    Observed(u64),
    /// Missing or invalid evidence must not be described as silence or activity.
    Unavailable(String),
}

#[derive(Debug)]
struct Extent {
    reference: OutputReference,
    length: u64,
    last_output: DateTime<Utc>,
    observed_at: DateTime<Utc>,
}

/// How a measurement dates growth past the committed baseline.
#[derive(Clone, Copy)]
enum GrowthDating {
    /// At the observation itself: the publisher's committed rule (SH-713).
    /// It only ever sees growth at its own ticks, and records what it saw.
    Observation,
    /// At the log's own modification second, clamped to the interval since
    /// the committed observation (SH-777): a read between two publisher
    /// ticks must not report the growth as older than it is, nor date it
    /// before the baseline that did not yet contain it.
    Modification,
}

/// Monotonic extent tracking owned by a single active verification slot.
#[derive(Debug, Default)]
pub(crate) struct OutputObserver {
    extent: Option<Extent>,
    invalid: Option<String>,
}

impl OutputObserver {
    /// Observe an authenticated reference. Once bound, evidence loss remains
    /// visible until the owner releases this observer with its attempt.
    pub(crate) fn observe(
        &mut self,
        reference: Option<&OutputReference>,
        attempt_id: &str,
        started_at: &str,
        now: &str,
    ) -> OutputObservation {
        if let Some(detail) = &self.invalid {
            return OutputObservation::Unavailable(detail.clone());
        }
        let result = self
            .measure(
                reference,
                attempt_id,
                started_at,
                now,
                GrowthDating::Observation,
            )
            .and_then(|extent| {
                let age = extent.age()?;
                self.extent = Some(extent);
                Ok(age)
            });
        match result {
            Ok(age) => OutputObservation::Observed(age),
            Err(detail) => {
                // Before registration an absent journal is an ordinary startup
                // state. After binding, losing it cannot reset the baseline.
                if self.extent.is_some() {
                    self.invalid = Some(detail.clone());
                }
                OutputObservation::Unavailable(detail)
            }
        }
    }

    /// Reads the current observation without committing it (SH-777): the
    /// progress publisher alone moves the baseline, so a status read can
    /// neither poison it with a different clock nor hide growth from it.
    /// A binding the publisher already lost stays unavailable here too.
    pub(crate) fn peek(
        &self,
        reference: Option<&OutputReference>,
        attempt_id: &str,
        started_at: &str,
        now: &str,
    ) -> OutputObservation {
        if let Some(detail) = &self.invalid {
            return OutputObservation::Unavailable(detail.clone());
        }
        match self
            .measure(
                reference,
                attempt_id,
                started_at,
                now,
                GrowthDating::Modification,
            )
            .and_then(|extent| extent.age())
        {
            Ok(age) => OutputObservation::Observed(age),
            Err(detail) => OutputObservation::Unavailable(detail),
        }
    }

    /// Validates `reference` against this attempt and the committed extent and
    /// measures the log, without committing anything: the caller decides
    /// whether the measurement becomes the new baseline.
    fn measure(
        &self,
        reference: Option<&OutputReference>,
        attempt_id: &str,
        started_at: &str,
        now: &str,
        dating: GrowthDating,
    ) -> Result<Extent, String> {
        let reference = reference.ok_or("current-attempt output reference is unavailable")?;
        if reference.attempt_id != attempt_id {
            return Err("output reference belongs to another attempt".into());
        }
        if self
            .extent
            .as_ref()
            .is_some_and(|extent| &extent.reference != reference)
        {
            return Err("output reference changed during the attempt".into());
        }
        let now = timestamp(now)?;
        if self
            .extent
            .as_ref()
            .is_some_and(|extent| now < extent.observed_at)
        {
            return Err("output observation clock moved backwards".into());
        }
        let started = timestamp(started_at)?;
        let capture = timestamp(&reference.at)?;
        if capture < started || capture > now {
            return Err("output capture timestamp is outside the current attempt".into());
        }
        // lstat never follows a substituted symlink or reads a special file.
        let metadata = std::fs::symlink_metadata(&reference.path)
            .map_err(|error| format!("output log {}: {error}", reference.path.display()))?;
        if !metadata.is_file() {
            return Err(format!(
                "output log {} is not a regular file",
                reference.path.display()
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if (metadata.dev(), metadata.ino()) != (reference.dev, reference.ino) {
                return Err(format!(
                    "output log {} was replaced",
                    reference.path.display()
                ));
            }
        }
        #[cfg(not(unix))]
        return Err("output file identity is unavailable on this platform".into());

        let length = metadata.len();
        if self
            .extent
            .as_ref()
            .is_some_and(|extent| length < extent.length)
        {
            return Err(format!(
                "output log {} was truncated",
                reference.path.display()
            ));
        }
        let grew = self
            .extent
            .as_ref()
            .is_none_or(|extent| length > extent.length);
        let last_output = if let Some(committed) = self.extent.as_ref().filter(|_| grew) {
            // Growth is directly observed; a touch without growth never
            // reaches this branch. This is activity, not semantic progress.
            match dating {
                GrowthDating::Observation => now,
                GrowthDating::Modification => {
                    modified_second(&metadata)?.clamp(committed.observed_at, now)
                }
            }
        } else if grew && length > 0 {
            let modified = modified_second(&metadata)?;
            if modified < capture || modified > now {
                return Err("output modification timestamp is outside the current capture".into());
            }
            modified
        } else {
            self.extent
                .as_ref()
                .map_or(capture, |extent| extent.last_output)
        };
        Ok(Extent {
            reference: reference.clone(),
            length,
            last_output,
            observed_at: now,
        })
    }
}

impl Extent {
    /// Seconds from the last output to the observation that measured it.
    fn age(&self) -> Result<u64, String> {
        u64::try_from((self.observed_at - self.last_output).num_seconds())
            .map_err(|_| "output observation clock moved backwards".to_string())
    }
}

/// The log's modification time on the daemon clock's whole-second grid:
/// Environment's RFC3339 clock has whole-second precision, and a real write
/// at .500 within the current second must not look like the future.
fn modified_second(metadata: &std::fs::Metadata) -> Result<DateTime<Utc>, String> {
    let modified = metadata_time(
        metadata
            .modified()
            .map_err(|error| format!("output log modification time: {error}"))?,
    )?;
    DateTime::from_timestamp(modified.timestamp(), 0)
        .ok_or_else(|| "output modification timestamp is out of range".to_string())
}

fn timestamp(value: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(value)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|error| format!("invalid output observation timestamp {value:?}: {error}"))
}

/// Convert filesystem time without Chrono's infallible `From<SystemTime>`:
/// a representable OS timestamp may still exceed Chrono's calendar range.
pub(crate) fn metadata_time(time: std::time::SystemTime) -> Result<DateTime<Utc>, String> {
    let invalid = || "filesystem timestamp is outside the supported range".to_string();
    let (seconds, nanos) = match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => (
            i64::try_from(duration.as_secs()).map_err(|_| invalid())?,
            duration.subsec_nanos(),
        ),
        Err(error) => {
            let duration = error.duration();
            let seconds = i64::try_from(duration.as_secs()).map_err(|_| invalid())?;
            let seconds = seconds.checked_neg().ok_or_else(invalid)?;
            if duration.subsec_nanos() == 0 {
                (seconds, 0)
            } else {
                (
                    seconds.checked_sub(1).ok_or_else(invalid)?,
                    1_000_000_000 - duration.subsec_nanos(),
                )
            }
        }
    };
    DateTime::from_timestamp(seconds, nanos).ok_or_else(invalid)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs::{File, FileTimes};
    use std::os::unix::fs::MetadataExt;

    const START: &str = "2026-09-13T04:00:00Z";
    const NOW: &str = "2026-09-13T04:10:00Z";

    #[test]
    fn filesystem_times_convert_fallibly_at_calendar_boundaries() {
        use std::time::{Duration, UNIX_EPOCH};
        assert_eq!(metadata_time(UNIX_EPOCH).unwrap().timestamp(), 0);
        assert_eq!(
            metadata_time(UNIX_EPOCH - Duration::from_nanos(1))
                .unwrap()
                .timestamp(),
            -1
        );
        let beyond_calendar = UNIX_EPOCH.checked_add(Duration::from_secs(9_000_000_000_000));
        if let Some(time) = beyond_calendar {
            assert!(metadata_time(time).is_err());
        }
    }

    fn reference(path: PathBuf) -> OutputReference {
        let m = std::fs::metadata(&path).unwrap();
        OutputReference {
            attempt_id: "owned".into(),
            path,
            dev: m.dev(),
            ino: m.ino(),
            at: START.into(),
        }
    }

    fn write_at(path: &std::path::Path, bytes: &[u8], at: &str) {
        std::fs::write(path, bytes).unwrap();
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(timestamp(at).unwrap().into()))
            .unwrap();
    }

    #[test]
    fn output_growth_is_separate_from_creation_and_touches() {
        let dir = storyhook_test_support::scratch_dir();
        let path = dir.path().join("log");
        write_at(&path, b"", START);
        let reference = reference(path.clone());
        let mut observer = OutputObserver::default();
        assert_eq!(
            observer.observe(Some(&reference), "owned", START, NOW),
            OutputObservation::Observed(600)
        );
        write_at(&path, b"stdout without newline", "2026-09-13T04:09:50Z");
        assert_eq!(
            observer.observe(Some(&reference), "owned", START, NOW),
            OutputObservation::Observed(0)
        );
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(timestamp(NOW).unwrap().into()))
            .unwrap();
        assert_eq!(
            observer.observe(Some(&reference), "owned", START, "2026-09-13T04:10:10Z"),
            OutputObservation::Observed(10)
        );
        write_at(&path, b"stdout without newline; stderr", NOW);
        assert_eq!(
            observer.observe(Some(&reference), "owned", START, "2026-09-13T04:10:20Z"),
            OutputObservation::Observed(0)
        );
    }

    #[test]
    fn replacement_truncation_and_binding_loss_cannot_rebase_an_attempt() {
        for damage in ["replace", "truncate", "missing", "symlink", "binding"] {
            let dir = storyhook_test_support::scratch_dir();
            let path = dir.path().join("log");
            write_at(&path, b"ordinary output", START);
            let reference = reference(path.clone());
            let mut observer = OutputObserver::default();
            assert_eq!(
                observer.observe(Some(&reference), "owned", START, NOW),
                OutputObservation::Observed(600)
            );
            match damage {
                "replace" | "symlink" => {
                    std::fs::rename(&path, dir.path().join("old")).unwrap();
                    if damage == "symlink" {
                        std::os::unix::fs::symlink(dir.path().join("old"), &path).unwrap();
                    } else {
                        write_at(&path, b"new output", NOW);
                    }
                }
                "truncate" => write_at(&path, b"", NOW),
                "missing" => std::fs::remove_file(&path).unwrap(),
                _ => {}
            }
            let input = (damage != "binding").then_some(&reference);
            let failed = observer.observe(input, "owned", START, NOW);
            assert!(
                matches!(failed, OutputObservation::Unavailable(_)),
                "{damage}: {failed:?}"
            );
            assert_eq!(
                observer.observe(Some(&reference), "owned", START, NOW),
                failed,
                "{damage}"
            );
        }
    }

    #[test]
    fn initial_observation_requires_current_identity_and_valid_times() {
        let dir = storyhook_test_support::scratch_dir();
        let path = dir.path().join("log");
        write_at(&path, b"output", "2026-09-13T04:09:59Z");
        let mut reference = reference(path.clone());
        assert_eq!(
            OutputObserver::default().observe(Some(&reference), "owned", START, NOW),
            OutputObservation::Observed(1)
        );
        for (attempt, started, now) in [
            ("foreign", START, NOW),
            ("owned", NOW, NOW),
            ("owned", "invalid", NOW),
            ("owned", START, "invalid"),
        ] {
            assert!(matches!(
                OutputObserver::default().observe(Some(&reference), attempt, started, now),
                OutputObservation::Unavailable(_)
            ));
        }
        for modified in ["2026-09-13T03:59:59Z", "2026-09-13T04:10:01Z"] {
            write_at(&path, b"output", modified);
            assert!(matches!(
                OutputObserver::default().observe(Some(&reference), "owned", START, NOW),
                OutputObservation::Unavailable(_)
            ));
        }
        reference.at = "invalid".into();
        assert!(matches!(
            OutputObserver::default().observe(Some(&reference), "owned", START, NOW),
            OutputObservation::Unavailable(_)
        ));
    }

    #[test]
    fn subsecond_initial_output_is_not_future_and_clock_reversal_is_unavailable() {
        let dir = storyhook_test_support::scratch_dir();
        let path = dir.path().join("log");
        write_at(&path, b"output", "2026-09-13T04:10:00.999Z");
        let reference = reference(path);
        let mut observer = OutputObserver::default();
        assert_eq!(
            observer.observe(Some(&reference), "owned", START, NOW),
            OutputObservation::Observed(0)
        );
        assert!(matches!(
            observer.observe(Some(&reference), "owned", START, "2026-09-13T04:09:59Z"),
            OutputObservation::Unavailable(_)
        ));
    }

    #[test]
    fn startup_absence_and_first_sample_uncertainty_can_retry() {
        let dir = storyhook_test_support::scratch_dir();
        let path = dir.path().join("log");
        write_at(&path, b"", START);
        let reference = reference(path.clone());
        let mut observer = OutputObserver::default();
        assert!(matches!(
            observer.observe(None, "owned", START, NOW),
            OutputObservation::Unavailable(_)
        ));
        assert_eq!(
            observer.observe(Some(&reference), "owned", START, NOW),
            OutputObservation::Observed(600)
        );

        let mut fresh = OutputObserver::default();
        std::fs::rename(&path, dir.path().join("old")).unwrap();
        let unavailable = fresh.observe(Some(&reference), "owned", START, NOW);
        assert!(matches!(unavailable, OutputObservation::Unavailable(_)));
        std::fs::rename(dir.path().join("old"), &path).unwrap();
        assert_eq!(
            fresh.observe(Some(&reference), "owned", START, NOW),
            OutputObservation::Observed(600)
        );
    }

    #[test]
    fn clock_rollback_above_last_output_does_not_shorten_silence() {
        let dir = storyhook_test_support::scratch_dir();
        let path = dir.path().join("log");
        write_at(&path, b"output", START);
        let reference = reference(path);
        let mut observer = OutputObserver::default();
        assert_eq!(
            observer.observe(Some(&reference), "owned", START, NOW),
            OutputObservation::Observed(600)
        );
        assert!(matches!(
            observer.observe(Some(&reference), "owned", START, "2026-09-13T04:05:00Z"),
            OutputObservation::Unavailable(_)
        ));
    }

    /// SH-777: status reads between publisher ticks. A peek sees growth the
    /// publisher has not observed yet, dates it by the log itself, and never
    /// moves the baseline the publisher's own next observation compares to.
    #[test]
    fn peek_reports_growth_between_ticks_without_moving_the_baseline() {
        let dir = storyhook_test_support::scratch_dir();
        let path = dir.path().join("log");
        write_at(&path, b"first", START);
        let reference = reference(path.clone());
        let mut observer = OutputObserver::default();
        // Before the publisher binds anything, a peek applies the first-sample rule.
        assert_eq!(
            observer.peek(Some(&reference), "owned", START, NOW),
            OutputObservation::Observed(600)
        );
        assert_eq!(
            observer.observe(Some(&reference), "owned", START, NOW),
            OutputObservation::Observed(600)
        );
        // Growth at 04:12:00, read at 04:15:00, is three minutes old: not
        // "now" (the publisher's tick rule) and not the 04:10:00 baseline.
        write_at(&path, b"first second", "2026-09-13T04:12:00Z");
        let later = "2026-09-13T04:15:00Z";
        assert_eq!(
            observer.peek(Some(&reference), "owned", START, later),
            OutputObservation::Observed(180)
        );
        assert_eq!(
            observer.peek(Some(&reference), "owned", START, later),
            OutputObservation::Observed(180),
            "a peek is repeatable because it commits nothing"
        );
        // The publisher still sees the growth as new and records its own tick.
        assert_eq!(
            observer.observe(Some(&reference), "owned", START, later),
            OutputObservation::Observed(0)
        );
    }

    #[test]
    fn peek_clamps_growth_to_the_interval_since_the_baseline() {
        let dir = storyhook_test_support::scratch_dir();
        let path = dir.path().join("log");
        write_at(&path, b"first", START);
        let reference = reference(path.clone());
        let mut observer = OutputObserver::default();
        observer.observe(Some(&reference), "owned", START, NOW);
        let later = "2026-09-13T04:15:00Z";
        // An mtime older than the baseline cannot date growth the baseline
        // did not contain, and a future mtime cannot make it newer than now.
        for (modified, age) in [
            ("2026-09-13T04:01:00Z", 300),
            ("2026-09-13T04:30:00Z", 0),
            ("2026-09-13T04:15:00.999Z", 0),
        ] {
            write_at(&path, b"first second", modified);
            assert_eq!(
                observer.peek(Some(&reference), "owned", START, later),
                OutputObservation::Observed(age),
                "{modified}"
            );
        }
    }

    #[test]
    fn peek_counts_no_touch_and_keeps_every_invalidity() {
        let dir = storyhook_test_support::scratch_dir();
        let path = dir.path().join("log");
        write_at(&path, b"output", START);
        let reference = reference(path.clone());
        let mut observer = OutputObserver::default();
        observer.observe(Some(&reference), "owned", START, NOW);
        // A touch without growth keeps the committed last output.
        write_at(&path, b"output", "2026-09-13T04:14:00Z");
        assert_eq!(
            observer.peek(Some(&reference), "owned", START, "2026-09-13T04:15:00Z"),
            OutputObservation::Observed(900)
        );
        // A reader whose clock is behind the baseline gets no answer and
        // leaves the publisher's binding intact.
        assert!(matches!(
            observer.peek(Some(&reference), "owned", START, "2026-09-13T04:05:00Z"),
            OutputObservation::Unavailable(_)
        ));
        assert_eq!(
            observer.observe(Some(&reference), "owned", START, "2026-09-13T04:15:00Z"),
            OutputObservation::Observed(900)
        );
        // Evidence the publisher lost stays lost for a peek as well.
        write_at(&path, b"", "2026-09-13T04:15:00Z");
        assert!(matches!(
            observer.observe(Some(&reference), "owned", START, "2026-09-13T04:16:00Z"),
            OutputObservation::Unavailable(_)
        ));
        write_at(&path, b"regrown after truncation", "2026-09-13T04:16:00Z");
        assert!(matches!(
            observer.peek(Some(&reference), "owned", START, "2026-09-13T04:17:00Z"),
            OutputObservation::Unavailable(_)
        ));
    }
}
