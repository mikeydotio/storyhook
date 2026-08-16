//! No tracked file cites a council audit trail by its local directory slug (SH-363).
//!
//! The council-vote skill writes its trail to a slug directory under a gitignored
//! plugin-state folder, relative to whatever directory the agent happened to be
//! standing in. Since every story is worked in a linked worktree that is torn down
//! on completion, a council convened inside one is deleted with it — while the code
//! and comments citing it survive. Measured when SH-363 was filed: 81 such citations
//! across 51 tracked files, of which 32 already resolved to nothing, and 19 distinct
//! slugs were gone for good.
//!
//! The obvious pin — "every cited slug directory exists" — is the one thing this
//! cannot assert. That directory is untracked, so a fresh clone has none of it: the
//! test would fail everywhere except the one machine that wrote the trails, or it
//! would have to go quiet when the folder is absent, which is a vacuous pass. So the
//! rule is the inverse, and it is purely lexical: **a slug may not follow the
//! slash.** A bare marker is fine — this file, the ignore entry that makes the folder
//! untracked, and CLAUDE.md's own statement of the rule all need to name it — because
//! a bare mention promises nothing. A slug is a locator into a directory no reader of
//! this repository can have, and that is the promise being broken.
//!
//! What replaces a citation is on the story: `story show SH-N` carries the verdict,
//! which is the copy that has actually survived (SH-339 reconstructed SH-326's
//! reasoning from a story comment after its trail was gone). Where a slug was already
//! dead, the repair states the verdict inline instead of delegating to a story that
//! carries nothing — a pointer that fails silently is worse than the dead path it
//! replaced, which at least failed loudly.
//!
//! Derived over `git ls-files`, in the style of `tests/dead_public_surface.rs` and
//! `tests/store_isolation.rs`, rather than from a list of the sites that do this — a
//! hand-maintained list is exactly what let ten dead items accumulate in SH-198.
//!
//! **This file needs no exemption for itself**, and that is deliberate. Its fixtures
//! are assembled from [`MARKER`] at run time, so the marker never appears adjacent to
//! a slug in the source text. A test that had to exempt itself would be one edit away
//! from exempting everything.
//!
//! Full deliberation: `story show SH-363`. Two councils, both unanimous — the second
//! convened after the first verdict's salvage step turned out to need a release that
//! does not exist yet (SH-369, SH-370).

use std::collections::BTreeMap;
use std::path::Path;

/// The marker a citation starts with.
///
/// Held as a constant and joined at run time so that this file's own fixtures never
/// contain the marker followed by a slug, which is the thing the scan refuses.
const MARKER: &str = ".council/";

/// The comment leaders a wrapped citation may resume behind.
///
/// Longest first: `//` is a prefix of both `///` and `//!`, and stripping the short
/// one first would leave a `/` that halts collection and silently truncate the slug.
const COMMENT_LEADERS: [&str; 6] = ["//!", "///", "//", "--", "*", "#"];

/// One citation: where it was found and the slug it names.
#[derive(Debug, PartialEq, Eq)]
struct Citation {
    /// 1-indexed line the marker itself appeared on, not where the slug ended.
    line: usize,
    /// The reconstructed slug, joined back across any line wrapping.
    slug: String,
}

/// Whether a character may appear in a slug.
///
/// Deliberately narrower than a filesystem path: a slash would swallow the rest of a
/// cited path (`<slug>/DECISION.md`), and the finding is about the slug, not the file
/// under it.
fn is_slug_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-'
}

/// A continuation line with its leading whitespace and comment leader removed, or
/// `None` if it carries no leader.
///
/// The leader is **required**, and that is the whole guard against over-reaching. A
/// citation wraps inside a comment block, so the next line always carries one; an
/// ignore-file entry sitting directly beneath a bare marker does not, and joining
/// those two would invent a citation nobody wrote.
fn resume_after_leader(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    COMMENT_LEADERS
        .iter()
        .find_map(|leader| trimmed.strip_prefix(leader))
        .map(str::trim_start)
}

/// Every council citation in one file's text.
///
/// A slug may be broken across lines — this corpus wraps them at a hyphen and, once,
/// immediately after the slash — so collection continues onto the next line when the
/// slug so far is empty or ends in a hyphen *and* that line resumes behind a comment
/// leader. Both conditions are load-bearing: without the first, a marker followed by
/// unrelated prose would absorb it; without the second, two adjacent ignore-file
/// entries would be read as one wrapped citation.
fn citations(text: &str) -> Vec<Citation> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();

    for (index, line) in lines.iter().enumerate() {
        let mut search_from = 0;
        while let Some(offset) = line[search_from..].find(MARKER) {
            let after = search_from + offset + MARKER.len();
            search_from = after;

            let mut slug: String = line[after..]
                .chars()
                .take_while(|c| is_slug_char(*c))
                .collect();
            let mut consumed_to_end_of_line = after + slug.len() == line.len();
            let mut next = index + 1;

            while consumed_to_end_of_line
                && (slug.is_empty() || slug.ends_with('-'))
                && let Some(resumed) = lines.get(next).and_then(|l| resume_after_leader(l))
            {
                let continued: String = resumed.chars().take_while(|c| is_slug_char(*c)).collect();
                if continued.is_empty() {
                    break;
                }
                consumed_to_end_of_line = continued.len() == resumed.len();
                slug.push_str(&continued);
                next += 1;
            }

            if !slug.is_empty() {
                found.push(Citation {
                    line: index + 1,
                    slug,
                });
            }
        }
    }

    found
}

