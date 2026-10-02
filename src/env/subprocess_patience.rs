//! Opt-in patience for a test CLI and its daemon. Default builds refuse it.

use std::ffi::OsStr;
use std::time::Duration;

/// Process-boundary declaration, cleared by the fixture isolation contract.
pub(super) const VARIABLE: &str = "STORYHOOK_TEST_SUBPROCESS_PATIENCE_MS";

/// Parses a bounded floor without changing any production deadline.
pub(super) fn parse(value: Option<&OsStr>) -> Result<Option<Duration>, String> {
    let Some(value) = value else { return Ok(None) };
    if !cfg!(feature = "test-seam") {
        return Err(format!(
            "{VARIABLE} requires a binary built with --features test-seam"
        ));
    }
    let invalid =
        || format!("{VARIABLE} must be integer milliseconds in 1..=900000; got {value:?}");
    let raw = value.to_str().ok_or_else(invalid)?;
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    let milliseconds: u64 = raw.parse().map_err(|_| invalid())?;
    if !(1..=900_000).contains(&milliseconds) {
        return Err(invalid());
    }
    Ok(Some(Duration::from_millis(milliseconds)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_declaration_keeps_production_policy() {
        assert_eq!(parse(None), Ok(None));
    }

    #[cfg(feature = "test-seam")]
    #[test]
    fn explicit_patience_accepts_only_positive_milliseconds_within_the_ceiling() {
        for (raw, expected) in [("1", 1), ("3000", 3000), ("900000", 900000)] {
            assert_eq!(
                parse(Some(OsStr::new(raw))),
                Ok(Some(Duration::from_millis(expected)))
            );
        }
        for raw in [
            "",
            "0",
            "-1",
            "+3",
            "1.5",
            " 30",
            "30 ",
            "900001",
            "18446744073709551616",
        ] {
            let error = parse(Some(OsStr::new(raw))).expect_err(raw);
            assert!(error.contains(VARIABLE), "{error}");
            assert!(error.contains("1..=900000"), "{error}");
        }
    }

    #[cfg(not(feature = "test-seam"))]
    #[test]
    fn default_build_refuses_an_override_instead_of_silently_ignoring_it() {
        let error = parse(Some(OsStr::new("3000"))).unwrap_err();
        assert!(error.contains(VARIABLE), "{error}");
        assert!(error.contains("test-seam"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_non_unicode_declaration_is_an_error() {
        use std::os::unix::ffi::OsStrExt;
        assert!(parse(Some(OsStr::from_bytes(&[0xff]))).is_err());
    }
}
