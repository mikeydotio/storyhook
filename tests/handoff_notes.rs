//! Durable work context belongs in StoryHook, not in a tracked session note
//! (SH-588) — and a gate over that rule asks git, never the filesystem (SH-621).
//!
//! `HANDOFF.md` is a local, temporary artifact: it exists for the duration of a
//! handoff from one agent to the next and for no longer. That is why it is
//! gitignored, and why the property this file pins is *tracking*, not
//! *existence*. SH-588's first version of this test asserted the file was not
//! on disk at all, which turned the operator's own standing rule ("if work
//! remains, write HANDOFF.md") into a red battery after a full compile — a gate
//! refusing the artifact's whole purpose. An ignored file's job is to exist
//! locally; the repository's job is to never commit it.
//!
//! So both questions below go to git, in the checkout this test binary was
//! built from: is the root `HANDOFF.md` in the index, and do the repository's
//! own ignore rules cover it, whatever their spelling. Neither question reads
//! the file, so a local copy — the case SH-621 was filed over — is invisible
//! here.

use std::path::Path;
use std::process::Command;

/// Runs one `git` query in the checkout root and returns its exit status,
/// failing the test by name when git could not answer at all (no repository,
/// a broken cwd) — a positive control, so the assertions below cannot pass
/// vacuously on an error that merely looks like the verdict they expect.
fn git_verdict(root: &Path, args: &[&str]) -> bool {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("running `git {}`: {e}", args.join(" ")));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("not a git repository"),
        "`git {}` did not answer, so this check proved nothing: {stderr}",
        args.join(" ")
    );
    output.status.success()
}

#[test]
fn the_repository_keeps_root_handoff_notes_local() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    // `--error-unmatch` fails for a path that is not in the index; success
    // therefore means the note has been committed or staged.
    assert!(
        !git_verdict(root, &["ls-files", "--error-unmatch", "--", "HANDOFF.md"]),
        "HANDOFF.md is tracked; durable handoff context belongs in story comments — \
         `git rm --cached HANDOFF.md` and keep the note local"
    );

    // `check-ignore -q` succeeds only when the repository's own rules ignore
    // the path. It also reports a tracked path as *not* ignored, so this
    // overlaps the assertion above on purpose, with its own message.
    assert!(
        git_verdict(root, &["check-ignore", "-q", "--", "HANDOFF.md"]),
        "the root HANDOFF.md is not covered by this repository's ignore rules; \
         it is a local, temporary handoff artifact and must stay ignored"
    );
}
