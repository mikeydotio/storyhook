//! Whether a daemon listens on this machine's tailnet as well as on loopback.
//!
//! A daemon always binds loopback. When `tailscale` reports an address, it also
//! binds that one, which makes the dashboard reachable from every device on the
//! tailnet (`crate::daemon::serve`). That is the product's purpose for a
//! person's own daemon, and a hazard for a test's: a fixture daemon on the
//! tailnet serves a throwaway store to every peer for as long as it runs, and
//! longer if it leaks. `STORYHOOK_TAILNET=0` keeps a daemon on loopback, and
//! every test environment sets it.
//!
//! The switch only ever narrows. No value widens a daemon beyond loopback plus
//! the tailnet address `tailscale` itself reports.

use std::ffi::OsStr;

use crate::error::AppError;

/// Whether a daemon binds the tailnet interface beside loopback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailnetPolicy {
    /// Bind the tailnet address `tailscale` reports, when there is one. The
    /// default for a daemon nobody configured.
    Bind,
    /// Bind loopback only, and never ask `tailscale` for an address.
    LoopbackOnly,
}

impl TailnetPolicy {
    /// The environment variable that selects the policy.
    pub const VARIABLE: &'static str = "STORYHOOK_TAILNET";

    /// Reads the policy from the variable's value, if it has one.
    ///
    /// Absent, empty or `1` binds the tailnet; `0` keeps the daemon on
    /// loopback. Anything else is refused rather than read as either, because
    /// this is a security switch: an `off` or a `false` that silently left the
    /// tailnet bound would fail open, and nothing would say so.
    pub fn from_variable(value: Option<&OsStr>) -> Result<Self, AppError> {
        match value.map(OsStr::to_str) {
            None | Some(Some("" | "1")) => Ok(Self::Bind),
            Some(Some("0")) => Ok(Self::LoopbackOnly),
            Some(other) => Err(AppError::Usage(format!(
                "{}=`{}` is not a tailnet policy: use 0 to keep the daemon on \
                 loopback only, or 1 (the default) to also bind this machine's \
                 tailnet address",
                Self::VARIABLE,
                other.unwrap_or("<not valid UTF-8>")
            ))),
        }
    }

    /// The value that selects this policy, for a child that must inherit it.
    #[must_use]
    pub const fn as_env_value(self) -> &'static str {
        match self {
            Self::Bind => "1",
            Self::LoopbackOnly => "0",
        }
    }

    /// Whether a daemon under this policy probes for and binds the tailnet.
    #[must_use]
    pub const fn binds(self) -> bool {
        matches!(self, Self::Bind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tailnet_switch_disables_only_on_zero_and_refuses_anything_else() {
        let parse = |value: Option<&str>| TailnetPolicy::from_variable(value.map(OsStr::new));
        assert_eq!(parse(None).expect("unset"), TailnetPolicy::Bind);
        assert_eq!(parse(Some("")).expect("empty"), TailnetPolicy::Bind);
        assert_eq!(parse(Some("1")).expect("one"), TailnetPolicy::Bind);
        assert_eq!(parse(Some("0")).expect("zero"), TailnetPolicy::LoopbackOnly);
        for typo in ["off", "false", "no", " 0", "00", "2", "true"] {
            let error = parse(Some(typo)).expect_err(typo);
            assert!(
                matches!(&error, AppError::Usage(message) if message.contains(TailnetPolicy::VARIABLE)
                    && message.contains(typo)),
                "{typo}: {error:?}"
            );
        }
    }

    #[test]
    fn each_policy_round_trips_through_its_variable_value() {
        for policy in [TailnetPolicy::Bind, TailnetPolicy::LoopbackOnly] {
            let value = OsStr::new(policy.as_env_value());
            assert_eq!(
                TailnetPolicy::from_variable(Some(value)).expect("its own value"),
                policy
            );
        }
        assert!(TailnetPolicy::Bind.binds());
        assert!(!TailnetPolicy::LoopbackOnly.binds());
    }
}
