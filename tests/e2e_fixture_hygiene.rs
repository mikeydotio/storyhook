//! A fence around the browser suite's shared fixtures (SH-245).
//!
//! `scripts/run-e2e.sh` seeds four projects and every spec shares them.
//! "Alpha Project" in particular is seeded with exactly two stories, a shape
//! `filter-persistence.spec.ts` and `column-visibility.spec.ts` assert on by
//! count — so a story some other spec created and did not remove makes those
//! two fail, in a file the actual defect never touched.
//!
//! Every spec deletes what it creates as the last statement of the test body,
//! which is exactly the statement a failing test never reaches. SH-245 is the
//! bill: one genuinely red spec was reported as three, the extra two naming
//! behaviour that was never involved. `cleanUpCreatedStories()`
//! (`e2e/specs/support.ts`) closes it with an `afterEach`, which runs whether
//! the test passed or failed.
//!
//! This test is the part that keeps closing it. A spec added later that
//! creates a story and forgets the registration fails here — in the Rust
//! suite, deterministically, on any machine — rather than intermittently, in
//! someone else's spec, on a loaded one.

use std::path::Path;

/// Every tracked browser spec, paired with its contents.
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

/// Whether a spec creates stories: every creation path in the suite goes
/// through the New Story modal's submit button, and the one spec that also
/// wants the create response's own body (`reopen-soft-deleted-confirm`)
/// still clicks it. A future spec that POSTs the story collection directly
/// is caught by the second arm rather than slipping past.
fn creates_stories(text: &str) -> bool {
    text.contains("#create-submit") || (text.contains(".post(") && text.contains("/story"))
}

/// The browser specs that create stories, paired with their contents.
fn story_creating_specs(root: &Path) -> Vec<(String, String)> {
    let specs: Vec<(String, String)> = all_specs(root)
        .into_iter()
        .filter(|(_, text)| creates_stories(text))
        .collect();

    // A scan that matches nothing passes every assertion built on top of it,
    // which would make a broken pattern indistinguishable from a clean tree.
    assert!(
        specs.len() >= 10,
        "expected the browser suite to have at least ten story-creating specs, \
         found {}: {:?}. Either the suite shrank dramatically or `creates_stories` \
         no longer recognises how a spec creates one.",
        specs.len(),
        specs.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );
    specs
}

/// Every browser spec that creates a story registers the `afterEach` that
/// sweeps it, so a failing test cannot strand one in a fixture the rest of
/// the suite reads.
#[test]
fn every_spec_that_creates_a_story_registers_the_cleanup() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    let unswept: Vec<String> = story_creating_specs(root)
        .iter()
        .filter(|(_, text)| !text.contains("cleanUpCreatedStories("))
        .map(|(relative, _)| relative.clone())
        .collect();

    assert!(
        unswept.is_empty(),
        "{unswept:?} create stories without registering cleanUpCreatedStories(). \
         A test that fails before its own delete strands the story it created in \
         a fixture project other specs count the cards of, so one red spec \
         becomes several and the extra ones name behaviour that was never \
         involved (SH-245). Add `cleanUpCreatedStories(\"<project name>\");` at \
         the top of the file."
    );
}

// ---------------------------------------------------------------------------
// SH-222: a board on screen is not a board with data
// ---------------------------------------------------------------------------

/// The one file allowed to click a project's Home card, because it is where
/// the wait that has to follow the click lives.
const PROJECT_OPENER: &str = "e2e/specs/support.ts";

