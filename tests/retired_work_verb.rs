//! The helper's claiming verb, the skill that fronted it, and the `[plugin]`
//! key it read are retired, and no tracked file may spell any of them
//! (SH-478).
//!
//! The sibling of [`tests/retired_next_claim.rs`], and for the same reason:
//! collapsing claiming onto `story claim (<id> | --next)` is only half the
//! cure, the other half being that the retired spellings stop appearing in
//! text a reader will believe. A router row, a README table, a skill's prose
//! or a scaffolded config comment naming a verb the script refuses, a skill
//! directory that is gone, or a config key nothing reads is a
//! docs-versus-behaviour mismatch — priced at the severity of the capability
//! it misdescribes rather than as a docs nit, which is this story's own
//! measurement of the config key below: a three-valued documented knob with
//! one binary effect at one call site and a third level that never had an
//! implementation at all.
//!
//! Derived over `git ls-files` rather than a hand-maintained list of the
//! places that mention them — the shape that has already cost this project
//! five times (SH-136, SH-198, SH-258, SH-260/276, SH-360) — so a file added
//! later is covered with no edit here.
//!
//! # Why the needles are assembled at run time
//!
//! Spelled as literals, this file would be its own first violation. Same
//! trick, same reason as [`tests/retired_next_claim.rs`] and
//! `tests/council_citations.rs`: build each forbidden string from pieces so
//! the source never contains it, which also means this test needs no
//! exemption of its own and cannot be quietly neutered by adding one. That
//! holds for the prose here as much as for the code, which is why this file
//! names the retired verb, skill and key by description throughout.
//!
//! The blind spot [`tests/retired_next_claim.rs`] documents applies here
//! unchanged: `git ls-files` is the corpus, so run before `git add` this scan
//! does not see itself and reports a clean tree while the file sits in
//! violation. The gate closes it, because the gate runs on committed content.
//!
//! # What this does not check
//!
//! Only these three retired spellings, and only as text. It says nothing
//! about whether the replacement route *works* — that is
//! `plugins/story/tests/test-claim-route.sh`, which asserts the router's
//! prescribed invocation against the real CLI, and SH-476's own suite, which
//! owns claiming itself.

use std::collections::BTreeMap;
use std::path::Path;

/// The one file allowed to spell them: a changelog records what a release
/// *did*, and the entries announcing these and then removing them are both
/// true statements about the past. Erasing them would make the history lie.
const HISTORY: &str = "CHANGELOG.md";

/// One retired spelling and the sentence explaining what replaced it.
struct Retired {
    /// Assembled at run time so this file never contains it.
    needle: String,
    /// What a reader who followed the retired spelling should do instead.
    replacement: &'static str,
}

/// Every spelling this story retired.
///
/// The helper subcommand and the skill directory are the two a reader could
/// literally type or open; the config key is the one that would be *authored*
/// into a repository's committed pointer file on the strength of stale docs.
fn retired_spellings() -> Vec<Retired> {
    vec![
        Retired {
            needle: format!("story.sh {}", "work"),
            replacement: "claiming is `story claim <id>` or `story claim --next`, which the \
                          /story router runs directly",
        },
        Retired {
            needle: format!("story{}work", "-"),
            replacement: "the skill retired with the verb; the router's own `claim` route \
                          replaces it",
        },
        Retired {
            needle: format!("[plugin].{}", "tracking"),
            replacement: "`[plugin].enabled` is the table's only key; whether a claim comments \
                          is `story claim --comment <text>` or `--no-comment`",
        },
    ]
}

/// Every tracked file's text, keyed by its path relative to the repository
/// root. Files that are not valid UTF-8 are skipped: a verb spelling cannot
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
fn no_tracked_file_outside_the_changelog_spells_a_retired_work_verb() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let corpus = tracked_files(root);
    let retired = retired_spellings();

    // Positive controls. Every assertion below is about what the scan did
    // *not* find, and a reader that silently stopped reading files would
    // satisfy all of them.
    assert!(
        corpus.len() > 100,
        "the corpus is implausibly small ({} files) — the scan proved nothing",
        corpus.len()
    );
    assert!(
        corpus.contains_key("plugins/story/bin/story.sh"),
        "a known tracked file is missing from the corpus"
    );
    let helper_verb = &retired[0].needle;
    assert!(
        corpus
            .get(HISTORY)
            .is_some_and(|text| text.contains(helper_verb)),
        "{HISTORY} no longer contains the retired helper subcommand, so this scan can no \
         longer tell a working reader from a broken one — if the changelog entries were \
         genuinely removed, retire this test with them"
    );

    let mut failures = Vec::new();
    for Retired {
        needle,
        replacement,
    } in &retired
    {
        let offenders: Vec<&str> = corpus
            .iter()
            .filter(|(path, _)| path.as_str() != HISTORY)
            .filter(|(_, text)| text.contains(needle))
            .map(|(path, _)| path.as_str())
            .collect();
        if !offenders.is_empty() {
            failures.push(format!(
                "`{needle}` was retired by SH-478 — {replacement}. Still named by:\n    {}",
                offenders.join("\n    ")
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "these files name spellings that no longer exist, so a reader who follows them is \
         misled:\n  {}",
        failures.join("\n  ")
    );
}
