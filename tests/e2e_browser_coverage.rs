//! Fences the browser-coverage invariants SH-335 and SH-348 introduce.
//!
//! `e2e/playwright.config.ts` now names five projects -- two engine pairs,
//! `chromium`/`webkit` (desktop) and `mobile-chromium`/`mobile-webkit`
//! (mobile, SH-348), plus SH-321's isolated untrusted-origin Chromium leg --
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
//! 4. A failed project stops the matrix or a later project's Playwright
//!    invocation erases its failure artifacts --
//!    `the_matrix_records_failures_continues_and_keeps_each_projects_artifacts`.
//! 5. The real-dispatch post-check selects exactly `specs/dispatch.spec.ts`
//!    and `specs/engine.spec.ts`, not another stubbed spec whose filename
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
         {offenders:?}. The Playwright matrix drives Chromium and WebKit, and `make \
         e2e-install` installs both (SH-335/SH-374)."
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

    assert_eq!(
        pair_selectors[0].1.1.as_str(),
        "DESKTOP_EXCLUDED_SPECS",
        "the desktop pair must exclude phone-subject specs and the isolated untrusted-origin spec"
    );
    assert_eq!(
        pair_selectors[1].1.1.as_str(),
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
            && config_text
                .contains("const DESKTOP_EXCLUDED_SPECS = [MOBILE_SPECS, UNTRUSTED_ORIGIN_SPECS];"),
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

// ---------------------------------------------------------------------------
// 4. A continued matrix preserves every project's failure evidence
// ---------------------------------------------------------------------------

#[test]
fn the_matrix_records_failures_continues_and_keeps_each_projects_artifacts() {
    let runner = read("scripts/run-e2e.sh");

    assert!(
        runner.contains("run_one_project \"$project\"")
            && runner.contains("project=$project FAILED (exit $status)")
            && runner.contains("overall_status=$status")
            && runner.contains("exit \"$overall_status\""),
        "a failed Playwright project must be recorded while the outer project loop continues, \
         and the runner must report failure only after every remaining project had its turn"
    );
    assert!(
        runner.contains("results_root=\"$repo_root/e2e/test-results/current\""),
        "scripts/run-e2e.sh must establish one artifact root for the whole invocation; without \
         it a later Playwright project can clear the earlier project's screenshots and traces"
    );
    assert!(
        runner.contains("--output=\"$results_root/$project\""),
        "each Playwright invocation must write beneath a project-keyed output directory; the \
         default shared test-results directory is cleared at the start of every invocation"
    );
}

// ---------------------------------------------------------------------------
// 5. The real-dispatch post-check selects exactly two specs
// ---------------------------------------------------------------------------

/// Reads the basic-regex argument from run-e2e.sh's live `grep -c` assignment
/// rather than copying the expression into this test. The assertion below
/// therefore exercises the pattern the harness will actually run.
fn real_dispatch_selected_pattern(runner: &str) -> &str {
    let assignment = runner
        .lines()
        .find(|line| line.trim_start().starts_with("real_dispatch_selected="))
        .expect("scripts/run-e2e.sh must assign real_dispatch_selected");
    let marker = "grep -c \"";
    let after = assignment
        .split_once(marker)
        .unwrap_or_else(|| {
            panic!("real_dispatch_selected does not contain `{marker}`: {assignment}")
        })
        .1;
    let end = after.find('"').unwrap_or_else(|| {
        panic!("real_dispatch_selected's grep pattern never closes: {assignment}")
    });
    &after[..end]
}

fn grep_count(pattern: &str, input: &str) -> usize {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut command = Command::new("grep");
    command
        .args(["-c", pattern])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child = ChildGuard::spawn_with_output(&mut command)
        .expect("spawning the same grep scripts/run-e2e.sh uses");
    child
        .stdin()
        .expect("grep stdin was piped")
        .write_all(input.as_bytes())
        .expect("writing synthetic Playwright list output to grep");
    let output = child.wait_with_output_within(UTILITY_DEADLINE, || {
        "the grep coverage probe did not finish".to_string()
    });
    assert!(
        output.status.success() || output.status.code() == Some(1),
        "grep failed unexpectedly: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("grep -c output is UTF-8")
        .trim()
        .parse()
        .expect("grep -c output is a count")
}

#[test]
fn the_real_dispatch_postcheck_matches_only_the_two_exact_specs() {
    let runner = read("scripts/run-e2e.sh");
    let pattern = real_dispatch_selected_pattern(&runner);
    let dispatch = "[chromium] › specs/dispatch.spec.ts:83:5 › Dispatch is absent\n";
    let engine = "[mobile-webkit] › specs/engine.spec.ts:157:5 › Full Auto runs\n";
    let stubbed =
        "[chromium] › specs/story-context-menu-dispatch.spec.ts:72:5 › Dispatch is present\n";

    assert_eq!(
        grep_count(pattern, dispatch),
        1,
        "the post-check must recognize dispatch.spec.ts"
    );
    assert_eq!(
        grep_count(pattern, engine),
        1,
        "the post-check must recognize engine.spec.ts"
    );
    assert_eq!(
        grep_count(pattern, stubbed),
        0,
        "the post-check must not mistake the stubbed context-menu spec for dispatch.spec.ts"
    );
    assert_eq!(
        grep_count(pattern, &format!("{stubbed}{dispatch}{engine}")),
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
        "data_root=\"$(mktemp -d /private/tmp/storyhook-e2e.XXXXXX)\"",
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
        runner.contains("run_one_project \"$project\"")
            && runner.contains("run_one_project \"$explicit_project\""),
        "both matrix and explicit-project paths must enter the same isolated runner"
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
        .split_once("\nfi\n\nexec bash")
        .expect("the dispatch-only guard must close immediately before exec")
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

    // Every process a spec starts is the leased binary: the first argument of
    // each `execFileSync(` is `storyBinary()` itself or a const bound to it in
    // the same file.
    let mut checked = 0;
    for (relative, text) in files.iter().filter(|(p, _)| p.starts_with("e2e/specs/")) {
        let code = without_comment_only_lines(text);
        for (offset, _) in code.match_indices("execFileSync(") {
            let rest = &code[offset + "execFileSync(".len()..];
            let first_arg = rest
                .split(',')
                .next()
                .map(str::trim)
                .unwrap_or_default()
                .to_string();
            let via_door = first_arg == "storyBinary()"
                || code.contains(&format!("const {first_arg} = storyBinary();"));
            assert!(
                via_door,
                "{relative}: execFileSync's first argument `{first_arg}` is not storyBinary() or a \
                 const bound to it -- a spec may only ever run the leased binary (SH-635)"
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 5,
        "expected at least the five spec call sites SH-635 migrated, found {checked}: the scan's \
         `execFileSync(` anchor has drifted"
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
    assert!(
        cleanup.contains("$data_root/faketmux/pane_pid")
            && cleanup.contains("kill -9 \"$placeholder\""),
        "cleanup must reap the placeholder the fake recorded, since no later new-window will"
    );
    let reap = cleanup.find("kill -9 \"$placeholder\"").unwrap();
    let removal = cleanup
        .find("rm -rf \"$data_root\"")
        .expect("cleanup removes the data root");
    assert!(
        reap < removal,
        "the pid must be read and the placeholder killed before the file naming it is deleted"
    );
}
