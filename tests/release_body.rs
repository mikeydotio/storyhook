//! `scripts/render-release-body.sh` — the release body, rendered from the
//! changelog rather than left to GitHub's generated notes.
//!
//! GitHub bases generated release notes on the newest **tag**, published or
//! not. `v2.1.0` was tagged and never published (the release build failed on
//! one target — SH-259), so the notes GitHub would generate for the next
//! release cover `v2.1.0...v2.1.1`: six pull requests, while a user upgrading
//! from the newest release they could actually install receives seven hundred
//! and forty-three commits. The generated notes are not wrong about what they
//! claim to describe; they describe the wrong span, and they do it silently
//! (SH-257's council, condition 2).
//!
//! So the body is rendered from `CHANGELOG.md` against the newest **published**
//! release — the one `install.sh` and `story update` both resolve through
//! `/releases/latest` — and the renderer refuses rather than emitting something
//! plausible when it cannot do that honestly. These tests are that refusal's
//! gate, and they run on any platform because the renderer is a text
//! transformation: the workflow supplies the one fact it cannot derive (which
//! release is published), and nothing here reaches the network.

use std::path::{Path, PathBuf};
use std::process::Output;
use tempfile::TempDir;

/// The repository root, which is this package's manifest directory.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The renderer under test.
fn renderer() -> PathBuf {
    repo_root().join("scripts/render-release-body.sh")
}

/// Runs the renderer, returning its raw output. Failures to *spawn* panic —
/// they mean the script is missing or unreadable, which is a finding rather
/// than a red assertion.
fn render(arguments: &[&str]) -> Output {
    std::process::Command::new("bash")
        .arg(renderer())
        .args(arguments)
        .current_dir(repo_root())
        .output()
        .expect("running scripts/render-release-body.sh")
}

/// Runs the renderer and insists it succeeded, returning stdout.
fn render_ok(arguments: &[&str]) -> String {
    let output = render(arguments);
    assert!(
        output.status.success(),
        "the renderer failed on {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body = String::from_utf8(output.stdout).expect("a UTF-8 release body");
    assert!(
        !body.trim().is_empty(),
        "the renderer succeeded and printed nothing, which would publish an \
         empty release body: {arguments:?}"
    );
    body
}

/// Runs the renderer and insists it refused, returning stderr. A renderer that
/// exits 0 on bad input is worse than one that crashes: the workflow publishes
/// whatever it printed.
fn render_refused(arguments: &[&str]) -> String {
    let output = render(arguments);
    assert!(
        !output.status.success(),
        "the renderer accepted {arguments:?} and printed:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8(output.stderr).expect("a UTF-8 diagnostic")
}

/// A changelog with a published release (`v2.0.0`), a version tagged but never
/// published above it (`v2.1.0`, carrying a breaking change), and the release
/// being cut (`v3.0.0`). Each section carries a line unique to it, so a test
/// can tell "linked" from "pasted".
const CHANGELOG: &str = "\
# Changelog

All notable changes to this project will be documented in this file.

## [v3.0.0] - 2026-09-01

### Added
- the newest thing (aaaaaaa)

## [v2.1.0] - 2026-08-12

### Breaking
- the CLI reaches the store only through the daemon (bbbbbbb)

### Added
- a middle thing (ccccccc)

## [v2.0.0] - 2026-08-01

### Changed
- an oldest thing (ddddddd)
";

/// Writes `contents` to a changelog inside a fresh directory, returning both.
/// The directory is returned so the caller keeps it alive — dropping it deletes
/// the file out from under the renderer.
fn changelog_of(contents: &str) -> (TempDir, PathBuf) {
    let directory = storyhook_test_support::scratch_dir_named("release-body");
    let path = directory.path().join("CHANGELOG.md");
    std::fs::write(&path, contents).expect("writing the fixture changelog");
    (directory, path)
}

/// The fixture changelog, ready to pass as `--changelog`.
fn fixture() -> (TempDir, PathBuf) {
    changelog_of(CHANGELOG)
}

/// Arguments for a render against `path`, spelled once.
fn arguments<'a>(path: &'a Path, version: &'a str, since: Option<&'a str>) -> Vec<&'a str> {
    let mut arguments = vec![
        "--changelog",
        path.to_str().expect("a UTF-8 fixture path"),
        "--repo",
        "mikeydotio/storyhook",
        "--version",
        version,
    ];
    if let Some(since) = since {
        arguments.push("--since");
        arguments.push(since);
    }
    arguments
}

// ---------------------------------------------------------------------------
// What the body carries
// ---------------------------------------------------------------------------

/// The body is the released version's own changelog section. Not a summary of
/// it, and not the whole file.
#[test]
fn the_released_versions_own_section_is_the_body() {
    let (_directory, path) = fixture();
    let body = render_ok(&arguments(&path, "v3.0.0", Some("v2.1.0")));

    assert!(
        body.contains("- the newest thing (aaaaaaa)"),
        "the released version's own entries are missing from the body:\n{body}"
    );
    assert!(
        !body.contains("an oldest thing"),
        "the body reaches back past the newest published release:\n{body}"
    );
}

