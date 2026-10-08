//! Fences the browser-coverage invariants SH-335 and SH-348 introduce.
//!
//! `e2e/playwright.config.ts` now names six projects -- two engine pairs,
//! `chromium`/`webkit` (desktop) and `mobile-chromium`/`mobile-webkit`
//! (mobile, SH-348), plus SH-321's isolated untrusted-origin Chromium leg and
//! SH-762's fractional-width Gecko leg --
//! `Makefile`'s `e2e-install` installs the browsers those projects need, and a
//! handful of specs quarantine an assertion WebKit
//! cannot satisfy on an unconfigured machine
//! (`e2e/specs/support.ts::fullKeyboardAccess()`,
//! story SH-335 carries the verdict). Three ways for that to
//! silently rot, each pinned here in the style `tests/dashboard_mutation_
//! deadline.rs` and `tests/release_targets.rs` already established for a
//! cross-file constant: root from `env!("CARGO_MANIFEST_DIR")`, read the real
//! files, panic on an unrecognised shape rather than passing silently.
//!
//! 1. A project names a browser the Makefile never installs (or the reverse)
//!    -- `every_browser_the_config_names_is_installed_by_make_e2e_install`.
//! 2. A quarantine (`test.skip` gated on `browserName === "webkit"`) lands
//!    with no story naming why -- `every_webkit_quarantine_names_a_story`.
//! 3. Either engine pair's two projects stop selecting their spec files
//!    identically, quietly narrowing WebKit coverage relative to Chromium's,
//!    or `engine.spec.ts` stops being the one explicit exception shared by all
//!    four -- `the_two_projects_in_each_engine_pair_select_their_specs_the_same_way`.
//!    SH-321's special spec is excluded from both pairs and selected only by
//!    its own Chromium project -- `the_untrusted_origin_spec_has_one_project`.
//!    SH-762's `*.fractional.spec.ts` specs are likewise selected only by the
//!    Gecko project that lays out fractional widths --
//!    `the_fractional_width_specs_have_one_project`.
//! 4. A failed project stops the matrix or a later project's Playwright
//!    invocation erases its failure artifacts --
//!    `the_matrix_records_failures_continues_and_keeps_each_projects_artifacts`.
//! 5. The real-dispatch post-check selects exactly `dispatch.spec.ts`
//!    and `engine.spec.ts`, not another stubbed spec whose filename
//!    happens to contain dispatch --
//!    `the_real_dispatch_postcheck_matches_only_the_two_exact_specs`.
//! 6. Every Playwright project invocation creates its daemon, seed and
//!    fake-tmux state inside the per-project subshell --
//!    `each_project_invocation_owns_its_daemon_seed_and_fake_tmux_state`.
//! 7. The fake-tmux one-writer guard rejects concurrent dispatches without
//!    rejecting stop-now's deliberately overlapping `unclaim` --
//!    `the_fake_tmux_writer_guard_applies_only_to_dispatch`.
//! 8. Dashboard source documentation cannot repeat the obsolete claims that
//!    the suite drives only Chromium, both projects are Blink, or installation
//!    fetches no other engine --
//!    `dashboard_source_does_not_repeat_obsolete_chromium_only_claims`.
//! 9. An ambient reverse-proxy allowlist cannot leak into the browser harness
//!    and silently withdraw localhost-only handoff authority --
//!    `the_runner_neutralizes_an_ambient_proxy_allowlist_before_startup`.
//! 10. The real-daemon browser harness supplies executable fixtures for both
//!     dispatch providers before daemon startup, so availability is independent
//!     of tools installed on the host --
//!     `the_runner_provisions_both_dispatch_provider_commands_before_daemon_startup`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use storyhook_test_support::ChildGuard;

#[path = "support/e2e_subprocess.rs"]
mod e2e_subprocess;

/// A local utility process gets twice the operating-system process-start budget.
const UTILITY_DEADLINE: Duration =
    Duration::from_secs(2 * storyhook::daemon::lifecycle::SPAWN_DEADLINE.as_secs());

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Reads a repository file, failing with the path rather than `None` -- a
/// missing file is a finding, not a reason to skip (same idiom as
/// `tests/dashboard_mutation_deadline.rs::read`).
fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
}

// ---------------------------------------------------------------------------
// 1. Every browser the config names is installed by `make e2e-install`
// ---------------------------------------------------------------------------

/// Maps a Playwright device descriptor to the browser engine it drives.
///
/// Closed and explicit, the same idiom as `tests/release_targets.rs`'s
/// `matrix_substring_for`: a device this function has not been taught panics
/// rather than silently passing, because "the config and the Makefile agree"
/// is a claim this function can only make about devices it knows how to map.
fn engine_for_device(device: &str) -> &'static str {
    match device {
        "Desktop Chrome" => "chromium",
        "Desktop Safari" => "webkit",
        // `defaultBrowserType: "chromium"` with `isMobile`/`hasTouch` set --
        // Blink under mobile emulation, not a real device's own engine (see
        // the config's own comment on this project).
        "Pixel 7" => "chromium",
        // `defaultBrowserType: "webkit"` with `isMobile`/`hasTouch` set --
        // WebKit under mobile emulation, and the same `webkit` binary
        // `Desktop Safari` drives, which is why SH-348 needed no Makefile
        // change to add this device.
        "iPhone 15" => "webkit",
        // Gecko, driven only by SH-762's fractional-width project.
        "Desktop Firefox" => "firefox",
        other => panic!(
            "e2e/playwright.config.ts uses devices[\"{other}\"], which engine_for_device() has \
             not been taught to map to a browser engine -- teach it what engine that device \
             drives before this test can vouch for Makefile coverage of it"
        ),
    }
}

/// Every `devices["..."]` a project's `use:` block actually spreads, in the
/// order they appear. Anchored to the `use: { ...devices["..."] }` shape
/// specifically (not a bare `devices["..."]` anywhere in the file) so a
/// device named only in a comment -- as the `mobile-chromium` project's own
/// doc comment does, for `Pixel 7` -- is not double-counted or mistaken for
/// a project that spreads it.
fn configured_devices(config_text: &str) -> Vec<String> {
    let marker = "use: { ...devices[\"";
    let mut devices = Vec::new();
    let mut rest = config_text;
    while let Some(idx) = rest.find(marker) {
        let after = &rest[idx + marker.len()..];
        let end = after.find('"').unwrap_or_else(|| {
            panic!("a `{marker}` in e2e/playwright.config.ts never closes its quote")
        });
        devices.push(after[..end].to_string());
        rest = &after[end..];
    }
    devices
}

/// The browser names on the Makefile's `npx playwright install` line for
/// `e2e-install`, e.g. `["chromium", "webkit"]`.
fn installed_browsers(makefile_text: &str) -> Vec<String> {
    let marker = "npx playwright install --with-deps ";
    let line = makefile_text
        .lines()
        .find(|line| line.contains(marker))
        .unwrap_or_else(|| {
            panic!("Makefile has no `{marker}...` line -- e2e-install's install step moved or was reworded")
        });
    let after = line
        .split(marker)
        .nth(1)
        .expect("marker matched but split found nothing after it");
    after.split_whitespace().map(str::to_string).collect()
}

#[test]
fn every_browser_the_config_names_is_installed_by_make_e2e_install() {
    let config_text = read("e2e/playwright.config.ts");
    let makefile_text = read("Makefile");

    let devices = configured_devices(&config_text);
    assert!(
        devices.len() >= 2,
        "configured_devices() found only {} device(s) in e2e/playwright.config.ts: {devices:?} \
         -- either the config lost a project, or the `use: {{ ...devices[\"...\"] }}` pattern \
         this scan looks for has drifted from the file's actual shape",
        devices.len()
    );

    // `configured_devices()` only recognises the single-line `use: { \
    // ...devices["..."] }` shape every project in this file happens to use
    // today -- a project that spreads its descriptor across a MULTI-LINE
    // `use: {` block instead (to add an option beside it, say) is invisible
    // to that scan, and `devices.len() >= 2` above would keep passing while
    // vouching for one fewer project than the config actually has. Cross-
    // checking against `project_blocks()` -- a second, independent parser
    // anchored on the project's own `name:` line rather than its `use:`
    // line -- turns that silent blind spot into a loud one (SH-348).
    let blocks = project_blocks(&config_text);
    assert_eq!(
        devices.len(),
        blocks.len(),
        "e2e/playwright.config.ts declares {} project(s) ({:?}) but configured_devices() found \
         {} device spread(s) ({devices:?}) -- one project's `use: {{ ...devices[\"...\"] }}` is \
         not on a single line, and this scan cannot see it. Keep every project's device spread \
         on one line, or teach configured_devices() the new shape.",
        blocks.len(),
        blocks.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        devices.len(),
    );

    let mut required_engines: Vec<&'static str> = devices
        .iter()
        .map(|device| engine_for_device(device))
        .collect();
    required_engines.sort_unstable();
    required_engines.dedup();

    let mut installed = installed_browsers(&makefile_text);
    installed.sort();
    installed.dedup();

    assert_eq!(
        required_engines, installed,
        "e2e/playwright.config.ts's projects require engines {required_engines:?} (from devices \
         {devices:?}), but Makefile's `e2e-install` target installs {installed:?}. A project \
         naming an engine this target never installs fails every fresh machine with a \
         missing-browser error; an engine this target installs that no project names is dead \
         weight. Keep the two lists in sync (SH-335)."
    );
}

#[test]
fn engine_for_device_recognises_this_configs_own_devices_and_panics_on_an_unknown_one() {
    assert_eq!(engine_for_device("Desktop Chrome"), "chromium");
    assert_eq!(engine_for_device("Desktop Safari"), "webkit");
    assert_eq!(engine_for_device("Pixel 7"), "chromium");
    assert_eq!(engine_for_device("iPhone 15"), "webkit");
    assert_eq!(engine_for_device("Desktop Firefox"), "firefox");

    // Deliberately NOT a device Playwright ships -- this probe used to name
    // "iPhone 15", a real descriptor the config had not yet adopted, and
    // SH-348 then adopted exactly it: the moment it did, the must-panic
    // control would have silently become a must-NOT-panic one on the same
    // commit that taught engine_for_device() about it. A name no registry
    // will ever ship can't be adopted out from under this test the same way.
    let panicked =
        std::panic::catch_unwind(|| engine_for_device("Storyhook Handset 9000")).is_err();
    assert!(
        panicked,
        "engine_for_device() silently accepted a device it was never taught -- it must panic on \
         an unrecognised device, not guess"
    );
}

