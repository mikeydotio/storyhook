//! Bounded launchd registration transitions. Tests replace only OS observations.

use crate::error::AppError;
use std::{
    path::Path,
    process::Output,
    time::{Duration, Instant},
};

/// One removal/bootstrap transaction shares this total wall-clock budget.
pub(crate) const REGISTRATION_DEADLINE: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(100);

/// OS boundary and clock for a registration transaction.
pub(crate) struct Control<'a> {
    /// Runs a launchctl command within the supplied remaining budget.
    pub(crate) run: &'a dyn Fn(&[&str], Duration) -> std::io::Result<Output>,
    /// Monotonic clock, replaced only by deterministic tests.
    pub(crate) now: &'a dyn Fn() -> Instant,
    /// Waits between observations without busy polling.
    pub(crate) sleep: &'a dyn Fn(Duration),
}

impl Control<'_> {
    /// Checks this exact target without parsing launchctl's diagnostic text.
    pub(crate) fn registered(&self, target: &str) -> Result<bool, AppError> {
        self.present(target, (self.now)() + REGISTRATION_DEADLINE)
    }

    /// Unloads the exact service and waits until its registration is absent.
    pub(crate) fn unload(&self, target: &str, until: Instant) -> Result<(), AppError> {
        let args = ["bootout", target];
        let result = self.call(&args, until)?;
        if !result.status.success()
            && result.status.code() != Some(super::LAUNCHCTL_SERVICE_NOT_FOUND)
        {
            return Err(refusal(&args, &result));
        }
        self.await_absent(target, until)
    }

    /// Loads a plist after removal, tolerating only the bounded error-5 race.
    pub(crate) fn load(&self, target: &str, path: &Path, until: Instant) -> Result<(), AppError> {
        let domain = target
            .rsplit_once('/')
            .ok_or_else(|| AppError::Storage(format!("invalid launchd target {target}")))?
            .0;
        let path = path
            .to_str()
            .ok_or_else(|| AppError::Storage("launchd plist path is not UTF-8".into()))?;
        let args = ["bootstrap", domain, path];
        let mut failures = Vec::new();
        let result = (|| {
            loop {
                let result = self.call(&args, until)?;
                if result.status.success() {
                    return Ok(());
                }
                let failure = refusal(&args, &result);
                if result.status.code() != Some(5) {
                    return Err(failure);
                }
                failures.push(failure.to_string());
                self.pause(until)?;
                self.await_absent(target, until)?;
            }
        })();
        result.map_err(|error| {
            AppError::Storage(format!(
                "{error}{}",
                if failures.is_empty() {
                    String::new()
                } else {
                    format!("\nEarlier bootstrap failures:\n{}", failures.join("\n"))
                }
            ))
        })
    }

    /// Replaces a job within one removal/bootstrap budget.
    pub(crate) fn replace(&self, target: &str, path: &Path) -> Result<(), AppError> {
        let until = (self.now)() + REGISTRATION_DEADLINE;
        self.unload(target, until)?;
        self.load(target, path, until)
    }

    fn call(&self, args: &[&str], until: Instant) -> Result<Output, AppError> {
        let remaining = until.saturating_duration_since((self.now)());
        if remaining.is_zero() {
            return Err(AppError::Storage(format!(
                "launchctl {}: registration deadline expired",
                args.join(" ")
            )));
        }
        (self.run)(args, remaining)
            .map_err(|error| AppError::Storage(format!("launchctl {}: {error}", args.join(" "))))
    }

    fn present(&self, target: &str, until: Instant) -> Result<bool, AppError> {
        let args = ["print", target];
        let result = self.call(&args, until)?;
        if result.status.success() {
            Ok(true)
        } else if result.status.code() == Some(super::LAUNCHCTL_SERVICE_NOT_FOUND) {
            Ok(false)
        } else {
            Err(refusal(&args, &result))
        }
    }

    fn await_absent(&self, target: &str, until: Instant) -> Result<(), AppError> {
        while self.present(target, until)? {
            self.pause(until)?;
        }
        Ok(())
    }

    fn pause(&self, until: Instant) -> Result<(), AppError> {
        let remaining = until.saturating_duration_since((self.now)());
        if remaining.is_zero() {
            return Err(AppError::Storage(
                "launchd registration deadline expired".into(),
            ));
        }
        (self.sleep)(POLL.min(remaining));
        Ok(())
    }
}