/// The gap is the whole point. A version tagged since the newest published
/// release ships to users *in this release*, and generated notes never say so,
/// because GitHub counts from the tag rather than from the release.
#[test]
fn a_version_tagged_but_never_published_is_named() {
    let (_directory, path) = fixture();
    let body = render_ok(&arguments(&path, "v3.0.0", Some("v2.0.0")));

    assert!(
        body.contains("v2.1.0"),
        "v2.1.0 was tagged after the newest published release and is not named \
         in the body, so this release silently delivers it:\n{body}"
    );
    assert!(
        body.contains("v2.0.0"),
        "the body does not say which release it is measuring from:\n{body}"
    );
}

/// Prereleases can be published without becoming the stable release returned
/// by `/releases/latest`. The body must describe that channel distinction
/// truthfully and must never present the changelog's `Unreleased` bucket as a
/// tag the reader can follow.
#[test]
fn prerelease_history_names_the_stable_channel_and_excludes_unreleased() {
    let (_directory, path) = changelog_of(
        "# Changelog\n\n\
         ## [v3.0.0] - 2026-09-03\n\n### Added\n- stable release\n\n\
         ## [v2.3.0-beta.1] - 2026-09-02\n\n### Added\n- beta release\n\n\
         ## [Unreleased]\n\n### Added\n- future work\n\n\
         ## [v2.2.0] - 2026-08-24\n\n### Added\n- prior stable\n",
    );
    let body = render_ok(&arguments(&path, "v3.0.0", Some("v2.2.0")));

    assert!(
        body.contains("stable `/releases/latest` channel"),
        "published prerelease history must be described by the actual stable-channel invariant:\n{body}"
    );
    assert!(
        !body.contains("never published"),
        "a published prerelease must not be called unpublished:\n{body}"
    );
    assert!(
        !body.contains("[Unreleased]") && !body.contains("#unreleased"),
        "the Unreleased bucket is not a tagged intermediate version:\n{body}"
    );
}

/// Named, and linked to its changelog anchor at the tag being released — so
/// the link keeps working after `main` moves on.
#[test]
fn a_skipped_version_links_to_its_changelog_anchor() {
    let (_directory, path) = fixture();
    let body = render_ok(&arguments(&path, "v3.0.0", Some("v2.0.0")));

    assert!(
        body.contains(
            "https://github.com/mikeydotio/storyhook/blob/v3.0.0/CHANGELOG.md#v210---2026-08-12"
        ),
        "the skipped version has no anchored link into the changelog at the \
         released tag:\n{body}"
    );
}

/// Linked, and deliberately not pasted: the section this stands in for ran to
/// roughly seven hundred and sixty lines, which is a release body nobody
/// reads (SH-257's council, condition 4).
#[test]
fn a_skipped_versions_entries_are_linked_rather_than_pasted() {
    let (_directory, path) = fixture();
    let body = render_ok(&arguments(&path, "v3.0.0", Some("v2.0.0")));

    assert!(
        !body.contains("a middle thing"),
        "the skipped version's entries were pasted into the body rather than \
         linked:\n{body}"
    );
}

/// A breaking change anywhere in the span crosses the upgrade, even when the
/// release being cut is itself a patch. The disclosure is scoped to the reader
/// it is true for — someone upgrading from the newest published release —
/// rather than claiming the release itself is breaking (condition 3).
#[test]
fn a_breaking_change_among_the_skipped_versions_is_disclosed() {
    let (_directory, path) = fixture();
    let body = render_ok(&arguments(&path, "v3.0.0", Some("v2.0.0")));

    assert!(
        body.contains("Breaking changes"),
        "v2.1.0 carries a `### Breaking` section and the body does not \
         disclose it:\n{body}"
    );
    assert!(
        body.contains("v2.1.0"),
        "the breaking disclosure does not say which version carries it:\n{body}"
    );
}

/// The same disclosure when the breaking change is in the release itself,
/// pointing at the entries below it rather than at another version.
#[test]
fn a_breaking_change_in_the_release_itself_is_disclosed() {
    let (_directory, path) = fixture();
    let body = render_ok(&arguments(&path, "v2.1.0", Some("v2.0.0")));

    assert!(
        body.contains("Breaking changes"),
        "the release's own `### Breaking` section is not disclosed:\n{body}"
    );
    assert!(
        body.contains("below"),
        "the disclosure should point at the entries in this very body:\n{body}"
    );
}

/// Nothing skipped and nothing breaking: no preamble at all. A body that opens
/// with a reassurance on every release trains the reader to skip the line that
/// matters.
#[test]
fn a_release_with_no_gap_and_no_break_carries_no_preamble() {
    let (_directory, path) = fixture();
    let body = render_ok(&arguments(&path, "v3.0.0", Some("v2.1.0")));

    assert!(
        !body.contains("never published"),
        "nothing was skipped, so nothing should be disclosed:\n{body}"
    );
    assert!(
        !body.contains("Breaking changes"),
        "v3.0.0 carries no breaking entries:\n{body}"
    );
    assert!(
        body.trim_start().starts_with("###"),
        "with nothing to disclose the body should open on the changelog \
         entries themselves:\n{body}"
    );
}

