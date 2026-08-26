//! The claiming flag `story next` used to carry is retired, and no tracked
//! file may spell it (SH-477).
//!
//! The claim epic (SH-475) exists because claiming had grown three spellings
//! and only one of them was atomic. Collapsing them onto `story claim
//! (<id> | --next)` is only half the cure: the other half is that the retired
//! spelling stops appearing in text a reader will believe. A usage line, a
//! help topic, a plugin comment or a doc comment naming a flag the parser
//! refuses is a docs-versus-behaviour mismatch, and this project prices that
//! at the severity of the capability it misdescribes rather than as a docs
//! nit (SH-478's own measurement of `[plugin].tracking`).
//!
//! Derived over `git ls-files` rather than a hand-maintained list of the
//! places that mention it — the shape that has already cost this project five
//! times (SH-136, SH-198, SH-258, SH-260/276, SH-360) — so a file added later
//! is covered with no edit here.
//!
//! # Why the needle is assembled at run time
//!
//! Spelled as one literal, this file would be its own first violation. The
//! same trick `tests/council_citations.rs` uses for the same reason: build the
//! forbidden string from pieces so the source never contains it, which also
//! means this test needs no exemption of its own and cannot be quietly
//! neutered by adding one. That holds for the prose here as much as for the
//! code, which is why this file names the flag by description throughout and
//! never writes it out.
//!
//! It found this the hard way, and the mechanism is worth stating because it
//! generalises: run before `git add`, the scan does not see itself at all, and
//! reports a clean tree while the file sits in violation. `git ls-files` is
//! the corpus, so **a fence derived from it proves nothing about its own
//! source until that source is staged.** Every scan in this style shares the
//! blind spot; the gate closes it, because the gate runs on committed content.
//!
//! # What this does not check
//!
//! Only this one retired spelling. The general form — every flag named in a
//! shipped help topic's usage line is one the parser accepts, which is what
//! `tests/readme_command_reference.rs` already does for README.md and nothing
//! does for `src/help_topics.rs` — is filed separately (SH-489). This test
//! would not have caught a *different* dead flag, and says so rather than
//! reading as broader coverage than it has.

use std::collections::BTreeMap;
use std::path::Path;

/// The one file allowed to spell it: a changelog records what a release
/// *did*, and the entries announcing the flag and then removing it are both
/// true statements about the past. Erasing them would make the history lie.
const HISTORY: &str = "CHANGELOG.md";

/// The retired spelling, assembled so this file never contains it.
fn retired_spelling() -> String {
    format!("next {}claim", "--")
}

/// Every tracked file's text, keyed by its path relative to the repository
/// root. Files that are not valid UTF-8 are skipped: a flag spelling cannot
/// hide in a binary.
fn tracked_files(root: &Path) -> BTreeMap<String, String> {
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z"])
        .output()
        .expect("listing this repository's tracked files");
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so this scan proved nothing: {}",
        String::from_utf8_lossy(&listed.stderr)
    );

    listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .filter_map(|path| {
            let relative = std::str::from_utf8(path).ok()?.to_string();
            let text = std::fs::read_to_string(root.join(&relative)).ok()?;
            Some((relative, text))
        })
        .collect()
}

#[test]
fn no_tracked_file_outside_the_changelog_spells_the_retired_flag() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let corpus = tracked_files(root);
    let needle = retired_spelling();

    // Positive controls. Every assertion below is about what the scan did
    // *not* find, and a reader that silently stopped reading files would
    // satisfy all of them.
    assert!(
        corpus.len() > 100,
        "the corpus is implausibly small ({} files) — the scan proved nothing",
        corpus.len()
    );
    assert!(
        corpus.contains_key("src/cli.rs"),
        "a known tracked file is missing from the corpus"
    );
    assert!(
        corpus
            .get(HISTORY)
            .is_some_and(|text| text.contains(&needle)),
        "{HISTORY} no longer contains the retired spelling, so this scan can \
         no longer tell a working reader from a broken one — if the changelog \
         entry was genuinely removed, retire this test with it"
    );

    let offenders: Vec<&str> = corpus
        .iter()
        .filter(|(path, _)| path.as_str() != HISTORY)
        .filter(|(_, text)| text.contains(&needle))
        .map(|(path, _)| path.as_str())
        .collect();

    assert!(
        offenders.is_empty(),
        "`story {needle}` was removed by SH-477 and the parser now refuses it \
         as an unknown flag. These files still name it, so a reader who \
         follows them gets exit 2:\n  {}\n\nClaiming is `story claim <id>` or \
         `story claim --next`.",
        offenders.join("\n  ")
    );
}