fn refusal(args: &[&str], output: &Output) -> AppError {
    AppError::Storage(format!(
        "launchctl {} failed ({}): {} {}",
        args.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stderr).trim(),
        String::from_utf8_lossy(&output.stdout).trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        collections::VecDeque,
        os::unix::process::ExitStatusExt,
    };

    fn output(code: i32, text: &str) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: vec![],
            stderr: text.as_bytes().to_vec(),
        }
    }

    struct Fixture {
        time: Cell<Instant>,
        replies: RefCell<VecDeque<(&'static str, i32)>>,
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl Fixture {
        fn new(replies: &[(&'static str, i32)]) -> Self {
            Self {
                time: Cell::new(Instant::now()),
                replies: RefCell::new(replies.iter().copied().collect()),
                calls: RefCell::new(vec![]),
            }
        }
        fn run(&self, args: &[&str], bound: Duration) -> std::io::Result<Output> {
            assert!(!bound.is_zero() && bound <= REGISTRATION_DEADLINE);
            self.calls
                .borrow_mut()
                .push(args.iter().map(|s| s.to_string()).collect());
            let (verb, code) = self
                .replies
                .borrow_mut()
                .pop_front()
                .expect("unexpected command");
            assert_eq!(args[0], verb);
            Ok(output(code, &format!("fixture {verb} {code}")))
        }
        fn apply(&self) -> Result<(), AppError> {
            let run = |a: &[&str], b| self.run(a, b);
            let now = || self.time.get();
            let sleep = |d| self.time.set(self.time.get() + d);
            Control {
                run: &run,
                now: &now,
                sleep: &sleep,
            }
            .replace(
                "gui/501/test.named-store",
                Path::new("/fixture/named.plist"),
            )
        }
    }

    #[test]
    fn replacement_waits_for_absence_and_uses_only_its_own_target() {
        let f = Fixture::new(&[
            ("bootout", 0),
            ("print", 0),
            ("print", 0),
            ("print", 113),
            ("bootstrap", 0),
        ]);
        f.apply().unwrap();
        assert!(f.replies.borrow().is_empty());
        let calls = f.calls.borrow();
        for call in &calls[..4] {
            assert_eq!(call[1], "gui/501/test.named-store");
        }
        assert_eq!(calls[4], ["bootstrap", "gui/501", "/fixture/named.plist"]);
    }

    #[test]
    fn absent_registration_can_be_loaded() {
        let f = Fixture::new(&[("bootout", 113), ("print", 113), ("bootstrap", 0)]);
        f.apply().unwrap();
        assert!(f.replies.borrow().is_empty());
    }

    #[test]
    fn refused_removal_or_probe_never_reaches_bootstrap() {
        for replies in [vec![("bootout", 77)], vec![("bootout", 0), ("print", 77)]] {
            let f = Fixture::new(&replies);
            let error = f.apply().unwrap_err().to_string();
            assert!(
                error.contains("77") && error.contains("gui/501/test.named-store"),
                "{error}"
            );
            assert!(f.replies.borrow().is_empty());
        }
    }

    #[test]
    fn transient_bootstrap_five_waits_for_absence_before_retry() {
        let f = Fixture::new(&[
            ("bootout", 0),
            ("print", 113),
            ("bootstrap", 5),
            ("print", 0),
            ("print", 113),
            ("bootstrap", 0),
        ]);
        f.apply().unwrap();
        assert!(f.replies.borrow().is_empty());
    }

    #[test]
    fn other_bootstrap_errors_are_not_retried() {
        let f = Fixture::new(&[("bootout", 0), ("print", 113), ("bootstrap", 78)]);
        let error = f.apply().unwrap_err().to_string();
        assert!(
            error.contains("78") && error.contains("named.plist"),
            "{error}"
        );
    }

    #[test]
    fn retries_share_one_budget_and_retain_the_bootstrap_failure() {
        let start = Instant::now();
        let time = Cell::new(start);
        let count = Cell::new(0);
        let run = |args: &[&str], _: Duration| {
            count.set(count.get() + 1);
            Ok(output(
                match args[0] {
                    "print" => 113,
                    "bootstrap" => 5,
                    _ => 0,
                },
                "bootstrap I/O error",
            ))
        };
        let now = || time.get();
        let sleep = |d| time.set(time.get() + d);
        let error = Control {
            run: &run,
            now: &now,
            sleep: &sleep,
        }
        .replace("gui/501/test", Path::new("/x.plist"))
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("I/O error") && error.contains("deadline"),
            "{error}"
        );
        assert_eq!(time.get() - start, REGISTRATION_DEADLINE);
        assert!(count.get() <= 203);
    }

    #[test]
    fn transport_timeout_is_an_error_not_absence() {
        let run = |_: &[&str], _: Duration| {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "launchctl timed out",
            ))
        };
        let c = Control {
            run: &run,
            now: &Instant::now,
            sleep: &std::thread::sleep,
        };
        let error = c.registered("gui/501/test").unwrap_err().to_string();
        assert!(
            error.contains("timed out") && error.contains("gui/501/test"),
            "{error}"
        );
    }
}