/// The first release of a repository has nothing published to measure from.
/// That is a first release, not a failure.
#[test]
fn a_first_release_renders_without_a_published_predecessor() {
    let (_directory, path) = fixture();
    let body = render_ok(&arguments(&path, "v2.0.0", None));

    assert!(
        body.contains("- an oldest thing (ddddddd)"),
        "the body should still be the released version's section:\n{body}"
    );
    assert!(
        !body.contains("never published"),
        "there is no published predecessor to have skipped anything:\n{body}"
    );
}

// ---------------------------------------------------------------------------
// What it refuses
// ---------------------------------------------------------------------------

/// The guard the whole story turns on. A tag with no changelog section means
/// the version was never bumped, and the alternative to refusing is a release
/// whose body is whatever the fallback produced.
#[test]
fn a_version_with_no_changelog_section_is_refused() {
    let (_directory, path) = fixture();
    let complaint = render_refused(&arguments(&path, "v9.9.9", Some("v2.0.0")));

    assert!(
        complaint.contains("v9.9.9"),
        "the refusal must name the version it could not find: {complaint}"
    );
}

/// A section that exists but says nothing is the same failure wearing a
/// heading.
#[test]
fn an_empty_section_is_refused() {
    let (_directory, path) = changelog_of(
        "# Changelog\n\n## [v3.0.0] - 2026-09-01\n\n## [v2.0.0] - 2026-08-01\n\n### Changed\n- a thing (ddddddd)\n",
    );
    let complaint = render_refused(&arguments(&path, "v3.0.0", Some("v2.0.0")));

    assert!(
        complaint.contains("v3.0.0"),
        "the refusal must name the empty section: {complaint}"
    );
}

/// If the newest published release is not in the changelog, the span cannot be
/// computed — and a body that quietly covers the wrong span is exactly what
/// this renderer exists to prevent.
#[test]
fn a_published_predecessor_missing_from_the_changelog_is_refused() {
    let (_directory, path) = fixture();
    let complaint = render_refused(&arguments(&path, "v3.0.0", Some("v1.2.3")));

    assert!(
        complaint.contains("v1.2.3"),
        "the refusal must name the predecessor it could not place: {complaint}"
    );
}

/// The predecessor has to be older than the release. Equal means the tag is
/// being re-cut; newer means the changelog and the tags disagree about
/// ordering. Neither has an honest body.
#[test]
fn a_predecessor_that_is_not_older_is_refused() {
    let (_directory, path) = fixture();

    // Re-cutting a tag that is already the published release.
    render_refused(&arguments(&path, "v3.0.0", Some("v3.0.0")));
    // A predecessor newer than the release being cut.
    render_refused(&arguments(&path, "v2.0.0", Some("v3.0.0")));
}

/// Both remaining inputs are required, because guessing either produces a body
/// that looks finished and is wrong: the version decides the content, the
/// repository decides where the links point.
#[test]
fn the_required_inputs_are_refused_when_missing() {
    let (_directory, path) = fixture();
    let changelog = path.to_str().expect("a UTF-8 fixture path");

    render_refused(&["--changelog", changelog, "--repo", "mikeydotio/storyhook"]);
    render_refused(&["--changelog", changelog, "--version", "v3.0.0"]);
}

/// A changelog that cannot be read is a finding, not an empty body.
#[test]
fn an_unreadable_changelog_is_refused() {
    let complaint = render_refused(&[
        "--changelog",
        "/nonexistent/CHANGELOG.md",
        "--repo",
        "mikeydotio/storyhook",
        "--version",
        "v3.0.0",
    ]);

    assert!(
        complaint.contains("CHANGELOG"),
        "the refusal must name the file it could not read: {complaint}"
    );
}

// ---------------------------------------------------------------------------
// Against the real repository
// ---------------------------------------------------------------------------

/// The fixtures above prove the renderer's rules; this proves the rules match
/// the file they will actually run against. A bump that writes `VERSION`
/// without a changelog section would publish a release whose body could not be
/// rendered — and it would find that out after the tag was pushed and could no
/// longer be moved.
#[test]
fn the_repository_changelog_renders_for_the_version_being_shipped() {
    let version = std::fs::read_to_string(repo_root().join("VERSION"))
        .expect("VERSION must be readable")
        .trim()
        .to_string();

    let body = render_ok(&[
        "--repo",
        "mikeydotio/storyhook",
        "--version",
        &version,
        "--changelog",
        repo_root()
            .join("CHANGELOG.md")
            .to_str()
            .expect("a UTF-8 changelog path"),
    ]);

    assert!(
        body.contains("###"),
        "the body rendered from the real changelog carries no entries:\n{body}"
    );
}

/// The renderer defaults to this repository's own changelog, so the workflow
/// does not have to spell a path that only means one thing.
#[test]
fn the_changelog_defaults_to_this_repositorys_own() {
    let version = std::fs::read_to_string(repo_root().join("VERSION"))
        .expect("VERSION must be readable")
        .trim()
        .to_string();

    render_ok(&["--repo", "mikeydotio/storyhook", "--version", &version]);
}