/// Every tracked file's text, keyed by its path relative to the repository root.
///
/// Files that are not valid UTF-8 are skipped rather than failing the scan: this
/// corpus is every tracked file, not just source, and a citation cannot hide in a
/// binary.
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
fn no_tracked_file_cites_a_council_trail_by_its_local_slug() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let corpus = tracked_files(root);

    // Positive control on the corpus itself. Every assertion below is about what the
    // scan did *not* find, and a reader that silently stopped reading files would
    // satisfy all of them. These three say the reader still works: the tree is
    // populated, a known file is in it, and the marker the scan looks for is still
    // present somewhere — bare, which is exactly what the rule permits.
    assert!(
        corpus.len() > 100,
        "only {} tracked files were read; this scan proved nothing",
        corpus.len()
    );
    assert!(
        corpus.contains_key("CLAUDE.md"),
        "CLAUDE.md was not in the corpus; this scan proved nothing"
    );
    assert!(
        corpus.values().any(|text| text.contains(MARKER)),
        "no tracked file contains the marker at all — either the ignore entry and the \
         rule's own statement of itself have gone, or this scan stopped reading files"
    );

    let findings: Vec<String> = corpus
        .iter()
        .flat_map(|(path, text)| {
            citations(text)
                .into_iter()
                .map(move |c| format!("{path}:{}: cites `{}`", c.line, c.slug))
        })
        .collect();

    assert!(
        findings.is_empty(),
        "a council trail is cited by its local directory slug, which resolves on at most \
         one machine and on no fresh clone. Cite the verdict on its story (`story show \
         SH-N`) instead, or state it inline where the trail is already gone (SH-363):\n{}",
        findings.join("\n")
    );
}

#[test]
fn the_analyzer_reads_every_citation_shape_this_corpus_has_written() {
    // The control the scan above cannot be: a fixture corpus, held in this file so it
    // cannot rot on disk, exercising each shape the tree has actually used. A parser
    // that stopped recognising wrapped citations would still report a clean tree; it
    // fails here instead.

    let bare_in_prose = format!("`{MARKER}` is untracked, so the verdict lives on the story.\n");
    assert_eq!(
        citations(&bare_in_prose),
        vec![],
        "a bare marker promises nothing and must be permitted"
    );

    let ignore_entry = format!(".freshen/\n{MARKER}\n\n# the next stanza\n");
    assert_eq!(
        citations(&ignore_entry),
        vec![],
        "the ignore entry that makes the folder untracked must be permitted"
    );

    let adjacent_entries = format!("{MARKER}\n.freshen/\n");
    assert_eq!(
        citations(&adjacent_entries),
        vec![],
        "two ignore entries are not one wrapped citation — the continuation rule \
         requires a comment leader precisely so this cannot be misread"
    );

    let no_slash = ".council is a jq path expression, not a directory\n";
    assert_eq!(
        citations(no_slash),
        vec![],
        "the marker requires its slash; a bare word is a live jq expression elsewhere \
         in this repository"
    );

    let single_line = format!("see {MARKER}sh49-linked-prs/DECISION.md for the vote\n");
    assert_eq!(
        citations(&single_line),
        vec![Citation {
            line: 1,
            slug: "sh49-linked-prs".to_string()
        }],
        "the plain single-line citation is the shape most of the repaired sites had"
    );

    let wrapped_at_hyphen = format!(
        "  // One contract, two surfaces, keyed on OUTCOME ({MARKER}sh-304-dashboard-notification-\n  \
         // contract/DECISION.md):\n"
    );
    assert_eq!(
        citations(&wrapped_at_hyphen),
        vec![Citation {
            line: 1,
            slug: "sh-304-dashboard-notification-contract".to_string(),
        }],
        "a slug wrapped at a hyphen must be rejoined byte-for-byte, or a scan reports \
         a live directory as absent and a dead one as unremarkable"
    );

    let wrapped_after_slash = format!(
        "/// `StoryCommitLinked` event (SH-70's council, {MARKER}\n\
         /// sh70-import-project-git-link-source/DECISION.md).\n"
    );
    assert_eq!(
        citations(&wrapped_after_slash),
        vec![Citation {
            line: 1,
            slug: "sh70-import-project-git-link-source".to_string(),
        }],
        "one site in this corpus broke the line immediately after the slash, so an \
         empty slug so far must still continue onto a leader-bearing next line"
    );

    let two_on_one_line =
        format!("{MARKER}sh49-linked-prs/ and {MARKER}sh94-daemon-fd-inheritance/\n");
    assert_eq!(
        citations(&two_on_one_line),
        vec![
            Citation {
                line: 1,
                slug: "sh49-linked-prs".to_string()
            },
            Citation {
                line: 1,
                slug: "sh94-daemon-fd-inheritance".to_string()
            },
        ],
        "a line may carry more than one citation and the scan must not stop at the first"
    );
}
