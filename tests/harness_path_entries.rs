//! Every `PATH="…:$PATH"` prepend in this repo's shell scripts names a
//! DIRECTORY (SH-264).
//!
//! A shell silently SKIPS a `PATH` entry that is not a directory — it is not
//! an error, there is no diagnostic, the command just falls through to
//! whatever the next entry (or the system) resolves. That is exactly what
//! happened to `plugins/story/tests/test-dispatch-actor-labels.sh:24`:
//! it prepended `$TESTS_DIR/fakes/tmux`, the fake tmux's own FILE, instead of
//! `$TESTS_DIR/fakes`, the directory holding it. `tmux` in that test resolved
//! to the real `/opt/homebrew/bin/tmux`, and the test passed anyway — the
//! real tmux failed to reach a socket literally named `fake` (from
//! `TMUX="fake,0,0"`) at roughly the point the fake would have refused, so
//! the claim/rollback pair this test asserts on held for a reason unrelated
//! to the one its own comment named. Nothing about that failure mode leaves
//! a trace a human reviewing a diff would catch: the line still parses, the
//! test still goes green.
//!
//! Derived over `git ls-files`, the same style `store_isolation.rs`'s three
//! scans use — a hand-maintained list of "the files that prepend PATH" is
//! exactly the kind of list that goes stale the moment a new fixture is
//! added, which is how the file-valued entry above went unnoticed until read
//! by inspection rather than caught by a failure.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Extraction: find every entry a PATH="...$PATH" line prepends
// ---------------------------------------------------------------------------

/// One `PATH=` prepend entry found in a tracked file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Occurrence {
    line: usize,
    /// The raw entry text, exactly as written (e.g. `$TESTS_DIR/fakes`,
    /// `$FAKE_TMUX_DIR`, `$F_ROOT/bin`) — never the literal `$PATH`/`${PATH}`
    /// placeholder itself, which every prepend also carries and which names
    /// no path at all.
    entry: String,
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Every `PATH="…"` (optionally `export`-prefixed) assignment's entries,
/// across `text`, whose value actually references `$PATH`/`${PATH}` — i.e. a
/// prepend/append onto the existing PATH, not an unrelated full replacement.
///
/// Boundary-checked on the character immediately before the match: `PATH=`
/// must start a genuine identifier, not merely end one
/// (`WORKTREE_IGNORE_PATH=`, `SELF_PATH=`, `CMP_WT_PATH=`, … all appear
/// verbatim in this codebase and must NOT match). Scoped to one line: this
/// codebase never spans a PATH value across a line continuation, so a value
/// missing its closing quote on the same line is skipped rather than guessed
/// at.
fn path_prepend_entries(text: &str) -> Vec<Occurrence> {
    const NEEDLE: &str = "PATH=\"";
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let bytes = line.as_bytes();
        let mut search_from = 0usize;
        while let Some(rel) = line[search_from..].find(NEEDLE) {
            let start = search_from + rel;
            let boundary = start == 0 || !is_ident_byte(bytes[start - 1]);
            let value_start = start + NEEDLE.len();
            if boundary && let Some(end_rel) = line[value_start..].find('"') {
                let value = &line[value_start..value_start + end_rel];
                if value.contains("$PATH") || value.contains("${PATH}") {
                    for entry in value.split(':') {
                        if entry.is_empty() || entry == "$PATH" || entry == "${PATH}" {
                            continue;
                        }
                        out.push(Occurrence {
                            line: line_no,
                            entry: entry.to_string(),
                        });
                    }
                }
            }
            search_from = value_start;
        }
    }
    out
}

