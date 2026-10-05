//! The process a test-owned storyhook run belongs to, and whether it is
//! still there.
//!
//! A test harness names itself in `STORYHOOK_PARENT_PID`, optionally with a
//! native start token in `STORYHOOK_PARENT_START_TIME`. A daemon started
//! under that name exits when its owner does (`serve::watch_parent`). That
//! alone left two gaps, and both let a fixture daemon outlive its test:
//!
//! - **A straggler started one after its owner had gone.** A helper or hook
//!   still running when its test ended ran `story`, which started a fresh
//!   daemon for the fixture's store and recreated the home the test had just
//!   deleted. That daemon bound its listeners before it ever checked its
//!   owner. A process whose owner has gone has no legitimate work, so
//!   [`ParentContract::still_here`] refuses to start a daemon for it.
//! - **A recycled pid kept a daemon alive.** Shell harnesses cannot read a
//!   native start token, so they export an empty one, and a pid-only watch
//!   follows whichever process reuses that pid. When the token is empty, the
//!   contract samples the owner's token while the owner is alive, and the
//!   daemon it starts watches that exact incarnation.
//!
//! Production names no owner, so every check here is a no-op there.

use crate::error::AppError;

/// The owner a storyhook process was started under, as this process found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParentContract {
    /// No owner was named. Production, and every environment a test builds in
    /// process: nothing watches, nothing is refused.
    Unwatched,
    /// The named owner was alive when this process resolved it. `start_time`
    /// pins one incarnation of `pid` when a token is known.
    Watching {
        /// The owner's pid.
        pid: u32,
        /// The owner's native start token, declared or sampled.
        start_time: Option<String>,
    },
    /// The named owner had already exited when this process resolved it.
    Gone {
        /// The pid the contract named.
        pid: u32,
    },
}

impl ParentContract {
    /// Resolves the contract this process inherited.
    #[must_use]
    pub fn from_process() -> Self {
        Self::resolve(
            super::lifecycle::parent_pid(),
            super::lifecycle::parent_start_time(),
            super::lifecycle::process_start_time,
            super::lifecycle::process_identity_is_live,
        )
    }

    /// Resolves a contract from a named `pid` and `declared` token, reading the
    /// process table through `sample` and `alive`.
    ///
    /// Without a declared token, the owner's own token is sampled, so the
    /// contract pins the incarnation that was alive now rather than any later
    /// process given the same pid. An owner that is not alive is
    /// [`Self::Gone`], whether its pid is free or reused by a different
    /// incarnation.
    pub fn resolve(
        pid: Option<u32>,
        declared: Option<String>,
        sample: impl Fn(u32) -> Option<String>,
        alive: impl Fn(u32, Option<&str>) -> bool,
    ) -> Self {
        let Some(pid) = pid else {
            return Self::Unwatched;
        };
        let start_time = declared.or_else(|| sample(pid));
        if alive(pid, start_time.as_deref()) {
            Self::Watching { pid, start_time }
        } else {
            Self::Gone { pid }
        }
    }

    /// Whether the owner is still the process this contract resolved.
    /// Always true when nothing is watched.
    #[must_use]
    pub fn is_live(&self) -> bool {
        match self {
            Self::Unwatched => true,
            Self::Watching { pid, start_time } => {
                super::lifecycle::process_identity_is_live(*pid, start_time.as_deref())
            }
            Self::Gone { .. } => false,
        }
    }

    /// Refuses when the owner has gone, before anything that would start a
    /// daemon or write into the owner's directories.
    pub fn still_here(&self) -> Result<(), AppError> {
        if self.is_live() {
            return Ok(());
        }
        let pid = match self {
            Self::Watching { pid, .. } | Self::Gone { pid } => *pid,
            Self::Unwatched => unreachable!("an unwatched contract is always live"),
        };
        Err(AppError::Usage(format!(
            "STORYHOOK_PARENT_PID names process {pid}, which has exited, so the run \
             this process belongs to is over. A storyhook daemon is not started for \
             it, because nothing would ever stop that daemon. Unset \
             STORYHOOK_PARENT_PID and STORYHOOK_PARENT_START_TIME to run outside a \
             test harness."
        )))
    }

    /// The start token a daemon started under this contract must watch, when
    /// one is known.
    #[must_use]
    pub fn start_time(&self) -> Option<&str> {
        match self {
            Self::Watching { start_time, .. } => start_time.as_deref(),
            Self::Unwatched | Self::Gone { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shape the contract resolves to, with the process table injected.
    #[test]
    fn a_parent_contract_resolves_each_shape() {
        let never = |_: u32| -> Option<String> { panic!("nothing is sampled without a pid") };
        assert_eq!(
            ParentContract::resolve(None, None, never, |_, _| true),
            ParentContract::Unwatched
        );

        // A declared token is used as given, and the owner must still match it.
        let unsampled = |_: u32| -> Option<String> { panic!("a declared token is not resampled") };
        assert_eq!(
            ParentContract::resolve(Some(7), Some("t1".into()), unsampled, |pid, token| {
                pid == 7 && token == Some("t1")
            }),
            ParentContract::Watching {
                pid: 7,
                start_time: Some("t1".into())
            }
        );
        assert_eq!(
            ParentContract::resolve(Some(7), Some("t1".into()), unsampled, |_, token| {
                token == Some("t2")
            }),
            ParentContract::Gone { pid: 7 },
            "a pid now held by another incarnation is gone"
        );

        // An empty declaration samples the live owner's token and pins it.
        assert_eq!(
            ParentContract::resolve(
                Some(7),
                None,
                |_| Some("sampled".into()),
                |_, token| token == Some("sampled")
            ),
            ParentContract::Watching {
                pid: 7,
                start_time: Some("sampled".into())
            }
        );
        // No token readable, but the pid is live: the pid-only contract remains.
        assert_eq!(
            ParentContract::resolve(Some(7), None, |_| None, |_, token| token.is_none()),
            ParentContract::Watching {
                pid: 7,
                start_time: None
            }
        );
        // Nothing alive at that pid.
        assert_eq!(
            ParentContract::resolve(Some(7), None, |_| None, |_, _| false),
            ParentContract::Gone { pid: 7 }
        );
    }

    #[test]
    fn only_a_gone_owner_is_refused_and_only_a_watched_one_has_a_token() {
        assert!(ParentContract::Unwatched.still_here().is_ok());
        assert_eq!(ParentContract::Unwatched.start_time(), None);

        let gone = ParentContract::Gone { pid: 4_000_000 };
        let refusal = gone.still_here().expect_err("a gone owner is refused");
        assert!(
            matches!(&refusal, AppError::Usage(message) if message.contains("STORYHOOK_PARENT_PID")
                && message.contains("4000000")),
            "{refusal:?}"
        );
        assert_eq!(gone.start_time(), None);

        let me = std::process::id();
        let token = super::super::lifecycle::process_start_time(me);
        let watching = ParentContract::Watching {
            pid: me,
            start_time: token.clone(),
        };
        assert!(watching.still_here().is_ok(), "this test process is alive");
        assert_eq!(watching.start_time(), token.as_deref());
    }
}
