//! The one place that formats `story --version`'s output.
//!
//! SH-406: two builds of this binary must never report the same version
//! string. [`CARGO_PKG_VERSION`] alone cannot promise that — `make install`
//! puts whatever `target/release/story` was just built onto `PATH` under
//! whatever `VERSION` already says, so a binary built before a schema
//! migration merged and one built after it both report the same bare semver
//! (the SH-404 incident). [`BUILD_ID`] is `build.rs`'s stamp of the tracked
//! git content the binary was built from — see that file's module doc for
//! the full design and why a semver bump was rejected. Two builds share
//! [`full`]'s output if and only if their tracked content is byte-identical.
//!
//! [`CARGO_PKG_VERSION`]: env!
use std::sync::LazyLock;

/// The build identity `build.rs` stamped this binary with, if any. `None`
/// when no `.git` was available to ask (a release tarball, a packaged
/// `cargo install` source) — the expected case for a published crate, not a
/// defect.
pub const BUILD_ID: Option<&str> = option_env!("STORYHOOK_BUILD_ID");

/// `story --version`'s exact output: `story <semver>`, plus
/// ` (build <id>)` when [`BUILD_ID`] is present.
///
/// `<semver>` is always [`CARGO_PKG_VERSION`] alone as its own whitespace
/// field — `scripts/release.sh` and `Makefile`'s `install` target both parse
/// it out with `awk '{print $2}'`, and that contract does not change.
///
/// [`CARGO_PKG_VERSION`]: env!
pub fn full() -> &'static str {
    static FULL: LazyLock<String> = LazyLock::new(|| match BUILD_ID {
        Some(id) => format!("story {} (build {id})", env!("CARGO_PKG_VERSION")),
        None => format!("story {}", env!("CARGO_PKG_VERSION")),
    });
    &FULL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_always_starts_with_story_and_the_bare_semver() {
        let expected_prefix = format!("story {}", env!("CARGO_PKG_VERSION"));
        assert!(
            full().starts_with(&expected_prefix),
            "full() = {:?} must start with {expected_prefix:?}",
            full()
        );
    }

    #[test]
    fn the_second_whitespace_field_is_the_bare_semver() {
        let field = full().split_whitespace().nth(1).expect("a second field");
        assert_eq!(
            field,
            env!("CARGO_PKG_VERSION"),
            "release.sh and Makefile's `awk '{{print $2}}'` depend on this"
        );
    }

    #[test]
    fn a_present_build_id_is_wrapped_in_parens_after_build() {
        if let Some(id) = BUILD_ID {
            let expected_suffix = format!(" (build {id})");
            assert!(
                full().ends_with(&expected_suffix),
                "full() = {:?} must end with {expected_suffix:?}",
                full()
            );
        }
    }
}