#[test]
fn extractor_finds_directory_and_file_valued_spellings_and_ignores_lookalikes() {
    let good = r#"FAKE_TMUX_DIR="$TESTS_DIR/fakes"
PATH="$FAKE_TMUX_DIR:$PATH" bash "$SCRIPT" dispatch "$id"
"#;
    let found = path_prepend_entries(good);
    assert_eq!(
        found,
        vec![Occurrence {
            line: 2,
            entry: "$FAKE_TMUX_DIR".to_string(),
        }]
    );

    // The exact bad spelling SH-264 shipped with: the fake's FILE, not its
    // directory. The extractor's job is only to find the entry -- telling
    // file from directory is resolve_entry's job, exercised below.
    let bad = r#"PATH="$TESTS_DIR/fakes/tmux:$PATH" TMUX="fake,0,0" bash "$SCRIPT" dispatch "$id""#;
    assert_eq!(
        path_prepend_entries(bad),
        vec![Occurrence {
            line: 1,
            entry: "$TESTS_DIR/fakes/tmux".to_string(),
        }]
    );

    // A variable that merely ENDS in PATH, and a PATH value that does not
    // touch $PATH at all, must both be ignored.
    let lookalikes = r#"WORKTREE_IGNORE_PATH="${STORY_WORKTREE_IGNORE_PATH:-.claude/worktrees/}"
SELF_PATH="$(cd "$(dirname "$0")" && pwd)"
PATH="/usr/bin:/bin"
"#;
    assert_eq!(path_prepend_entries(lookalikes), Vec::new());

    // Two entries prepended ahead of $PATH in one assignment (the
    // test-dispatch-occupant-gate.sh shape) -- both are reported.
    let two = r#"dispatch_run PATH="$F_ROOT/bin:$FAKE_TMUX_DIR:$PATH" STORY_READY_DELAY=0"#;
    assert_eq!(
        path_prepend_entries(two),
        vec![
            Occurrence {
                line: 1,
                entry: "$F_ROOT/bin".to_string(),
            },
            Occurrence {
                line: 1,
                entry: "$FAKE_TMUX_DIR".to_string(),
            },
        ]
    );
}

// ---------------------------------------------------------------------------
// Resolution: does an entry name an existing directory?
// ---------------------------------------------------------------------------

/// Repo-relative synonyms every `plugins/story/tests/*.sh` file gets for
/// free from sourcing `lib.sh` — the two path variables it computes from
/// `${BASH_SOURCE[0]}` rather than a literal assignment this scan could
/// otherwise pick up on its own.
fn builtin_synonyms() -> HashMap<&'static str, &'static str> {
    HashMap::from([
        ("TESTS_DIR", "plugins/story/tests"),
        ("PLUGIN_ROOT", "plugins/story"),
    ])
}

/// `VAR="literal text, optionally with $OTHER references"` assignments in
/// `text` — simple aliases like `FAKE_TMUX_DIR="$TESTS_DIR/fakes"`. A value
/// containing a command substitution (`$(`) is deliberately excluded here:
/// that is not a literal, it belongs in [`dynamic_assignments`].
fn literal_assignments(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let rest = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let Some(eq) = rest.find("=\"") else {
            continue;
        };
        let name = &rest[..eq];
        if !is_valid_ident(name) {
            continue;
        }
        let after = &rest[eq + 2..];
        let Some(end) = after.find('"') else {
            continue;
        };
        let value = &after[..end];
        if !value.contains("$(") {
            map.insert(name.to_string(), value.to_string());
        }
    }
    map
}

/// Variable names `text` assigns from a command substitution — the shape every
/// runtime-minted fixture directory in this suite uses (`mktemp -d`, `$(cd …
/// && pwd)`, `mk_versioned_claude`, …). An entry whose leading variable
/// resolves here is EXPLAINED rather than merely unresolved: it names a path
/// this scan cannot know statically, on purpose, not a gap in the scan.
///
/// **Quoted and defaulted forms count, and they did not until SH-531.** This
/// used to require the exact bytes `=$(`, which recognises `VAR=$(cmd)` and
/// misses both `VAR="$(cmd)"` — the *safer* spelling, since an unquoted
/// substitution word-splits on a path with a space in it — and
/// `VAR="${OTHER:-$(cmd)}"`. Two harnesses written in those shapes were
/// reported as gaps in the scan's vocabulary, which is what this function is
/// supposed to distinguish them from. The rule is now the property rather than
/// one spelling of it: a value that contains a command substitution anywhere
/// is a value this scan cannot evaluate.
///
/// It stays disjoint from [`literal_assignments`], which excludes any value
/// containing `$(` for the same reason from the other side.
fn dynamic_assignments(text: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let rest = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let Some(eq) = rest.find('=') else {
            continue;
        };
        let name = &rest[..eq];
        if !is_valid_ident(name) {
            continue;
        }
        if rest[eq + 1..].contains("$(") {
            set.insert(name.to_string());
        }
    }
    set
}

