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

    let specs: Vec<(String, String)> = listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .filter_map(|path| {
            let relative = std::str::from_utf8(path).expect("a UTF-8 path").to_string();
            let text = std::fs::read_to_string(root.join(&relative))
                .unwrap_or_else(|e| panic!("reading {relative}: {e}"));
            creates_stories(&text).then_some((relative, text))
        })
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
