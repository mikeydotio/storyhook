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
//! 2. The launch probe is wired: the config names `./launch-probe.ts` as its
//!    `globalSetup`, the runner exports `E2E_PROJECT` ahead of the real
//!    `npx playwright test --project=` line and after `--list` (which runs no
//!    global setup), the probe resolves the engine from the project's own
//!    `use` rather than a hand-kept map, passes `launch` no deadline of its
//!    own, and refuses by name — `the_launch_probe_is_wired_into_every_project_run`,
//!    `the_launch_probe_derives_the_engine_and_invents_no_deadline`.
//!
//! Measured when this landed, on the toggle the story records
//! (`PLAYWRIGHT_BROWSERS_PATH=/nonexistent` versus the real cache): the
//! poisoned webkit project refused in 11 ms with zero tests attempted, exit 1;
//! the healthy one launched in 409 ms and ran.

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

// ---------------------------------------------------------------------------
// 2. The launch probe is wired into every project run
// ---------------------------------------------------------------------------

/// Byte offset of `needle` in `haystack`, failing with `what` rather than
/// returning `None` — an absent line is a finding.
fn offset_of(haystack: &str, needle: &str, what: &str) -> usize {
    haystack
        .find(needle)
        .unwrap_or_else(|| panic!("{what}: expected to find {needle:?}"))
}

#[test]
fn the_launch_probe_is_wired_into_every_project_run() {
    let config = read("e2e/playwright.config.ts");
    let runner = read("scripts/run-e2e.sh");

    assert_eq!(
        top_level_setting(&config, "globalSetup"),
        Some("\"./launch-probe.ts\""),
        "e2e/playwright.config.ts must name ./launch-probe.ts as its globalSetup; without it a \
         browser that cannot start fails every test in a project at Playwright's launch \
         timeout, one worker at a time, and reads as that many tree failures (SH-627)"
    );
    assert!(
        repo_root().join("e2e/launch-probe.ts").is_file(),
        "e2e/launch-probe.ts must exist: the config names it"
    );

    // The runner exports the selected project's name for the probe — after
    // the `--list` probe (no global setup runs there, so a filter selecting
    // nothing is answered without a launch) and before the real run. Since
    // SH-625 the probe is `scripts/e2e-selection.sh`'s `e2e_list_selection`,
    // which is where the `--list` flags themselves now live.
    let list = offset_of(&runner, "e2e_list_selection ", "scripts/run-e2e.sh");
    let export = offset_of(
        &runner,
        "export E2E_PROJECT=\"$project\"",
        "scripts/run-e2e.sh",
    );
    let real_run = offset_of(
        &runner,
        "npx playwright test --project=\"$project\" --output=",
        "scripts/run-e2e.sh",
    );
    assert!(
        list < export && export < real_run,
        "scripts/run-e2e.sh must export E2E_PROJECT after its `--list` probe and before the real \
         `npx playwright test --project=` invocation (list at {list}, export at {export}, run at \
         {real_run})"
    );
}

#[test]
fn the_launch_probe_derives_the_engine_and_invents_no_deadline() {
    let probe = read("e2e/launch-probe.ts");
    let code: String = probe
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !(trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with("/*"))
        })
        .collect::<Vec<_>>()
        .join("\n");

    // The engine comes from the resolved project, the way Playwright's own
    // `browserName` fixture resolves it — never from a project-name map that
    // a sixth project would silently miss (SH-136).
    assert!(
        code.contains("use.browserName ?? use.defaultBrowserType"),
        "e2e/launch-probe.ts must resolve the engine as `use.browserName ?? use.defaultBrowserType`"
    );
    assert!(
        !code.contains("\"mobile-webkit\"") && !code.contains("\"untrusted-origin-chromium\""),
        "e2e/launch-probe.ts must not carry a project-name-to-engine map"
    );

    // No deadline of its own: the ceiling is Playwright's launch default.
    // `timeout:` is the option key; the refusal message may name the concept.
    assert!(
        !code.contains("timeout:"),
        "e2e/launch-probe.ts must pass `launch` no `timeout:` — its ceiling derives from \
         Playwright's own launch default, never from a number chosen here (SH-394)"
    );

    // Refusals name the mechanism and the seam.
    for needle in [
        "E2E_PROJECT",
        "SH-627",
        "NO TEST RAN",
        "console.error(`launch-probe:",
    ] {
        assert!(
            code.contains(needle),
            "e2e/launch-probe.ts must contain {needle:?}: a refusal names its seam and its \
             story, and a probe nobody can see is the SH-306 shape"
        );
    }
}
