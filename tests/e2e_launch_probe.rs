//! Fences what SH-627 settled about the browser tier.
//!
//! A council on that story (`story show SH-627`, never its own directory —
//! SH-363) ruled that the release tier does not launder a red: `retries: 0`
//! stays, no blind re-run, no receipt over a known-unfixed defect. Measurement
//! then showed that the night's "flake population" was the machine — a
//! WindowServer crash after which every `webkit.launch()` hung to Playwright's
//! default launch timeout, 180 s **per test** under `workers: 1`, so one dead
//! browser read as 45 tree failures over two and a quarter hours.
//!
//! Two things are pinned here, in the wiring sense `tests/dashboard_focus_
//! coverage.rs` established (SH-360): a call site exists, never that it reaches
//! the right pixel. The behaviour itself was proved by toggle and is recorded on
//! the story, because a mock of `webkit.launch` would validate the mock
//! (SH-263, SH-345).
//!
//! 1. The council's decision — `retries: 0` and `workers: 1` in
//!    `e2e/playwright.config.ts` — cannot be quietly revised into "the fix" —
//!    `the_release_tier_never_retries_and_runs_one_worker`.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Reads a repository file, failing with the path rather than `None` — a
/// missing file is a finding, not a reason to skip (the idiom of
/// `tests/e2e_browser_coverage.rs::read`).
fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
}

/// The value assigned to a top-level `key:` line of the Playwright config —
/// the line's own indentation is the discriminator (two spaces: inside
/// `defineConfig({`, outside any `projects:` block), the same narrowness
/// `scripts/run-e2e.sh::config_project_names` relies on for `name:`.
fn top_level_setting<'a>(config: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("  {key}: ");
    config
        .lines()
        .find_map(|line| line.strip_prefix(prefix.as_str()))
        .map(|rest| rest.trim_end_matches(',').trim())
}

// ---------------------------------------------------------------------------
// 1. The council's decision: no retries, one worker
// ---------------------------------------------------------------------------

#[test]
fn the_release_tier_never_retries_and_runs_one_worker() {
    let config = read("e2e/playwright.config.ts");

    assert_eq!(
        top_level_setting(&config, "retries"),
        Some("0"),
        "e2e/playwright.config.ts must keep `retries: 0`. A retry is a launder, not a fix: \
         a test that passes on its second attempt has hidden the failure the first attempt \
         found, and the release tier's job is to enumerate failures, not to survive them. \
         SH-627's council rejected a global retries:1 and every bounded re-run; if a red is \
         environmental, the launch probe (e2e/launch-probe.ts) is where that is said by name"
    );
    assert_eq!(
        top_level_setting(&config, "workers"),
        Some("1"),
        "e2e/playwright.config.ts must keep `workers: 1`: every project runs against one \
         daemon, one seed and one fake-tmux state (SH-335), and dispatch.spec.ts/engine.spec.ts \
         claim seeded stories for real — a second worker would race them, and SH-627's H3 \
         (sequential fixture leakage, not a race) is only a tractable question while it is one"
    );
}

#[test]
fn top_level_setting_reads_this_configs_own_shape() {
    let config = read("e2e/playwright.config.ts");

    // Positive control: a key that is definitely top-level and definitely
    // not a bare literal, so a scan that stopped seeing the file would fail
    // here rather than report the pins above vacuously (SH-364).
    assert_eq!(
        top_level_setting(&config, "fullyParallel"),
        Some("false"),
        "the top-level scan no longer sees e2e/playwright.config.ts's own `fullyParallel` line"
    );
    // A project-level `name:` is six spaces deep and must NOT be read as a
    // top-level setting — otherwise `retries` inside a project block could
    // satisfy the pin above.
    assert_eq!(
        top_level_setting(&config, "name"),
        None,
        "a six-space-indented project field must not read as a top-level setting"
    );
}
