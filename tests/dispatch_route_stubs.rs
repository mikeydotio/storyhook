//! No e2e spec may stub the dispatch endpoint with a glob wide enough to
//! swallow a sibling route (SH-361).
//!
//! # The hazard
//!
//! Nine call sites across six spec files stubbed dispatch with
//! `page.route("**/dispatch**", …)`. That glob is anchored on nothing: it
//! matches any URL whose path contains `dispatch` **followed by anything at
//! all**, so `GET /api/dispatch-log` matched it too and was answered with a
//! `DispatchEnvelope` — a body shaped `{result, dispatch}` where the log
//! reader expects `{result, dispatches, retention}`. Every one of those
//! specs would have quietly broken the new route inside its own test, and
//! the failure would have read as a bug in the dashboard rather than as a
//! stub reaching too far.
//!
//! **Two council seats found this independently**, from different
//! directions, before the route existed — which is why the fix is the class
//! rather than the sighting. Renaming the route away from `dispatch` would
//! have worked exactly once and taught nobody anything; the next
//! dispatch-adjacent path would have hit it again.
//!
//! # Why a scan, and why derived
//!
//! Derived over `git ls-files` rather than a hand-maintained list of the
//! files that stub dispatch, in the SH-198 / SH-260 / SH-364 style, for the
//! reason those stories each paid for: a hand-maintained list is exactly
//! what let ten dead `pub` items, two unbuildable platform arms and
//! fourteen migrations' worth of a wrong `events.kind` spelling go
//! uncounted. A spec file added tomorrow is covered with no edit here.
//!
//! # What this does NOT claim
//!
//! It reads source text, so it proves the *shape* of the glob, never that
//! Playwright's matcher behaves as expected. The behavioural claim — that
//! the log route survives alongside a dispatch stub — belongs to
//! `e2e/specs/dispatch-log.spec.ts`, which stubs dispatch and reads the log
//! in the same test. This scan's job is to stop the next author
//! reintroducing the hazard that spec would then catch expensively, in a
//! nine-minute suite, with a confusing message.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Every tracked `e2e/specs/*.ts`, resolved through git so an untracked
/// scratch spec in somebody's worktree can neither fail this nor satisfy it.
fn tracked_spec_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = Command::new("git")
        .current_dir(root)
        .args(["ls-files", "e2e/specs"])
        .output()
        .expect("running `git ls-files`");
    assert!(
        output.status.success(),
        "`git ls-files` failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git ls-files emits utf-8 paths here")
        .lines()
        .filter(|line| line.ends_with(".ts"))
        .map(|line| root.join(line))
        .collect()
}

/// The glob every dispatch stub must use: anchored on the per-story path
/// segment, so it cannot reach a daemon-scoped sibling.
const ANCHORED: &str = "\"**/story/*/dispatch**\"";

/// The unanchored shape this test exists to keep out.
const UNANCHORED: &str = "\"**/dispatch**\"";

#[test]
fn no_spec_stubs_dispatch_with_a_glob_that_reaches_a_sibling_route() {
    let mut offenders = Vec::new();
    let mut anchored_seen = 0usize;
    for path in tracked_spec_files() {
        let source = std::fs::read_to_string(&path).expect("reading a tracked spec");
        for (n, line) in source.lines().enumerate() {
            if line.contains(UNANCHORED) {
                offenders.push(format!("{}:{}", path.display(), n + 1));
            }
            if line.contains(ANCHORED) {
                anchored_seen += 1;
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "these dispatch stubs use `{UNANCHORED}`, which also matches \
         `/api/dispatch-log` and would answer it with a DispatchEnvelope. \
         Use `{ANCHORED}`:\n  {}",
        offenders.join("\n  ")
    );

    // The positive control, and it is not decoration: without it, a parser
    // that stopped finding anything at all — a moved directory, a renamed
    // extension, a `git ls-files` that returned nothing — would report a
    // clean tree and pass forever. SH-364 wrote this rule down after a
    // fixture-vocabulary scan could have failed the same way.
    assert!(
        anchored_seen >= 9,
        "expected at least the nine known anchored dispatch stubs, found \
         {anchored_seen} — this scan has probably stopped reading the specs \
         at all rather than found a clean tree"
    );
}