/// Whether `text` clicks a project's card on the Home screen itself, rather
/// than calling `openProject()`.
///
/// Whitespace is stripped first: the click is often a chained
/// `.locator(…)\n.click()` across two lines, and the shape is otherwise the
/// same everywhere. A `.repo-card-name` locator that is *asserted* on rather
/// than clicked — `expect(page.locator(".repo-card-name", …)).toBeVisible()`,
/// which several specs use to prove Home rendered at all — is deliberately
/// not a match: it navigates nowhere, so there is nothing to wait for.
fn clicks_a_project_card(text: &str) -> bool {
    let dense: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    dense
        .match_indices(r#"locator(".repo-card-name""#)
        .any(|(at, _)| {
            let rest = &dense[at..];
            match rest.find(')') {
                Some(close) => rest[close + 1..].starts_with(".click("),
                None => false,
            }
        })
}

#[test]
fn clicks_a_project_card_reads_the_shapes_the_suite_actually_writes() {
    assert!(clicks_a_project_card(
        r#"await page.locator(".repo-card-name", { hasText: "Alpha Project" }).click();"#
    ));
    // Chained across lines, as `project-selector.spec.ts` used to write it.
    assert!(clicks_a_project_card(
        "await page\n  .locator(\".repo-card-name\", { hasText: \"Gamma Archive\" })\n  .click();"
    ));
    assert!(!clicks_a_project_card(
        r#"await expect(page.locator(".repo-card-name", { hasText: "Alpha Project" })).toBeVisible();"#
    ));
    assert!(!clicks_a_project_card(
        r#"await openProject(page, "Alpha Project");"#
    ));
}

/// Every browser spec reaches a project's board through `openProject()`,
/// which waits for that project's data — never by clicking the card itself.
///
/// `selectRepo()` sets `state.data = null`, renders the board screen and
/// *then* fetches, so `#board-view` is visible for as long as the fetch takes
/// with no stories, no metadata and no per-project vocabulary behind it. A
/// spec that treated the click plus a `toBeVisible()` as arrival was racing
/// that fetch, and losing it is not a slow assertion that retries: the create
/// modal and the filter-bar dropdowns are built **once**, synchronously, from
/// whatever `meta()` holds at the moment they open. Opened in that window
/// they hold placeholders and never repopulate, so `selectOption()` spends
/// the entire 15s test timeout reporting "did not find some options" — which
/// is what SH-223 recorded against `board-sort.spec.ts` twice and
/// `create-story-defaults.spec.ts` once, each time on a machine busy with
/// something else.
///
/// Scoped to the card-click path on purpose, and that is the whole scope: the
/// other two ways into a board are `?project=` deep links and the header
/// selector's menu, and no spec follows either with an action on a
/// `meta()`-derived control — they assert, and assertions retry. A spec that
/// starts to would need the same wait, and this fence would not catch it.
/// `board-readiness.spec.ts` documents the window from the deep-link side for
/// exactly that reason.
#[test]
fn every_spec_opens_a_board_through_the_helper_that_waits_for_its_data() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let opener = std::fs::read_to_string(root.join(PROJECT_OPENER))
        .unwrap_or_else(|e| panic!("reading {PROJECT_OPENER}: {e}"));
    assert!(
        clicks_a_project_card(&opener),
        "{PROJECT_OPENER} no longer clicks a project card, so this scan's one \
         exclusion excludes nothing and the pattern below may match nothing for \
         that reason rather than because the tree is clean"
    );

    let specs = all_specs(root);
    assert!(
        specs.len() >= 20,
        "expected the browser suite to have at least twenty specs, found {}: \
         either it shrank dramatically or the `git ls-files` pathspec no longer \
         matches it",
        specs.len()
    );

    let direct: Vec<String> = specs
        .into_iter()
        .filter(|(_, text)| clicks_a_project_card(text))
        .map(|(relative, _)| relative)
        .collect();

    assert!(
        direct.is_empty(),
        "{direct:?} click a project's Home card directly. The board becomes \
         visible before its data arrives, so the two lines that look like \
         arrival — the click and `expect(#board-view).toBeVisible()` — are the \
         start of a race, not the end of one (SH-222). Call \
         `openProject(page, \"<project name>\")` from ./support instead, which \
         clicks and then waits for the data."
    );
}