#[test]
fn dashboard_source_does_not_repeat_obsolete_chromium_only_claims() {
    let dashboard = read("src/web_dashboard.html");
    let normalized = dashboard.split_whitespace().collect::<Vec<_>>().join(" ");
    let stale_claims = [
        "suite drives only Chromium",
        "both projects are Blink",
        "e2e-install` installs chromium and nothing else",
    ];
    let offenders: Vec<&str> = stale_claims
        .into_iter()
        .filter(|claim| normalized.contains(claim))
        .collect();

    assert!(
        offenders.is_empty(),
        "src/web_dashboard.html repeats obsolete Chromium-only browser coverage claims: \
         {offenders:?}. The Playwright matrix drives Chromium and WebKit, plus one Gecko \
         fractional-width leg, and `make e2e-install` installs all three (SH-335/SH-374/SH-762)."
    );
}

// ---------------------------------------------------------------------------
// 2. Every WebKit quarantine names a story
// ---------------------------------------------------------------------------

/// Every tracked browser spec, paired with its contents -- same enumerator
/// shape as `tests/e2e_fixture_hygiene.rs::all_specs`, kept local rather than
/// shared across integration-test binaries (each `tests/*.rs` file compiles
/// as its own crate, so there is no `mod` to share it through without a
/// `tests/common/` module neither file currently needs).
fn all_specs(root: &Path) -> Vec<(String, String)> {
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z", "--", "e2e/specs/*.spec.ts"])
        .output()
        .expect("listing this repository's tracked browser specs");
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so this scan proved nothing: {}",
        String::from_utf8_lossy(&listed.stderr)
    );

    listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|path| {
            let relative = std::str::from_utf8(path).expect("a UTF-8 path").to_string();
            let text = std::fs::read_to_string(root.join(&relative))
                .unwrap_or_else(|e| panic!("reading {relative}: {e}"));
            (relative, text)
        })
        .collect()
}

/// Whether `window` -- the text starting at a `test.skip(` call, out to a
/// bounded number of characters -- gates on WebKit specifically. A skip
/// conditioned on something else entirely (a data shape, a feature flag) is
/// not the quarantine this test polices.
fn gates_on_webkit(window: &str) -> bool {
    window.contains("browserName") && window.contains("webkit")
}

/// Whether `window` names the story the quarantine is filed under.
fn names_a_story(window: &str) -> bool {
    let bytes = window.as_bytes();
    window.match_indices("SH-").any(|(idx, _)| {
        bytes[idx + 3..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count()
            > 0
    })
}

/// A hard ceiling on how far past a `test.skip(` call this scan will read if
/// the call's own closing `);` is never found -- a safety cap, not the
/// normal case. The normal case ends the window at the call's own close
/// (see below), so a `test title` string sitting a few lines later in the
/// same test body -- which routinely embeds an unrelated `SH-<n>` in its
/// fixture title, as most specs in this suite do -- is never mistaken for
/// this quarantine's own attribution.
const SKIP_WINDOW_CEILING_CHARS: usize = 400;

/// Every `test.skip(` call site across the tracked specs, paired with the
/// text of that call's own arguments -- everything up to the FIRST `);`
/// (a close-paren immediately followed by a semicolon) after the marker,
/// which ends the statement rather than any inner call within it. A
/// `browserName === "webkit" && !fullKeyboardAccess()` condition contains
/// its own `()`, but that close is followed by `,` or `&&`, never `;`, so it
/// does not end the scan early -- verified by the control test below against
/// a real call site with exactly this shape.
fn skip_call_windows(specs: &[(String, String)]) -> Vec<(String, String)> {
    let marker = "test.skip(";
    let statement_end = ");";
    let mut windows = Vec::new();
    for (relative, text) in specs {
        let mut search_from = 0usize;
        while let Some(rel_idx) = text[search_from..].find(marker) {
            let start = search_from + rel_idx + marker.len();
            let ceiling = (start + SKIP_WINDOW_CEILING_CHARS).min(text.len());
            let end = text[start..ceiling]
                .find(statement_end)
                .map(|offset| start + offset)
                .unwrap_or(ceiling);
            windows.push((relative.clone(), text[start..end].to_string()));
            search_from = start;
        }
    }
    windows
}

#[test]
fn every_webkit_quarantine_names_a_story() {
    let root = repo_root();
    let specs = all_specs(&root);
    assert!(
        specs.len() >= 20,
        "this scan is supposed to find every tracked browser spec, and it found {}: too few to \
         trust the pattern -- `git ls-files -- e2e/specs/*.spec.ts` may have drifted",
        specs.len()
    );

    let unattributed: Vec<String> = skip_call_windows(&specs)
        .into_iter()
        .filter(|(_, window)| gates_on_webkit(window) && !names_a_story(window))
        .map(|(relative, _)| relative)
        .collect();

    assert!(
        unattributed.is_empty(),
        "{unattributed:?} contain a `test.skip(...)` gated on `browserName === \"webkit\"` with \
         no `SH-<n>` anywhere in that call's own condition and reason arguments. A WebKit quarantine is a \
         declared debt, not a silent absence (SH-335's whole point) -- name the story it is \
         filed under in the skip's reason string, the way e2e/specs/overlay-modality.spec.ts and \
         e2e/specs/notification-contract.spec.ts already do for SH-335 itself."
    );
}

#[test]
fn the_webkit_quarantine_pattern_matches_what_it_claims_to() {
    // Positive: copied verbatim from e2e/specs/overlay-modality.spec.ts's own
    // quarantine, not invented.
    let real_quarantine = r#"
      browserName === "webkit" && !fullKeyboardAccess(),
      "WebKit's Tab order skips buttons/links unless AppleKeyboardUIMode>=2 (SH-335)",
    );
    "#;
    assert!(gates_on_webkit(real_quarantine));
    assert!(names_a_story(real_quarantine));

    // Negative: gates on webkit, but the reason names no story -- this is
    // exactly what the real test must catch.
    let unattributed = r#"
      browserName === "webkit",
      "flaky on webkit for now",
    );
    "#;
    assert!(gates_on_webkit(unattributed));
    assert!(!names_a_story(unattributed));

    // Negative: a skip that has nothing to do with WebKit at all.
    let unrelated = r#"
      !someFeatureFlag,
      "this feature isn't built yet",
    );
    "#;
    assert!(!gates_on_webkit(unrelated));
}

#[test]
fn skip_call_windows_stops_at_its_own_statement_not_the_next_tests_fixture_title() {
    // Reproduces the false negative this scan originally had: a fixed
    // character budget swallowed the NEXT test's own title, which routinely
    // embeds an unrelated `SH-<n>` (`"SH-168 status flags — ready card"`, a
    // real title from this suite), so an unattributed skip read as
    // attributed. The window must end at the skip call's own `);`, before
    // ever reaching that title.
    let file = r#"
test.skip(
  browserName === "webkit",
  "flaky on webkit for now",
);

test("a ready card carries no flag badge on the board", async ({ page }) => {
  const title = "SH-168 status flags — ready card";
});
"#
    .to_string();

    let windows = skip_call_windows(&[("fixture.spec.ts".to_string(), file)]);
    assert_eq!(
        windows.len(),
        1,
        "expected exactly one test.skip( call site"
    );
    let (_, window) = &windows[0];

    assert!(
        gates_on_webkit(window),
        "the window must still see its own condition"
    );
    assert!(
        !names_a_story(window),
        "the window leaked past its own `);` into the next test's fixture title, which is the \
         exact false negative this test exists to catch: {window:?}"
    );
}

// ---------------------------------------------------------------------------
// 3. Each engine pair selects its specs the same way
// ---------------------------------------------------------------------------

/// A Playwright project's declared name, paired with the text of its own
/// `{ ... }` block -- everything up to (but not including) the next
/// project's `      name: "` anchor, or the end of the file for the last
/// project. The anchor is the exact six-space-indented shape `scripts/
/// run-e2e.sh`'s own `config_project_names()` parses -- documented there as
/// deliberately narrow, verified against the current file, which has no
/// other line at that indentation containing `name:`.
fn project_blocks(config_text: &str) -> Vec<(String, String)> {
    const ANCHOR: &str = "      name: \"";
    let mut blocks = Vec::new();
    let anchor_positions: Vec<usize> = config_text
        .match_indices(ANCHOR)
        .map(|(idx, _)| idx)
        .collect();

    for (i, &start) in anchor_positions.iter().enumerate() {
        let name_start = start + ANCHOR.len();
        let name_end = config_text[name_start..]
            .find('"')
            .map(|offset| name_start + offset)
            .unwrap_or_else(|| panic!("a `{ANCHOR}` never closes its quote"));
        let name = config_text[name_start..name_end].to_string();

        let body_end = anchor_positions
            .get(i + 1)
            .copied()
            .unwrap_or(config_text.len());
        let body = config_text[name_end..body_end].to_string();
        blocks.push((name, body));
    }
    blocks
}

/// A project block's spec-selection clause: which of `testIgnore`/`testMatch`
/// it uses, and the bare expression it's set to (e.g. `MOBILE_SPECS`).
fn selector(block: &str) -> Option<(&'static str, String)> {
    for (kind, marker) in [("testIgnore", "testIgnore:"), ("testMatch", "testMatch:")] {
        if let Some(idx) = block.find(marker) {
            let after = &block[idx + marker.len()..];
            let end = after.find([',', '\n']).unwrap_or(after.len());
            return Some((kind, after[..end].trim().to_string()));
        }
    }
    None
}

/// The config's two engine pairs, as `(pair label, first project, second
/// project, expected selector kind)`. Within a pair, both members must
/// select identically, or one engine's coverage can be narrowed without
/// either project's own file saying so (SH-335 established this for the
/// desktop pair; SH-348 extends it to the mobile pair). Across the pairs,
/// the selector KINDS remain opposite. The mobile pair's expression adds the
/// intentional cross-class files (`engine.spec.ts`, `open-pr-chip.spec.ts`,
/// and `verification-layout.spec.ts`) to `MOBILE_SPECS`;
/// the desktop pair additionally excludes SH-321's dedicated untrusted-origin
/// spec, whose own project is pinned below.
const ENGINE_PAIRS: [(&str, &str, &str, &str); 2] = [
    ("desktop", "chromium", "webkit", "testIgnore"),
    ("mobile", "mobile-chromium", "mobile-webkit", "testMatch"),
];

#[test]
fn the_two_projects_in_each_engine_pair_select_their_specs_the_same_way() {
    let config_text = read("e2e/playwright.config.ts");
    let blocks = project_blocks(&config_text);

    let by_name = |wanted: &str| -> &str {
        blocks
            .iter()
            .find(|(name, _)| name == wanted)
            .unwrap_or_else(|| {
                panic!(
                    "e2e/playwright.config.ts names no \"{wanted}\" project -- \
                     project_blocks() found {:?}",
                    blocks.iter().map(|(n, _)| n).collect::<Vec<_>>()
                )
            })
            .1
            .as_str()
    };

    let mut pair_selectors = Vec::new();
    for (label, first, second, expected_kind) in ENGINE_PAIRS {
        let first_selector = selector(by_name(first)).unwrap_or_else(|| {
            panic!("the \"{first}\" project declares neither testIgnore nor testMatch")
        });
        let second_selector = selector(by_name(second)).unwrap_or_else(|| {
            panic!("the \"{second}\" project declares neither testIgnore nor testMatch")
        });

        assert_eq!(
            first_selector, second_selector,
            "\"{first}\" selects its specs with {first_selector:?} and \"{second}\" with \
             {second_selector:?} -- these two projects are supposed to run the identical {label} \
             spec set (every {label} spec runs on both engines, nothing hand-listed per engine), \
             so a divergence here silently narrows one engine's coverage relative to the other's \
             without either project's own file saying so. Point both at the same \
             testIgnore/testMatch expression."
        );
        assert_eq!(
            first_selector.0, expected_kind,
            "the {label} pair (\"{first}\"/\"{second}\") selects with {:?}, not {expected_kind:?}",
            first_selector.0
        );
        pair_selectors.push((label, first_selector));
    }

    let (_, (_, desktop_selector)) = &pair_selectors[0];
    let (_, (_, mobile_selector)) = &pair_selectors[1];
    assert_eq!(
        desktop_selector.as_str(),
        "DESKTOP_EXCLUDED_SPECS",
        "the desktop pair must exclude phone-subject specs and both dedicated-project partitions"
    );
    assert_eq!(
        mobile_selector.as_str(),
        "MOBILE_OR_ENGINE_SPECS",
        "the mobile pair must share the named cross-device specs and phone set"
    );
    assert!(
        config_text.contains("const ENGINE_SPECS = /engine\\.spec\\.ts$/;")
            && config_text.contains("const OPEN_PR_CHIP_SPECS = /open-pr-chip\\.spec\\.ts$/;")
            && config_text.contains("const VERIFICATION_LAYOUT_SPECS = /verification-layout\\.spec\\.ts$/;")
            && config_text.contains(
                "const MOBILE_OR_ENGINE_SPECS = [MOBILE_SPECS, ENGINE_SPECS, OPEN_PR_CHIP_SPECS, VERIFICATION_LAYOUT_SPECS];"
            ),
        "engine.spec.ts, open-pr-chip.spec.ts, and verification-layout.spec.ts must be the explicit shared exceptions, \
         composed from the same MOBILE_SPECS constant for both mobile engines"
    );
}

#[test]
fn project_blocks_and_selector_read_this_configs_own_shape() {
    let config_text = read("e2e/playwright.config.ts");
    let blocks = project_blocks(&config_text);
    let names: Vec<&str> = blocks.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "chromium",
            "webkit",
            "mobile-chromium",
            "mobile-webkit",
            "untrusted-origin-chromium",
            "fractional-firefox",
            "node",
        ],
        "project_blocks() parsed {names:?} out of the live config -- either a project was \
         added/removed/reordered, or the parser's anchor no longer matches the file's shape"
    );

    // Control fixture, independent of the real file: two synthetic project
    // blocks proving `selector()` reads both shapes correctly.
    let ignore_block = r#"",
      use: { ...devices["Desktop Chrome"] },
      testIgnore: MOBILE_SPECS,
    },"#;
    assert_eq!(
        selector(ignore_block),
        Some(("testIgnore", "MOBILE_SPECS".to_string()))
    );

    let match_block = r#"",
      use: { ...devices["Pixel 7"] },
      testMatch: MOBILE_OR_ENGINE_SPECS,
    },"#;
    assert_eq!(
        selector(match_block),
        Some(("testMatch", "MOBILE_OR_ENGINE_SPECS".to_string()))
    );
}

#[test]
fn the_untrusted_origin_spec_has_one_project() {
    let config_text = read("e2e/playwright.config.ts");
    let blocks = project_blocks(&config_text);
    let special = blocks
        .iter()
        .find(|(name, _)| name == "untrusted-origin-chromium")
        .unwrap_or_else(|| panic!("the config names no untrusted-origin-chromium project"));

    assert_eq!(
        selector(&special.1),
        Some(("testMatch", "UNTRUSTED_ORIGIN_SPECS".to_string())),
        "the untrusted-origin project must select only its dedicated spec"
    );
    assert!(
        config_text
            .contains("const UNTRUSTED_ORIGIN_SPECS = /untrusted-origin-cookie\\.spec\\.ts$/;")
            && config_text.contains(DESKTOP_EXCLUDED_SPECS_LINE),
        "the dedicated spec must be excluded from the ordinary desktop pair and selected from \
         one shared expression"
    );
    assert!(
        special
            .1
            .contains("--host-resolver-rules=MAP ${UNTRUSTED_ORIGIN_HOST} 127.0.0.1"),
        "the special Chromium project must map the fake hostname to loopback in the browser"
    );
}

/// The desktop pair's one exclusion list: phone-subject specs plus the three
/// partitions that each belong to a single dedicated project.
const DESKTOP_EXCLUDED_SPECS_LINE: &str = "const DESKTOP_EXCLUDED_SPECS = [MOBILE_SPECS, UNTRUSTED_ORIGIN_SPECS, FRACTIONAL_SPECS, NODE_SPECS];";

/// SH-762: a layout width strictly between two whole CSS pixels is what
/// exposed the gap between `(min-width: 769px)` and `(max-width: 768px)`, and
/// only Gecko at `layout.css.devPixelsPerPx` 1.1 produces one in this suite.
/// The `*.fractional.spec.ts` specs assert that precondition, so any other
/// project selecting them would fail every run; the Gecko project must carry
/// the preference, or the specs lose the band they exist to measure.
#[test]
fn the_fractional_width_specs_have_one_project() {
    let config_text = read("e2e/playwright.config.ts");
    let blocks = project_blocks(&config_text);
    assert!(
        config_text.contains("const FRACTIONAL_SPECS = /\\.fractional\\.spec\\.ts$/;")
            && config_text.contains(DESKTOP_EXCLUDED_SPECS_LINE),
        "the fractional-width specs must be one named pattern that the desktop pair excludes"
    );

    let selecting: Vec<&str> = blocks
        .iter()
        .filter(|(_, body)| body.contains("FRACTIONAL_SPECS"))
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(
        selecting,
        vec!["fractional-firefox"],
        "only the fractional-firefox project may name the fractional-width pattern (the desktop \
         pair reaches it through DESKTOP_EXCLUDED_SPECS, pinned above); found {selecting:?}"
    );

    let gecko = &blocks
        .iter()
        .find(|(name, _)| name == "fractional-firefox")
        .unwrap_or_else(|| panic!("the config names no fractional-firefox project"))
        .1;
    assert_eq!(
        selector(gecko),
        Some(("testMatch", "FRACTIONAL_SPECS".to_string())),
        "the fractional-firefox project must select only the fractional-width specs"
    );
    assert!(
        gecko.contains(
            "use: { ...devices[\"Desktop Firefox\"], launchOptions: { firefoxUserPrefs: { \"layout.css.devPixelsPerPx\": \"1.1\" } } },"
        ),
        "the fractional-firefox project must drive Gecko at layout.css.devPixelsPerPx 1.1, the \
         preference that makes its viewport widths fractional"
    );
}

/// SH-792 council verdict: a spec that never reaches a browser runs once, in
/// the engine-free `node` project, instead of once per desktop engine. The
/// project must refuse every browser launch -- the runtime half of the
/// partition's guard -- and the launch probe must skip it rather than launch
/// the engine it refuses.
#[test]
fn the_page_less_specs_have_one_engine_free_project() {
    let config_text = read("e2e/playwright.config.ts");
    let blocks = project_blocks(&config_text);
    assert!(
        config_text.contains("const NODE_SPECS = /\\.node\\.spec\\.ts$/;")
            && config_text.contains(DESKTOP_EXCLUDED_SPECS_LINE),
        "the page-less specs must be one named pattern that the desktop pair excludes"
    );
    let selecting: Vec<&str> = blocks
        .iter()
        .filter(|(_, body)| body.contains("NODE_SPECS"))
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(
        selecting,
        vec!["node"],
        "only the node project may name the page-less pattern; found {selecting:?}"
    );
    let node = &blocks
        .iter()
        .find(|(name, _)| name == "node")
        .unwrap_or_else(|| panic!("the config names no node project"))
        .1;
    assert_eq!(
        selector(node),
        Some(("testMatch", "NODE_SPECS".to_string()))
    );
    assert!(
        node.contains("launchOptions: { executablePath: ENGINE_FREE_EXECUTABLE } },")
            && node.contains("metadata: { engineFree: true },"),
        "the node project must launch no browser (a nonexistent executable) and say so in its \
         metadata"
    );
    assert!(
        config_text.contains(
            "const ENGINE_FREE_EXECUTABLE = \"/engine-free-project/node-specs-must-not-launch-a-browser\";"
        ),
        "the refusal must name why it refuses in the launch error it produces"
    );
    let probe = read("e2e/launch-probe.ts");
    let skip = probe
        .find("?.engineFree) {")
        .expect("the launch probe skips an engine-free project");
    let launch = probe
        .find("playwright[engine].launch(")
        .expect("the launch probe launches");
    assert!(skip < launch, "the skip comes before any launch");
}

/// Browser fixtures a spec can ask Playwright for.
const BROWSER_FIXTURES: [&str; 4] = ["page", "context", "browser", "browserName"];

/// Whether a spec's source destructures a browser fixture in any `({ … })`
/// parameter list -- a test, a hook, or a fixture. Coarse on purpose: an
/// object literal that happens to name `page` reads as a request, which can
/// only keep a page-less file in the desktop pair (a wasted duplicate run),
/// never push a browser-driving file out of it.
fn requests_a_browser_fixture(source: &str) -> bool {
    let mut rest = source;
    while let Some(at) = rest.find("({") {
        let after = &rest[at + 2..];
        let Some(end) = after.find('}') else { break };
        if after[..end]
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .any(|word| BROWSER_FIXTURES.contains(&word))
        {
            return true;
        }
        rest = &after[end..];
    }
    false
}

#[test]
fn the_browser_fixture_scanner_reads_the_shapes_specs_use() {
    for requesting in [
        "test(\"t\", async ({ page }) => {});",
        "test.beforeEach(async ({ page, request }) => {});",
        "test(\"t\", async ({ context }) => {});",
        "test(\"t\", async ({ browserName, page: p }) => {});",
        "base.extend({ x: async ({ browser }, use) => {} });",
    ] {
        assert!(requests_a_browser_fixture(requesting), "{requesting}");
    }
    for page_less in [
        "test(\"t\", async () => { expect(parse(\"#fff\")).toBe(1); });",
        "test(\"t\", async ({}, testInfo) => {});",
        "test(\"t\", async ({ request }) => {});",
        "const pageSize = 3; test(\"paging\", () => {});",
    ] {
        assert!(!requests_a_browser_fixture(page_less), "{page_less}");
    }
}

/// The static half of the page-less partition's guard (SH-792 council): a
/// desktop-pair spec that requests no browser fixture runs byte-identically on
/// both engines, so it belongs in `*.node.spec.ts`. Membership is derived from
/// each file's own fixture use, never from a list.
#[test]
fn a_desktop_pair_spec_that_requests_no_browser_is_a_node_spec() {
    let specs_dir = repo_root().join("e2e/specs");
    let mut page_less = Vec::new();
    let mut node_specs = 0;
    for entry in std::fs::read_dir(&specs_dir).expect("e2e/specs is readable") {
        let path = entry.expect("a spec entry").path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".spec.ts") {
            continue;
        }
        if name.ends_with(".node.spec.ts") {
            node_specs += 1;
            continue;
        }
        let desktop_pair = !(name.ends_with(".mobile.spec.ts")
            || name.ends_with(".fractional.spec.ts")
            || name == "untrusted-origin-cookie.spec.ts");
        if desktop_pair {
            let source = std::fs::read_to_string(&path).expect("a readable spec");
            if !requests_a_browser_fixture(&source) {
                page_less.push(name);
            }
        }
    }
    assert!(node_specs >= 1, "the node partition exists");
    assert!(
        page_less.is_empty(),
        "these desktop specs request no page, context or browser, so a second engine runs \
         them identically: rename each to *.node.spec.ts so the engine-free project runs it \
         once: {page_less:?}"
    );
}

// ---------------------------------------------------------------------------
// 4. A continued matrix preserves every slice's failure evidence
// ---------------------------------------------------------------------------

#[test]
fn the_matrix_records_failures_continues_and_keeps_each_slices_artifacts() {
    let runner = read("scripts/run-e2e.sh");

    // The pool itself runs every slice after a failure and reports only once
    // all have run (tests/e2e_pool.rs); this is the runner handing it all.
    assert!(
        runner.contains(". \"$repo_root/scripts/e2e-pool.sh\"")
            && runner.contains("e2e_pool_run \"$e2e_jobs\" \"$E2E_STOP_GRACE_SECONDS\" run_slice ")
            && runner.contains("|| overall_status=$?")
            && runner.contains("exit \"$overall_status\""),
        "every slice must run through scripts/e2e-pool.sh, which continues past a failed \
         slice, and the runner must report failure only after every slice had its turn"
    );
    assert!(
        runner.contains(
            "results_root=\"${STORYHOOK_E2E_RESULTS_DIR-$repo_root/e2e/test-results/current}\""
        ),
        "scripts/run-e2e.sh must establish one artifact root for the whole invocation; without \
         it a later Playwright project can clear the earlier project's screenshots and traces"
    );
    assert!(
        runner.contains("--output=\"$results_root/$slice\""),
        "each Playwright invocation must write beneath a slice-keyed output directory: \
         Playwright clears its output directory at the start of every invocation, so two \
         slices of one project sharing one would erase each other's evidence"
    );
}

// ---------------------------------------------------------------------------
// 5. The real-dispatch post-check selects exactly two specs
// ---------------------------------------------------------------------------

fn dispatch_count(input: &str) -> usize {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut command = Command::new("/bin/bash");
    command
        .args([
            "-c",
            ". \"$1\"; e2e_selection_real_dispatch",
            "dispatch-coverage",
        ])
        .arg(repo_root().join("scripts/e2e-selection.sh"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child = ChildGuard::spawn_with_output(&mut command)
        .expect("spawning the dispatch selector scripts/run-e2e.sh uses");
    child
        .stdin()
        .expect("selector stdin was piped")
        .write_all(input.as_bytes())
        .expect("writing root-relative Playwright file counts to the selector");
    let output = child.wait_with_output_within(
        storyhook_test_support::load_grace::graced_now(UTILITY_DEADLINE),
        || "the dispatch coverage probe did not finish".to_string(),
    );
    assert!(
        output.status.success(),
        "selector failed unexpectedly: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("selector output is UTF-8")
        .trim()
        .parse()
        .expect("selector output is a count")
}

#[test]
fn the_real_dispatch_postcheck_matches_only_the_two_exact_specs() {
    let runner = read("scripts/run-e2e.sh");
    assert!(runner.lines().any(
        |line| line.trim_start().starts_with("real_dispatch_selected=")
            && line.contains("e2e_selection_real_dispatch")
    ));
    let dispatch = "chromium\tdispatch.spec.ts\t2\n";
    let engine = "mobile-webkit\tengine.spec.ts\t2\n";
    let stubbed = "chromium\tstory-context-menu-dispatch.spec.ts\t2\n";

    assert_eq!(
        dispatch_count(dispatch),
        1,
        "the post-check must recognize dispatch.spec.ts"
    );
    assert_eq!(
        dispatch_count(engine),
        1,
        "the post-check must recognize engine.spec.ts"
    );
    assert_eq!(
        dispatch_count(stubbed),
        0,
        "the post-check must not mistake the stubbed context-menu spec for dispatch.spec.ts"
    );
    assert_eq!(
        dispatch_count(&format!("{stubbed}{dispatch}{engine}")),
        2,
        "a mixed Playwright list must count only the two exact real-dispatch specs"
    );
}

// ---------------------------------------------------------------------------
// 6. Every project invocation owns the state that real dispatch mutates
// ---------------------------------------------------------------------------

#[test]
fn each_project_invocation_owns_its_daemon_seed_and_fake_tmux_state() {
    let runner = read("scripts/run-e2e.sh");
    let body = runner
        .split_once("run_one_project() {")
        .expect("scripts/run-e2e.sh must define run_one_project")
        .1
        .split_once("\n# --- Decide:")
        .expect("scripts/run-e2e.sh must end run_one_project before its outer project selection")
        .0;

    for required in [
        "data_root=\"$(mktemp -d /private/tmp/story-e2e.XXXXXX)\"",
        "export FAKE_TMUX_STATE=\"$data_root/faketmux\"",
        "seed_dir=\"$data_root/seed\"",
        "start_output=\"$(\"$story_bin\" daemon start 2>&1)\"",
        "export DASHBOARD_ENGINE_STORY_ID=\"$engine_story_id\"",
    ] {
        assert!(
            body.contains(required),
            "run_one_project must own `{required}` inside its per-project subshell"
        );
    }
    assert!(
        runner
            .matches("run_one_project \"${slice_projects[$i]}\" \"$1\" \"${slice_lists[$i]}\"")
            .count()
            == 2
            && runner.matches("run_one_project \"").count() == 2,
        "every slice, of the full matrix or of an explicit --project, must enter the same \
         isolated runner: one ordinary call or one isolation call with retained logs"
    );
}

/// A signalled slice's cleanup stops its daemon and removes its seed. The
/// pool sends TERM to a slice's shells after its writers (SH-792); a second
/// signal arriving mid-cleanup must not abandon that half-done.
#[test]
fn a_slices_cleanup_cannot_be_cut_short_by_a_second_signal() {
    let runner = read("scripts/run-e2e.sh");
    let cleanup = runner
        .split_once("  cleanup() {")
        .expect("run_one_project defines cleanup")
        .1
        .split_once("\n  }\n")
        .expect("cleanup ends")
        .0;
    let immune = cleanup
        .find("trap '' TERM INT HUP")
        .expect("cleanup ignores TERM, INT and HUP while it runs");
    let stop = cleanup
        .find("\"$story_bin\" daemon stop")
        .expect("cleanup stops the daemon");
    assert!(
        immune < stop,
        "the signals are ignored before the daemon stop starts"
    );
}

/// Every `story` call in the seeding auto-starts this run's daemon, well
/// before the explicit `daemon start`. A slice stopped while it seeds must
/// still stop that daemon before removing its root: left running, the
/// daemon lives until the outer script exits and then recreates its state
/// directory inside the removed root on the way out (found by SH-792's
/// mid-run TERM check, which left `home/.local/state` behind). The one
/// condition is the isolation itself: before `storyhook_isolate`, a `daemon
/// stop` would reach the developer's real store.
#[test]
fn a_slices_cleanup_stops_its_daemon_whenever_its_store_is_isolated() {
    let runner = read("scripts/run-e2e.sh");
    let cleanup = runner
        .split_once("  cleanup() {")
        .expect("run_one_project defines cleanup")
        .1
        .split_once("\n  }\n")
        .expect("cleanup ends")
        .0;
    let guard = cleanup
        .find("if [ \"$isolated\" = \"1\" ]; then")
        .expect("cleanup stops the daemon only once the store is isolated");
    let stop = cleanup
        .find("\"$story_bin\" daemon stop")
        .expect("cleanup stops the daemon");
    let remove = cleanup
        .find("rm -rf \"$data_root\"")
        .expect("cleanup removes the data root");
    assert!(
        guard < stop && stop < remove,
        "guard, stop, then removal: the daemon is stopped before its root goes"
    );
    assert!(
        !cleanup.contains("daemon_started"),
        "the stop must not wait for the explicit start: a daemon auto-started during \
         seeding exists before any start is recorded"
    );

    let body = runner
        .split_once("run_one_project() {")
        .expect("run-e2e.sh defines run_one_project")
        .1;
    let isolate = body
        .find("  storyhook_isolate \"$data_root\"\n  isolated=1\n")
        .expect("the flag is raised on the line after the isolation, and nowhere else");
    assert_eq!(body.matches("isolated=1").count(), 1);
    let first_story_call = body
        .find("\"$story_bin\" project new")
        .expect("the seeding calls story");
    assert!(
        isolate < first_story_call,
        "the store is isolated before the first story call can start a daemon"
    );
}

// ---------------------------------------------------------------------------
// 7. Stop-now's unclaim is not a second dispatch writer
// ---------------------------------------------------------------------------

#[test]
fn the_fake_tmux_writer_guard_applies_only_to_dispatch() {
    let runner = read("scripts/run-e2e.sh");
    let wrapper = runner
        .split_once("cat >\"$STORYHOOK_DISPATCH_SCRIPT\" <<WRAPPER")
        .expect("scripts/run-e2e.sh must generate its dispatch wrapper")
        .1
        .split_once("\nWRAPPER")
        .expect("the generated dispatch wrapper must terminate its heredoc")
        .0;

    assert!(
        wrapper.contains("_helper_verb=\"\"")
            && wrapper.contains("--project) _expect_project=true ;;")
            && wrapper.contains(r#"*) _helper_verb="\$_arg"; break ;;"#),
        "the generated wrapper must parse the helper verb without mistaking --project's value \n\
         for the command"
    );
    assert!(
        !wrapper.contains('`'),
        "the generated wrapper's unquoted heredoc must contain no backticks; they execute as \n\
         command substitutions while the wrapper is being written"
    );

    let gated = wrapper
        .split_once(r#"if [ "\$_helper_verb" = dispatch ]; then"#)
        .expect("the fake-tmux writer guard must be explicitly dispatch-only")
        .1
        .split_once("\nfi\n\n# Keep the registered leader")
        .expect("the dispatch-only guard must close before the supervised helper")
        .0;
    for required in [
        r#"_holders="\$FAKE_TMUX_STATE/holders""#,
        r#"kill -0 "\$_pid""#,
        r#"printf '%s\n' "\$\$" >"\$_holders""#,
    ] {
        assert!(
            gated.contains(required),
            "the dispatch-only guard must retain `{required}`"
        );
    }
}

// ---------------------------------------------------------------------------
// 9. The browser harness owns the proxy-allowlist decision
// ---------------------------------------------------------------------------

#[test]
fn the_runner_neutralizes_an_ambient_proxy_allowlist_before_startup() {
    let runner = read("scripts/run-e2e.sh");
    let body = runner
        .split_once("run_one_project() {")
        .expect("scripts/run-e2e.sh must define run_one_project")
        .1
        .split_once("\n# --- Decide:")
        .expect("scripts/run-e2e.sh must end run_one_project before its outer project selection")
        .0;

    let isolated = body
        .find("storyhook_isolate \"$data_root\"")
        .expect("run_one_project must enter the shared isolated environment");
    let neutralized = body
        .find("unset STORYHOOK_WEB_TRUSTED_HOSTS")
        .expect("run_one_project must clear an inherited reverse-proxy allowlist");
    let started = body
        .find("start_output=\"$(\"$story_bin\" daemon start 2>&1)\"")
        .expect("run_one_project must start its daemon");

    assert!(
        isolated < neutralized && neutralized < started,
        "run_one_project must clear STORYHOOK_WEB_TRUSTED_HOSTS after isolation and before daemon \
         startup; inherited proxy configuration withdraws local_request authority and breaks the \
         handoff fixture"
    );
}

#[test]
fn the_runner_scopes_the_fake_origin_to_the_special_daemon() {
    let runner = read("scripts/run-e2e.sh");
    let body = runner
        .split_once("run_one_project() {")
        .expect("scripts/run-e2e.sh must define run_one_project")
        .1
        .split_once("\n# --- Decide:")
        .expect("scripts/run-e2e.sh must end run_one_project before its outer project selection")
        .0;

    for required in [
        "UNTRUSTED_ORIGIN_HOST=\"storyhook.e2e.test\"",
        "if [ \"$project\" = \"untrusted-origin-chromium\" ]; then",
        "export STORYHOOK_WEB_TRUSTED_HOSTS=\"$UNTRUSTED_ORIGIN_HOST\"",
        "base_url=\"http://$UNTRUSTED_ORIGIN_HOST:$port\"",
    ] {
        assert!(
            body.contains(required),
            "specialized runner path lost `{required}`"
        );
    }

    let neutralized = body
        .find("unset STORYHOOK_WEB_TRUSTED_HOSTS")
        .expect("the ambient allowlist must be cleared first");
    let specialized = body
        .find("export STORYHOOK_WEB_TRUSTED_HOSTS=\"$UNTRUSTED_ORIGIN_HOST\"")
        .expect("the special project must opt back in");
    let started = body
        .find("start_output=\"$(\"$story_bin\" daemon start 2>&1)\"")
        .expect("run_one_project must start its daemon");
    assert!(
        neutralized < specialized && specialized < started,
        "the fake host must be admitted only after ambient state is cleared and before this \
         project's isolated daemon starts"
    );
}

// ---------------------------------------------------------------------------
// 10. Dispatch-provider availability belongs to the browser fixture
// ---------------------------------------------------------------------------

#[test]
fn the_runner_provisions_both_dispatch_provider_commands_before_daemon_startup() {
    let runner = read("scripts/run-e2e.sh");
    let body = runner
        .split_once("run_one_project() {")
        .expect("scripts/run-e2e.sh must define run_one_project")
        .1
        .split_once("\n# --- Decide:")
        .expect("scripts/run-e2e.sh must end run_one_project before its outer project selection")
        .0;

    // SH-626 moved the doubles themselves into `scripts/e2e-provider-doubles.sh`
    // so a test can generate them and drive them from the daemon's own
    // allowlisted environment (`tests/e2e_provider_doubles.rs`); the runner's
    // obligation here is unchanged -- both provider names executable on PATH
    // before the daemon snapshots availability -- and is checked at the call
    // site plus the library that writes them.
    let library = read("scripts/e2e-provider-doubles.sh");
    let claude = library
        .find("cat >\"$provider_bin/claude\" <<'PROVIDER'")
        .expect("the browser harness must create its own Claude fixture");
    let codex = library
        .find("cat >\"$provider_bin/codex\" <<'PROVIDER'")
        .expect("the browser harness must retain its Codex fixture");
    let executable = library
        .find("chmod 700 \"$provider_bin/claude\" \"$provider_bin/codex\" \"$provider_bin/tmux\"")
        .expect("both provider fixtures and fake tmux must be executable");
    assert!(
        claude < executable && codex < executable,
        "the library writes both provider fixtures before making them executable"
    );

    let written = body
        .find("write_e2e_provider_doubles \"$provider_bin\" \"$faketmux_env\" \"$FAKE_TMUX_IMPLEMENTATION\" || exit 1")
        .expect("run_one_project must generate the doubles through the library, refusing on failure");
    let path = body
        .find("export PATH=\"$provider_bin:$PATH\"")
        .expect("the fixture directory must be placed on PATH");
    let started = body
        .find("start_output=\"$(\"$story_bin\" daemon start 2>&1)\"")
        .expect("run_one_project must start its daemon");

    assert!(
        written < path && path < started,
        "both executable provider fixtures must be on PATH before daemon startup snapshots \n\
         provider availability"
    );
}

// ---------------------------------------------------------------------------
// 11. The runner and the specs run ONE leased inode, never Cargo's artifact
// ---------------------------------------------------------------------------
//
// SH-635: `target/debug/story` is the path any `cargo build|test|check` in
// this checkout replaces. A browser run that executes it directly -- for its
// daemon, its seeding, or a spec's own CLI call -- is voided by the next
// rebuild: the daemon's `(exe, exe_mtime)` identity no longer matches, the
// next client replaces it on a new port, and every later test refuses the
// connection. `scripts/binary-lease.sh` (the shell twin of SH-532's
// `story_binary()`) is the lease; these fences pin that the runner takes it,
// hands it to the specs, and that no tracked file under `e2e/` reaches for
// the artifact itself.

/// `text` with every line that is wholly a comment removed. Deliberately not a
/// real comment stripper: a trailing `// ...` stays, so a mention hidden after
/// code on the same line still counts -- the scan fails closed.
fn without_comment_only_lines(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let t = line.trim_start();
            !(t.starts_with("//")
                || t.starts_with('*')
                || t.starts_with("/*")
                || t.starts_with('#'))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every tracked file under `e2e/` (specs, support, config, the probe) --
/// derived from `git ls-files`, never a hand-kept list.
fn all_tracked_e2e_files(root: &Path) -> Vec<(String, String)> {
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z", "--", "e2e"])
        .output()
        .expect("listing this repository's tracked e2e files");
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so this scan proved nothing: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|path| {
            let relative = std::str::from_utf8(path).expect("a UTF-8 path").to_string();
            let text = std::fs::read_to_string(root.join(&relative))
                .unwrap_or_else(|e| panic!("reading {relative}: {e}"));
            (relative, text)
        })
        .collect()
}

#[test]
fn the_runner_leases_the_artifact_after_building_and_never_runs_it_bare() {
    let runner = read("scripts/run-e2e.sh");
    let code = without_comment_only_lines(&runner);

    assert!(
        code.contains(". \"$repo_root/scripts/binary-lease.sh\""),
        "scripts/run-e2e.sh must source scripts/binary-lease.sh (SH-635)"
    );
    let build = code
        .find("cargo build --quiet")
        .expect("the runner builds the binary");
    let lease = code
        .find("story_bin=\"$(storyhook_lease_binary \"$story_artifact\")\" || exit 1")
        .expect(
            "story_bin must be the lease of the artifact, taken through storyhook_lease_binary",
        );
    assert!(
        build < lease,
        "the lease must be taken AFTER the build, or it pins the previous build"
    );
    assert!(
        code.contains("trap 'rm -rf \"$story_lease_dir\"' EXIT"),
        "the outer script must release its own lease on exit"
    );
    // The artifact path is assigned once and thereafter only compared (`-ef`)
    // or tested (`-x`) -- never executed.
    for line in code.lines() {
        let t = line.trim_start();
        assert!(
            !(t.starts_with("\"$story_artifact\"") || t.contains("$(\"$story_artifact\"")),
            "scripts/run-e2e.sh executes Cargo's mutable artifact directly: {line}"
        );
    }
    assert!(
        !code.contains("story_bin=\"$repo_root/target/debug/story\""),
        "story_bin must never be Cargo's own path again"
    );
}

#[test]
fn the_runner_hands_the_lease_to_the_specs_before_the_daemon_starts() {
    let runner = read("scripts/run-e2e.sh");
    let body = runner
        .split_once("run_one_project() {")
        .expect("scripts/run-e2e.sh must define run_one_project")
        .1
        .split_once("\n# --- Decide:")
        .expect("scripts/run-e2e.sh must end run_one_project before its outer project selection")
        .0;
    let exported = body
        .find("export DASHBOARD_STORY_BIN=\"$story_bin\"")
        .expect(
            "run_one_project must export DASHBOARD_STORY_BIN, the specs' one door to the lease",
        );
    let started = body
        .find("start_output=\"$(\"$story_bin\" daemon start 2>&1)\"")
        .expect("run_one_project must start its daemon from the lease");
    assert!(
        exported < started,
        "the export precedes the daemon start it describes"
    );
}

/// Three reviewed SQLite commands and one reporter test, never a general
/// interpreter exception. The reporter script is additionally pinned by digest.
/// Full invocation text pins executable, literal Python payload, argument shape,
/// and process bound. A body/argv edit needs a new audit; moving a command into
/// a helper does not remove it from this inventory. No SQLite payload can invoke
/// Story CLI: setup writes only its private test database, the lock holder
/// holds only that private database until stdin closes, and the reader opens
/// the mandatory isolated store with mode=ro and parameterized identity queries.
/// The reader is asynchronous (`execFile`, SH-765) and its bound is a name,
/// `boundMs`: the time left of the cleanup wait's graced patience. The reader
/// refuses an unusable bound before it spawns
/// (`the_barrier_read_refuses_an_unusable_bound_before_it_spawns`).
const AUDITED_COMMANDS: [(&str, &str); 4] = [
    (
        "e2e/specs/cleanup-delivery-barrier.node.spec.ts",
        r#"execFileSync("python3", ["-c", `
import sqlite3, sys
with sqlite3.connect(sys.argv[1]) as db:
    db.executescript("""
        CREATE TABLE projects(id,uuid,slug,prefix,checkout_path);
        CREATE TABLE stories(project_id,story_no,created_at);
        CREATE TABLE block_deliveries(id,project_id,story_no,action,status);
        INSERT INTO projects VALUES(1,'fixture-uuid','alpha-project','AA','/fixture'),(2,'other','beta-project','BB','/other');
        INSERT INTO stories VALUES(1,171,'created'),(1,172,'neighbor'),(2,171,'other');
        INSERT INTO block_deliveries VALUES(1,1,171,'interrupt','attempting'),(2,1,172,'interrupt','pending'),(3,2,171,'interrupt','pending');
    """)
`, path], { timeout: gracedPatience(), stdio: "pipe" })"#,
    ),
    (
        "e2e/block-delivery-barrier.cjs",
        r#"execFile("python3", ["-c", `
import json, pathlib, sqlite3, sys
path, project, story, bound_ms = sys.argv[1:]
with sqlite3.connect(pathlib.Path(path).as_uri() + "?mode=ro", uri=True, timeout=int(bound_ms) / 1000) as db:
    db.execute("BEGIN")
    identities = db.execute("""
        SELECT p.id,p.uuid,p.slug,p.prefix,p.checkout_path,s.story_no,s.created_at
        FROM projects p JOIN stories s ON s.project_id=p.id
        WHERE p.slug=? AND p.prefix || '-' || s.story_no=?
    """, (project, story)).fetchall()
    if len(identities) != 1:
        raise RuntimeError("cleanup story identity is absent or ambiguous: " + project + "/" + story)
    identity = identities[0]
    deliveries = db.execute("""
        SELECT id,action,status FROM block_deliveries
        WHERE project_id=? AND story_no=? ORDER BY id
    """, (identity[0], identity[5])).fetchall()
    print(json.dumps({"identity": identity, "deliveries": deliveries}))
`, storePath, project, story, String(boundMs)], { encoding: "utf8", timeout: boundMs }, "#,
    ),
    (
        "e2e/specs/cleanup-delivery-barrier.node.spec.ts",
        r#"execFile("python3", ["-c", `
import sqlite3, sys
with sqlite3.connect(sys.argv[1]) as db:
    db.execute("BEGIN EXCLUSIVE")
    print("locked", flush=True)
    sys.stdin.read()
    db.rollback()
`, path], { timeout: gracedOperationBudget(LOCK_HOLDER_BASE_MS) }, "#,
    ),
    (
        "e2e/reporter-command.ts",
        r#"execFile("python3", [
      resolve(__dirname, "../scripts/test-browser-launch-reporter.py"), "--watch-parent",
    ], { encoding: "utf8", timeout: boundMs, signal }, "#,
    ),
];

/// The authority required by one browser-harness subprocess invocation.
#[derive(Debug, PartialEq, Eq)]
enum E2eSubprocessOwner {
    StoryLease,
    AuditedCommand(usize),
}

/// The two `node:child_process` calls this audit classifies. Both are direct
/// calls: the synchronous form, and the asynchronous one the cleanup barrier's
/// reader uses so the worker's timers keep running (SH-765). The anchors are
/// disjoint, because `execFileSync(` does not contain `execFile(`.
const SUBPROCESS_CALLS: [&str; 2] = e2e_subprocess::CALLS;

/// Byte offsets of every audited subprocess call in `code`, in file order.
fn subprocess_call_offsets(code: &str) -> Vec<usize> {
    let mut offsets: Vec<usize> = SUBPROCESS_CALLS
        .iter()
        .flat_map(|anchor| code.match_indices(anchor).map(|(offset, _)| offset))
        .collect();
    offsets.sort_unstable();
    offsets
}

/// The expression an approved invocation passes as its process bound.
fn approved_bound(approved: &str) -> &str {
    let rest = approved
        .split_once("timeout: ")
        .expect("every audited command states a process bound")
        .1;
    let end = rest
        .find([',', ' ', '}'])
        .expect("the bound expression ends inside the options object");
    &rest[..end]
}

/// Classify the exact call, never the mere presence of an approved interpreter.
fn e2e_subprocess_owner(relative: &str, code: &str, offset: usize) -> Option<E2eSubprocessOwner> {
    let invocation = code.get(offset..)?;
    let rest = SUBPROCESS_CALLS
        .iter()
        .find_map(|anchor| invocation.strip_prefix(anchor))?;
    let first_arg = rest.split(',').next()?.trim();
    if first_arg == "storyBinary()" || code.contains(&format!("const {first_arg} = storyBinary();"))
    {
        return Some(E2eSubprocessOwner::StoryLease);
    }
    AUDITED_COMMANDS
        .iter()
        .position(|(path, approved)| *path == relative && invocation.starts_with(*approved))
        .map(E2eSubprocessOwner::AuditedCommand)
}

#[test]
fn sqlite_data_commands_require_their_exact_audited_site_and_payload() {
    for (index, (path, approved)) in AUDITED_COMMANDS[..3].iter().enumerate() {
        assert_eq!(
            e2e_subprocess_owner(path, approved, 0),
            Some(E2eSubprocessOwner::AuditedCommand(index))
        );
        assert_eq!(
            e2e_subprocess_owner("e2e/other-helper.cjs", approved, 0),
            None
        );
        let bound = format!("timeout: {}", approved_bound(approved));
        // Dropping the bound entirely, whichever side of it the comma sits on.
        let unbounded = if approved.contains(&format!(", {bound}")) {
            approved.replace(&format!(", {bound}"), "")
        } else {
            approved.replace(&bound, "")
        };
        for changed in [
            approved.replace("\"python3\"", "\"story\""),
            approved.replace(&bound, "timeout: 0"),
            unbounded,
            approved.replace("[\"-c\",", "[\"-m\","),
            approved.replace(
                "\nwith sqlite3.connect",
                "\n__import__('subprocess').run(['story', 'daemon', 'stop'])\nwith sqlite3.connect",
            ),
            approved.replace("`\nimport", "`\n${unleasedStory()}\nimport"),
            if index != 1 {
                approved.replace("`, path]", "`, storyBinary()]")
            } else {
                approved.replace("?mode=ro", "?mode=rw")
            },
        ] {
            assert_ne!(
                changed.as_str(),
                *approved,
                "the counterexample must change the call"
            );
            assert_eq!(e2e_subprocess_owner(path, &changed, 0), None, "{changed}");
        }
    }
}

#[test]
fn reporter_command_requires_its_exact_site_arguments_and_bound() {
    let (path, approved) = AUDITED_COMMANDS[3];
    assert_eq!(
        e2e_subprocess_owner(path, approved, 0),
        Some(E2eSubprocessOwner::AuditedCommand(3))
    );
    assert_eq!(
        e2e_subprocess_owner("e2e/other-helper.ts", approved, 0),
        None
    );
    for changed in [
        approved.replace("\"python3\"", "\"story\""),
        approved.replace("test-browser-launch-reporter.py", "other.py"),
        approved.replace("\"--watch-parent\"", "\"-c\", \"import subprocess\""),
        approved.replace("timeout: boundMs, ", ""),
        approved.replace("timeout: boundMs", "timeout: 0"),
        approved.replace("timeout: boundMs", "timeout: 60000"),
        approved.replace(", signal", ""),
    ] {
        assert_ne!(changed, approved);
        assert_eq!(e2e_subprocess_owner(path, &changed, 0), None, "{changed}");
    }
}

/// The exception authorizes this reviewed script, never arbitrary Python code.
fn is_reviewed_reporter_script(source: &str) -> bool {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(source.as_bytes()))
        == "c7ad2d0ee26cc09112575f876ba15990e46c8e3b4ce4d86a25635778946f87b8"
}

#[test]
fn reporter_script_and_derived_budget_require_reaudit_when_they_change() {
    let script = read("scripts/test-browser-launch-reporter.py");
    assert!(
        is_reviewed_reporter_script(&script),
        "reporter script changed: review its subprocesses and refresh the SHA-256 pin"
    );
    assert!(!is_reviewed_reporter_script(&format!(
        "{script}\nsubprocess.run(['story'])\n"
    )));
    let helper = read("e2e/reporter-command.ts");
    let case_count = script
        .lines()
        .filter(|line| line.starts_with("    def test_"))
        .count();
    assert_eq!(
        case_count, 4,
        "update the Node budget with the Python suite"
    );
    assert!(script.contains("CASE_TIMEOUT_SECONDS = 60"));
    for required in [
        "REPORTER_CASE_COUNT = 4;",
        "REPORTER_CASE_TIMEOUT_MS = 60_000;",
        "REPORTER_CASE_COUNT * REPORTER_CASE_TIMEOUT_MS + BASE_TEST_TIMEOUT_MS",
        "loadGraceEnabled() ? gracedTestBudget(REPORTER_BASE_MS, ratio) : REPORTER_BASE_MS",
        "testMs - gracedPatience(ratio)",
    ] {
        assert!(helper.contains(required), "reporter budget lost {required}");
    }
    let guard = helper
        .find("if (!Number.isSafeInteger(boundMs) || boundMs < 1)")
        .unwrap();
    let spawn = helper.find("execFile(\"python3\"").unwrap();
    assert!(guard < spawn, "invalid bounds must fail before spawning");
}

#[test]
fn reporter_cleanup_reaps_nested_processes_on_every_exit_path() {
    let mut command = std::process::Command::new("python3");
    command
        .arg("-B")
        .arg(repo_root().join("scripts/tests/test_browser_reporter_cleanup.py"));
    // Six scenarios, each allowing startup, cancellation/exit and the reap receipt.
    let budget = storyhook_test_support::load_grace::graced_now(UTILITY_DEADLINE * 6 * 3);
    let output = ChildGuard::spawn_with_output(&mut command)
        .expect("start reporter lifecycle regressions")
        .wait_with_output_within(budget, || {
            "reporter lifecycle regressions did not finish".into()
        });
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// SH-807: real out-of-tree writers and placeholders must settle before removal.
#[test]
fn interrupted_slice_drains_registered_dispatch_writers_before_removal() {
    let mut command = std::process::Command::new("python3");
    command
        .arg("-B")
        .arg(repo_root().join("scripts/tests/test_e2e_dispatch_cleanup.py"));
    let budget = storyhook_test_support::load_grace::graced_now(UTILITY_DEADLINE * 8 * 3);
    let output = ChildGuard::spawn_with_output(&mut command)
        .expect("start dispatch cleanup regressions")
        .wait_with_output_within(budget, || {
            "dispatch cleanup regressions did not finish".into()
        });
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Whether a process bound is a bare millisecond literal (`5_000`, `15000`).
fn is_bare_numeric(bound: &str) -> bool {
    !bound.is_empty() && bound.chars().all(|c| c.is_ascii_digit() || c == '_')
}

/// SH-765: every process bound in this inventory is derived from the
/// load-grace patience, never a bare literal. A fixed number of milliseconds
/// is exactly the bound that failed under load (`spawnSync python3
/// ETIMEDOUT` at load 727-900), so a re-audit that writes one back fails
/// here, by name, rather than passing the exact-text match it was given.
#[test]
fn no_audited_command_carries_a_bare_numeric_bound() {
    // Controls, so the predicate below cannot pass by seeing nothing.
    assert!(is_bare_numeric("5_000") && is_bare_numeric("15000"));
    assert!(!is_bare_numeric("boundMs") && !is_bare_numeric("gracedPatience()"));
    for (path, approved) in AUDITED_COMMANDS {
        let bound = approved_bound(approved);
        assert!(
            !is_bare_numeric(bound),
            "{path}: the audited process bound `{bound}` is a bare literal. Derive it from \
             the load-grace patience (gracedPatience(), or what remains of a wait's) (SH-765)"
        );
    }
}

/// The reader's bound is a name, so the audit pins the guard that keeps the
/// name meaningful (SH-765). Node reads `timeout: 0` as "no bound at all" and
/// throws on a fraction, so the guard must run before the spawn, inside the
/// Promise executor, where a throw becomes the read's rejection.
#[test]
fn the_barrier_read_refuses_an_unusable_bound_before_it_spawns() {
    let reader = without_comment_only_lines(&read("e2e/block-delivery-barrier.cjs"));
    let signature = reader
        .find("function readBlockDeliverySnapshot(storePath, project, story, boundMs) {")
        .expect("the reader takes its bound from its caller");
    let executor = reader[signature..]
        .find("return new Promise((resolve, reject) => {")
        .map(|at| signature + at)
        .expect("the reader is asynchronous, so the worker's timers keep running");
    let guard = reader[executor..]
        .find("if (!Number.isSafeInteger(boundMs) || boundMs < 1) throw new Error(")
        .map(|at| executor + at)
        .expect("the reader refuses a bound that is not a positive whole number");
    let spawn = reader[executor..]
        .find("execFile(\"python3\"")
        .map(|at| executor + at)
        .expect("the reader spawns python3 inside its Promise");
    assert!(
        guard < spawn,
        "the bound guard must run before python3 starts, or a zero bound spawns an unbounded read"
    );
}

#[test]
fn a_data_fixture_exception_cannot_authorize_another_subprocess() {
    let (path, approved) = AUDITED_COMMANDS[0];
    for unapproved in [
        r#"execFileSync("story", ["daemon", "stop"]);"#,
        r#"execFileSync("/Users/example/.local/bin/story", ["show", "SH-1"]);"#,
        r#"execFileSync("python3", ["-c", "import subprocess; subprocess.run(['story'])"]);"#,
        r#"execFileSync("python3", ["-c", "print('unreviewed')"]);"#,
        r#"execFile("python3", ["-c", "print('unreviewed')"], callback);"#,
        r#"execFile("story", ["daemon", "stop"], callback);"#,
    ] {
        let code = format!("{approved};\n{unapproved}");
        let calls: Vec<_> = subprocess_call_offsets(&code)
            .into_iter()
            .map(|offset| e2e_subprocess_owner(path, &code, offset))
            .collect();
        assert_eq!(calls, [Some(E2eSubprocessOwner::AuditedCommand(0)), None]);
    }
}

#[test]
fn story_cli_commands_still_require_the_lease_in_specs_and_helpers() {
    for path in ["e2e/specs/example.spec.ts", "e2e/example-helper.cjs"] {
        for code in [
            "execFileSync(storyBinary(), args, options);",
            "const STORY_BINARY = storyBinary();\nexecFileSync(STORY_BINARY, args, options);",
        ] {
            let offset = code.find("execFileSync(").unwrap();
            assert_eq!(
                e2e_subprocess_owner(path, code, offset),
                Some(E2eSubprocessOwner::StoryLease)
            );
        }
        for code in [
            "execFileSync(STORY_BINARY, args, options);",
            "const STORY_BINARY = installedStory();\nexecFileSync(STORY_BINARY, args, options);",
            "execFileSync(process.env.DASHBOARD_STORY_BIN, args, options);",
        ] {
            let offset = code.find("execFileSync(").unwrap();
            assert_eq!(e2e_subprocess_owner(path, code, offset), None);
        }
    }
}

#[test]
fn no_tracked_e2e_file_names_cargos_artifact_and_every_cli_call_goes_through_story_binary() {
    let root = repo_root();
    let files = all_tracked_e2e_files(&root);
    assert!(
        files.iter().any(|(p, _)| p == "e2e/specs/support.ts"),
        "the scan must see support.ts, or it proved nothing"
    );

    let offenders: Vec<String> = files
        .iter()
        .filter(|(_, text)| without_comment_only_lines(text).contains("target/debug"))
        .map(|(p, _)| p.clone())
        .collect();
    assert!(
        offenders.is_empty(),
        "{offenders:?} name Cargo's mutable artifact directory; use support.ts's storyBinary() \
         (the lease scripts/run-e2e.sh exports as DASHBOARD_STORY_BIN) instead (SH-635)"
    );

    let support = read("e2e/specs/support.ts");
    assert!(
        support.contains("export function storyBinary(): string {")
            && support.contains("return requiredEnv(\"DASHBOARD_STORY_BIN\");"),
        "support.ts must define storyBinary() as the required-env read of DASHBOARD_STORY_BIN"
    );

    // Story CLI calls require the lease. Reviewed fixture commands require their
    // entire audited invocation, not just an interpreter name. Include helpers
    // outside specs/: moving an unleased call must not make it invisible.
    let mut checked = 0;
    let mut audited = [0; AUDITED_COMMANDS.len()];
    for (relative, text) in &files {
        for offset in
            e2e_subprocess::calls(relative, text).unwrap_or_else(|error| panic!("{error}"))
        {
            match e2e_subprocess_owner(relative, text, offset) {
                Some(E2eSubprocessOwner::StoryLease) => checked += 1,
                Some(E2eSubprocessOwner::AuditedCommand(index)) => audited[index] += 1,
                None => panic!(
                    "{relative}: unowned subprocess invocation at byte {offset}; Story CLI calls \
                     must use storyBinary() or a const bound to it. Only the exact reviewed \
                     SQLite and reporter commands have separate authority (SH-635/SH-718/SH-805)"
                ),
            }
        }
    }
    assert!(
        checked >= 5,
        "expected at least the five spec call sites SH-635 migrated, found {checked}: the scan's \
         `execFileSync(` anchor has drifted"
    );
    assert_eq!(
        audited,
        [1; AUDITED_COMMANDS.len()],
        "every audited command must be present exactly once; re-audit changed sites"
    );
}

#[test]
fn the_runner_asks_whether_its_daemon_survived_after_every_playwright_run() {
    let runner = read("scripts/run-e2e.sh");
    let code = without_comment_only_lines(&runner);
    assert!(
        code.contains(". \"$repo_root/scripts/e2e-daemon-check.sh\""),
        "scripts/run-e2e.sh must source scripts/e2e-daemon-check.sh (SH-635)"
    );
    let body = runner
        .split_once("run_one_project() {")
        .expect("scripts/run-e2e.sh must define run_one_project")
        .1
        .split_once("\n# --- Decide:")
        .expect("scripts/run-e2e.sh must end run_one_project before its outer project selection")
        .0;
    let body = &without_comment_only_lines(body);
    let playwright = body
        .find("npx playwright test --project=\"$project\" --output=")
        .expect("run_one_project runs Playwright");
    let divergence = body
        .find("if ! [ \"$story_bin\" -ef \"$story_artifact\" ]; then")
        .expect("the runner reports an artifact rebuilt under the run, by inode comparison");
    let check = body
        .find("if ! storyhook_daemon_is_still_ours \"$portfile\" \"$port\" \"$story_bin\"; then")
        .expect("the runner asks whether the daemon it started is still the one answering");
    let verdict = body
        .find("\"$([ \"$status\" = 0 ] && echo passed || echo failed)\"")
        .expect("the per-project verdict line");
    assert!(
        playwright < divergence && playwright < check && check < verdict,
        "both post-run checks sit between the Playwright run and the verdict it reports"
    );
    let failure_branch = &body[check..verdict];
    assert!(
        failure_branch.contains("status=1"),
        "a green Playwright verdict from a daemon this harness did not configure must still \
         fail the project (SH-226, SH-306)"
    );
    assert!(
        !failure_branch.contains("$daemon_pid") && !body.contains("expected_pid"),
        "the check must not compare the pid recorded at start -- untrusted-origin-cookie.spec.ts \
         restarts the daemon on purpose and keeps its port (SH-321)"
    );
}

// ---------------------------------------------------------------------------
// 12. The placeholder pane outlives the run, and the run is what ends it
// ---------------------------------------------------------------------------
//
// SH-626: the fake tmux's `new-window` spawns a real `sleep` to stand in for
// the pane's occupant, self-expiring after `FAKE_TMUX_PANE_LIFETIME` seconds
// (30 by default -- a self-heal for shell tests that forget to kill it). Once
// the daemon's liveness probe actually reaches the fake, that expiry reads as
// a genuinely dead window, and a lane alive longer than the default would be
// quarantined `window-gone` for a reason no browser spec controls. The runner
// therefore states a lifetime derived from the longest measured browser leg,
// BEFORE the snapshot that forwards knobs to dispatch children (the same
// position rule `tests/store_isolation.rs` pins), and its cleanup reaps the
// recorded placeholder so nothing outlives the run.

#[test]
fn the_placeholder_pane_lifetime_is_stated_before_the_snapshot_and_reaped_at_cleanup() {
    let runner = read("scripts/run-e2e.sh");
    let body = runner
        .split_once("run_one_project() {")
        .expect("scripts/run-e2e.sh must define run_one_project")
        .1
        .split_once("\n# --- Decide:")
        .expect("scripts/run-e2e.sh must end run_one_project before its outer project selection")
        .0;
    let lifetime = body
        .find("export FAKE_TMUX_PANE_LIFETIME=")
        .expect("run_one_project must state the placeholder pane's lifetime");
    let seconds: u64 = body[lifetime + "export FAKE_TMUX_PANE_LIFETIME=".len()..]
        .split_whitespace()
        .next()
        .and_then(|value| value.parse().ok())
        .expect("the lifetime is a literal number of seconds the fake's `sleep` accepts");
    // The longest browser leg measured in this repository, 6538s under
    // contention (docs/spec/test-tiers.md, SH-627): a placeholder that
    // expired inside a leg would turn its expiry into a lane's verdict.
    const LONGEST_MEASURED_LEG_SECS: u64 = 6538;
    assert!(
        seconds > LONGEST_MEASURED_LEG_SECS,
        "the placeholder must outlive the longest measured browser leg ({LONGEST_MEASURED_LEG_SECS}s), got {seconds}s"
    );
    let snapshot = body
        .find("compgen -e")
        .expect("run_one_project snapshots its FAKE_TMUX_* knobs");
    assert!(
        lifetime < snapshot,
        "the lifetime must be exported before the snapshot that forwards it to dispatch children"
    );
    let cleanup = body
        .split_once("cleanup() {")
        .expect("run_one_project defines cleanup")
        .1
        .split_once("\n  }\n")
        .expect("cleanup closes")
        .0;
    let drain = cleanup
        .find("\"$dispatch_owner_tool\" drain")
        .expect("cleanup drains registered helper and placeholder incarnations");
    let removal = cleanup
        .find("rm -rf \"$data_root\"")
        .expect("cleanup removes the data root");
    assert!(drain < removal);
    assert!(
        !cleanup.contains("kill -9") && !cleanup.contains("cat \"$data_root/faketmux/pane_pid"),
        "a bare historical placeholder PID must not grant signal authority"
    );
}

/// Run the runner's actual environment statements without building or starting a browser.
fn run_runner_environment(
    block: &str,
    environment: &[(&str, &str)],
    probe: &str,
) -> std::process::Output {
    let mut command = std::process::Command::new("/bin/bash");
    command.env_clear().env("PATH", "/usr/bin:/bin");
    for (name, value) in environment {
        command.env(name, value);
    }
    command.args(["-c", &format!("set -euo pipefail\n{block}\n{probe}")]);
    ChildGuard::spawn_with_output(&mut command)
        .expect("running the browser harness environment boundary")
        .wait_with_output_within(
            storyhook_test_support::load_grace::graced_now(UTILITY_DEADLINE),
            || "browser harness environment probe did not finish".to_string(),
        )
}

#[test]
fn the_runner_routes_plugin_children_and_bare_story_calls_through_its_lease() {
    let runner = read("scripts/run-e2e.sh");
    let block = runner
        .split_once("  export DASHBOARD_STORY_BIN=\"$story_bin\"")
        .expect("the browser runner must hand the lease to specs")
        .1
        .split_once("  # This is not a store-isolation parameter")
        .expect("the lease boundary must precede proxy setup")
        .0;
    let block = format!("export DASHBOARD_STORY_BIN=\"$story_bin\"\n{block}");
    let fixture = storyhook_test_support::scratch_dir();
    let lease_dir = fixture.path().join("lease with spaces");
    let ambient_dir = fixture.path().join("ambient");
    for directory in [&lease_dir, &ambient_dir] {
        std::fs::create_dir_all(directory).unwrap();
        std::os::unix::fs::symlink("/bin/sh", directory.join("story")).unwrap();
    }
    let lease = lease_dir.join("story");
    let ambient = ambient_dir.join("story");
    let path = format!("{}:/usr/bin:/bin", ambient_dir.display());
    for poisoned_override in [false, true] {
        let mut environment = vec![
            ("story_bin", lease.to_str().unwrap()),
            ("story_lease_dir", lease_dir.to_str().unwrap()),
            ("PATH", path.as_str()),
            ("DASHBOARD_STORY_BIN", ambient.to_str().unwrap()),
        ];
        if poisoned_override {
            environment.push(("STORY_BIN", ambient.to_str().unwrap()));
        }
        let output = run_runner_environment(
            &block,
            &environment,
            "/bin/bash -c 'printf \"%s\\n\" \"${STORY_BIN:-missing}\" \"$(command -v story)\" \"$DASHBOARD_STORY_BIN\"'",
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected = format!("{0}\n{0}\n{0}\n", lease.display());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            expected,
            "plugin overrides, bare CLI calls, and browser specs must inherit the same lease"
        );
    }
}

#[test]
fn the_runner_preserves_no_color_intent_without_exporting_conflicting_node_flags() {
    let runner = read("scripts/run-e2e.sh");
    let entry = runner
        .split_once("# Playwright forces FORCE_COLOR=1 in workers.")
        .expect("the runner must translate the caller's color intent")
        .1
        .split_once('\n')
        .unwrap()
        .1
        .split_once("\ncd \"$(dirname \"$0\")/..\"")
        .expect("the runner entry must precede checkout setup")
        .0;
    for no_color in ["", "1"] {
        let output = run_runner_environment(
            entry,
            &[
                ("NO_COLOR", no_color),
                ("FORCE_COLOR", "1"),
                ("DEBUG_COLORS", "1"),
            ],
            "printf '%s\\n' \"${NO_COLOR+present}\" \"${FORCE_COLOR:-missing}\" \"${DEBUG_COLORS:-missing}\"",
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "\n0\n0\n",
            "NO_COLOR must become one unambiguous no-color setting before Node starts"
        );
    }
    let output = run_runner_environment(
        entry,
        &[("FORCE_COLOR", "2"), ("DEBUG_COLORS", "1")],
        "printf '%s\\n' \"${NO_COLOR+present}\" \"$FORCE_COLOR\" \"$DEBUG_COLORS\"",
    );
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "\n2\n1\n",
        "an explicit color preference without NO_COLOR must remain unchanged"
    );
}
