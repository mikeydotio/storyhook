//! Startup guard for environment variables that require fault injection.
//!
//! A release build contains neither the store crash points nor the daemon's
//! deliberate-panic seam. Silently accepting their environment variables
//! would turn an explicit instruction into a no-op (SH-534), so the binary
//! refuses before argument dispatch or any side effect. [`decide`] takes the
//! build capability as a plain value so tests can prove both feature branches;
//! [`crate::env::is_test_build`] remains the single source of that capability.

/// Environment variables whose instructions only a fault-injection build can honor.
pub const GUARDED_VARIABLES: [&str; 2] = ["STORYHOOK_FAULT", "STORYHOOK_TEST_PANIC"];

/// An unsupported request for fault-injection behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    variable: String,
}

impl Refusal {
    /// The environment variable this build cannot honor.
    #[must_use]
    pub fn variable(&self) -> &str {
        &self.variable
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "refusing to ignore {variable}: this binary was built without the \
             `fault-injection` feature, so it cannot honor the requested fault behavior. \
             Either unset {variable}, or rebuild with `cargo build --features fault-injection`.",
            variable = self.variable,
        )
    }
}

/// Decides whether a requested feature-gated environment variable is supported.
///
/// # Errors
///
/// Returns [`Refusal`] when the request cannot be honored by this build.
pub fn decide(requested: Option<&str>, feature_enabled: bool) -> Result<(), Refusal> {
    match (requested, feature_enabled) {
        (Some(variable), false) => Err(Refusal {
            variable: variable.to_string(),
        }),
        _ => Ok(()),
    }
}

/// Returns the first guarded variable present in this process's environment.
#[must_use]
pub fn requested_from_process() -> Option<&'static str> {
    GUARDED_VARIABLES
        .into_iter()
        .find(|variable| std::env::var_os(variable).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_decision_covers_both_feature_branches_and_every_guarded_variable() {
        assert_eq!(decide(None, false), Ok(()));
        for variable in GUARDED_VARIABLES {
            assert_eq!(decide(Some(variable), true), Ok(()), "{variable}");
            let refusal = decide(Some(variable), false).expect_err(variable);
            assert_eq!(refusal.variable(), variable);
            let message = refusal.to_string();
            assert!(message.contains(variable), "{message}");
            assert!(message.contains("fault-injection"), "{message}");
            assert!(message.contains("unset"), "{message}");
            assert!(message.contains("cargo build --features"), "{message}");
        }
    }

    #[test]
    fn process_gathering_treats_every_present_value_as_a_request() {
        struct Restore(Vec<(&'static str, Option<std::ffi::OsString>)>);

        impl Drop for Restore {
            fn drop(&mut self) {
                for (name, value) in self.0.drain(..) {
                    match value {
                        Some(value) => unsafe { std::env::set_var(name, value) },
                        None => unsafe { std::env::remove_var(name) },
                    }
                }
            }
        }

        let _restore = Restore(
            GUARDED_VARIABLES
                .into_iter()
                .map(|name| (name, std::env::var_os(name)))
                .collect(),
        );
        for name in GUARDED_VARIABLES {
            unsafe { std::env::remove_var(name) };
        }
        assert_eq!(requested_from_process(), None);

        unsafe { std::env::set_var(GUARDED_VARIABLES[0], "") };
        assert_eq!(requested_from_process(), Some(GUARDED_VARIABLES[0]));
        unsafe { std::env::remove_var(GUARDED_VARIABLES[0]) };

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;

            unsafe {
                std::env::set_var(
                    GUARDED_VARIABLES[1],
                    std::ffi::OsString::from_vec(vec![0xff]),
                )
            };
            assert_eq!(requested_from_process(), Some(GUARDED_VARIABLES[1]));
        }
    }
}