/// The widened rule really does recognise the three spellings, and still
/// refuses a name it has no assignment for.
///
/// A positive control on the parser itself: a `dynamic_assignments` that
/// silently stopped matching would turn every runtime-minted entry into an
/// `Unexplained` offender, and one that matched everything would turn every
/// real SH-264 defect into a shrug.
#[test]
fn a_command_substitution_is_recognised_in_every_spelling_a_harness_uses() {
    let found = dynamic_assignments(
        "BARE=$(mktemp -d)\n\
         QUOTED=\"$(cd \"$(dirname \"$x\")\" && pwd)\"\n\
         DEFAULTED=\"${OTHER:-$(cd .. && pwd)/target}\"\n\
         export EXPORTED=\"$(pwd)\"\n\
         PLAIN=\"$TESTS_DIR/fakes\"\n\
         NOT_AN_ASSIGNMENT $(cmd)\n",
    );
    for name in ["BARE", "QUOTED", "DEFAULTED", "EXPORTED"] {
        assert!(
            found.contains(name),
            "{name} must be seen as runtime-minted"
        );
    }
    assert!(
        !found.contains("PLAIN"),
        "a plain alias is `literal_assignments`' business, and the two must \
         stay disjoint"
    );
    assert!(!found.contains("NOT_AN_ASSIGNMENT"));
}

/// The leading `$IDENT` or `${IDENT}` a value starts with, and whatever
/// follows it with one leading `/` stripped. `None` if `value` does not
/// start with a variable reference at all.
fn split_leading_var(value: &str) -> Option<(&str, &str)> {
    let rest = value.strip_prefix('$')?;
    let (ident, remainder) = if let Some(braced) = rest.strip_prefix('{') {
        let end = braced.find('}')?;
        (&braced[..end], &braced[end + 1..])
    } else {
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        (&rest[..end], &rest[end..])
    };
    if ident.is_empty() {
        return None;
    }
    Some((ident, remainder.strip_prefix('/').unwrap_or(remainder)))
}

/// What became of one entry after every resolution strategy was tried.
#[derive(Debug)]
enum Resolution {
    /// Resolved to a path inside the repo that exists and is a directory.
    Directory,
    /// Resolved to a path inside the repo that either does not exist, or
    /// exists and is NOT a directory — the exact shape of SH-264's bug.
    NotADirectory(String),
    /// The leading variable is a proven runtime value (`dynamic_assignments`
    /// found it), so this scan cannot and should not know its path — expected,
    /// not a finding.
    RuntimeMinted(String),
    /// The leading variable is neither a known synonym, a literal alias, nor
    /// a proven runtime assignment — the scan has no explanation for it. This
    /// is a gap in the scan's own vocabulary, not a verdict on the entry, so
    /// it is still reported as an offender: silently shrugging is exactly
    /// the failure mode this file exists to close off.
    Unexplained(String),
}

/// Resolve one PATH entry found in `file` (relative to `root`) to a verdict.
///
/// Substitutes the entry's leading variable against `builtins` then
/// `literal`, repeatedly (a literal may itself reference a builtin, as
/// `FAKE_TMUX_DIR` does), until the entry is a plain repo-relative path or no
/// further substitution applies. A plain path is checked directly; a path
/// that still starts with an unresolved variable is checked two more ways
/// before giving up on it — the whole entry from repo root, and the whole
/// entry from the scanned file's own directory (which is what lets
/// `scripts/run-e2e.sh`'s `$repo_root/plugins/story/tests/fakes` resolve
/// without this scan ever being told what `repo_root` means).
fn resolve_entry(
    entry: &str,
    literal: &HashMap<String, String>,
    dynamic: &HashSet<String>,
    root: &Path,
    file_dir: &Path,
) -> Resolution {
    let builtins = builtin_synonyms();
    let mut current = entry.to_string();
    let mut last_var: Option<(String, String)> = None;

    for _ in 0..8 {
        let Some((ident, remainder)) = split_leading_var(&current) else {
            last_var = None;
            break;
        };
        let replacement = builtins
            .get(ident)
            .map(|s| s.to_string())
            .or_else(|| literal.get(ident).cloned());
        match replacement {
            Some(value) => {
                current = if remainder.is_empty() {
                    value
                } else {
                    format!("{value}/{remainder}")
                };
            }
            None => {
                last_var = Some((ident.to_string(), remainder.to_string()));
                break;
            }
        }
    }

    if last_var.is_none() {
        // Fully resolved to a plain repo-relative (or absolute) path.
        let candidate = root.join(&current);
        return if candidate.is_dir() {
            Resolution::Directory
        } else {
            Resolution::NotADirectory(current)
        };
    }

    let (ident, remainder) = last_var.unwrap();
    if !remainder.is_empty() {
        for base in [root, file_dir] {
            if base.join(&remainder).is_dir() {
                return Resolution::Directory;
            }
        }
    }
    if dynamic.contains(&ident) {
        Resolution::RuntimeMinted(ident)
    } else {
        Resolution::Unexplained(format!("${ident} (in entry `{entry}`)"))
    }
}

