//! Pins the dashboard's mutation timeout to the daemon's own documented
//! worst case for a write (SH-312).
//!
//! `src/web_dashboard.html`'s `api()` used to give every mutation a flat 10s
//! before giving up — against a daemon whose event hooks are sanctioned to
//! run synchronously, *after* the commit, for up to
//! [`storyhook::event_hooks::HOOK_TIMEOUT_CEILING_SECS`] (60s). A slow but
//! successful write was reported to the user as `"request timed out"`, read
//! as a definite failure, and resubmitted — filing a duplicate story
//! (SH-310/SH-311). The fix raises the client's deadline and *derives* it
//! from the server's own ceiling rather than hand-copying a second 60,
//! because this repository has already been bitten three times by constants
//! that drifted apart with nothing to catch it (SH-136, and the scans in
//! `tests/store_isolation.rs` and `tests/release_targets.rs` that exist
//! because of it). This test is that catch: it fails if the server's ceiling
//! moves and the client's deadline does not move with it.

use std::path::PathBuf;

/// The repository root, which is this package's manifest directory: the root
/// package and the workspace root are the same crate here (see `Cargo.toml`'s
/// opening comment).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Reads a repository file, failing with the path rather than with `None` —
/// a missing dashboard file is a finding, not a reason to skip.
fn read(relative: &str) -> String {
    let path: PathBuf = repo_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
}

/// The margin over the server's own ceiling, in seconds: the zero-capacity
/// dispatcher rendezvous a request can queue behind (`src/daemon/serve.rs`)
/// and the process-wide write lock, both of which run *before* a hook's own
/// clock starts. Stated here, once, rather than left for the JS comment
/// alone to assert — this is the number this test actually checks against.
const MARGIN_SECS: u64 = 15;

/// The `var MUTATION_TIMEOUT_MS = intFromQuery("mutationTimeoutMs", <n>);`
/// declaration in `src/web_dashboard.html`, with `<n>` parsed out.
///
/// Plain string parsing rather than a regex dependency: the declaration is
/// one fixed shape this file controls, not untrusted input.
fn dashboard_mutation_timeout_ms() -> u64 {
    let html = read("src/web_dashboard.html");
    let marker = "intFromQuery(\"mutationTimeoutMs\", ";
    let after = html
        .split(marker)
        .nth(1)
        .unwrap_or_else(|| panic!("src/web_dashboard.html must declare MUTATION_TIMEOUT_MS via `{marker}<n>)`"));
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits
        .parse()
        .unwrap_or_else(|error| panic!("MUTATION_TIMEOUT_MS's value {digits:?} must parse as an integer: {error}"))
}

#[test]
fn the_dashboards_mutation_deadline_is_derived_from_the_hook_ceiling_not_hand_copied() {
    let expected_ms = (storyhook::event_hooks::HOOK_TIMEOUT_CEILING_SECS + MARGIN_SECS) * 1000;
    let actual_ms = dashboard_mutation_timeout_ms();
    assert_eq!(
        actual_ms, expected_ms,
        "src/web_dashboard.html's MUTATION_TIMEOUT_MS ({actual_ms}ms) has drifted from \
         storyhook::event_hooks::HOOK_TIMEOUT_CEILING_SECS + {MARGIN_SECS}s margin ({expected_ms}ms) — \
         raising the server's hook ceiling without raising the dashboard's mutation timeout to match \
         reopens the SH-310/SH-311 duplicate-story race this test exists to catch."
    );
}