// ---------------------------------------------------------------------------
// The scan itself
// ---------------------------------------------------------------------------

fn tracked_shell_scripts(root: &Path) -> Vec<(String, String)> {
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z", "--", "*.sh"])
        .output()
        .expect("listing this repository's tracked shell scripts");
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so this scan proved nothing: {}",
        String::from_utf8_lossy(&listed.stderr)
    );

    let scripts: Vec<(String, String)> = listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|path| {
            let relative = std::str::from_utf8(path).expect("a UTF-8 path").to_string();
            let text = std::fs::read_to_string(root.join(&relative))
                .unwrap_or_else(|e| panic!("reading {relative}: {e}"));
            (relative, text)
        })
        .collect();

    assert!(
        scripts.len() >= 10,
        "this scan is supposed to find every tracked shell script, and it found {} \
         -- the pattern (`git ls-files -- '*.sh'`) is broken, not the tree.",
        scripts.len()
    );
    scripts
}

/// Every `PATH="…:$PATH"` entry in a tracked shell script names an existing
/// directory (SH-264).
///
/// A scan that finds nothing at all would pass this vacuously, which is
/// indistinguishable from "the pattern broke" — so this also floors how many
/// entries it expects to resolve to a real directory. Measured on the tree
/// that motivated this file: 27 matching lines, 31 entries, none of them
/// runtime-minted or unexplained; the floor below leaves headroom for
/// entries this scan cannot yet explain being added deliberately (a new
/// `mktemp -d`-based fixture var) without also lowering the bar that matters.
#[test]
fn every_path_prepend_entry_names_a_directory() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scripts = tracked_shell_scripts(root);

    let mut resolved_directories = 0usize;
    let mut bad: Vec<String> = Vec::new();
    let mut unexplained: Vec<String> = Vec::new();
    let mut runtime_minted: Vec<String> = Vec::new();

    for (relative, text) in &scripts {
        let occurrences = path_prepend_entries(text);
        if occurrences.is_empty() {
            continue;
        }
        let literal = literal_assignments(text);
        let dynamic = dynamic_assignments(text);
        let file_dir: PathBuf = Path::new(relative)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let file_dir_abs = root.join(&file_dir);

        for occ in occurrences {
            match resolve_entry(&occ.entry, &literal, &dynamic, root, &file_dir_abs) {
                Resolution::Directory => resolved_directories += 1,
                Resolution::NotADirectory(resolved) => bad.push(format!(
                    "{relative}:{line} -- `{entry}` resolves to `{resolved}`, which is not an \
                     existing directory (a PATH entry that isn't a directory is silently \
                     skipped by the shell -- this is the exact shape of SH-264's bug)",
                    line = occ.line,
                    entry = occ.entry,
                )),
                Resolution::Unexplained(why) => unexplained.push(format!(
                    "{relative}:{line} -- `{entry}` -- {why}",
                    line = occ.line,
                    entry = occ.entry,
                )),
                Resolution::RuntimeMinted(var) => {
                    runtime_minted.push(format!("{relative}:{line} -- ${var}", line = occ.line))
                }
            }
        }
    }

    // Reported unconditionally (not just on failure) so a reviewer can see
    // exactly which entries this scan declined to check statically, and why
    // that's expected rather than a gap.
    eprintln!(
        "harness_path_entries: {} entries resolved to a real directory, {} are runtime-minted \
         and explained: {runtime_minted:?}",
        resolved_directories,
        runtime_minted.len(),
    );

    assert!(
        resolved_directories >= 20,
        "this scan is supposed to find every PATH entry that resolves to a directory, and it \
         resolved {resolved_directories} -- the pattern is broken, not the tree."
    );

    let mut offenders = bad;
    offenders.extend(unexplained);
    assert!(
        offenders.is_empty(),
        "{offenders:#?}\n\nEach of these either names something other than an existing \
         directory (SH-264's bug: a shell silently skips a non-directory PATH entry rather \
         than erroring, so the real binary answers instead of the fixture) or uses a variable \
         this scan doesn't recognize as either a literal alias or a runtime-minted value (teach \
         `literal_assignments`/`dynamic_assignments`/`builtin_synonyms` about it, or fix the \
         entry)."
    );
}
