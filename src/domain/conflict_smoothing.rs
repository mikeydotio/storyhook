//! Automated smoothing of non-code conflicts in a verification batch
//! (SH-834; spec position B8 in `docs/spec/verification-batching.md`,
//! council decision D1 on SH-834).
//!
//! No model writes a resolution. A path is smoothed only when a project
//! names it in `[batch] smooth` (read from the batch's base, never from a
//! member), the built-in deny floor does not hold it, every side of it is
//! small UTF-8 text, and every diff3 hunk Git wrote for it is
//! insertion-only: both sides added lines at the same point and neither
//! changed a base line. The resolution then keeps the side merged onto
//! first and the side merged in second, which is the conflicted file with
//! its marker lines removed, so every line it holds is a member's own.
//!
//! This module is the pure half: the allowlist, the deny floor, the text
//! guards and the hunk parser. Reading blobs and writing trees is
//! `service::batch_smoothing`'s.

use std::fmt;

/// The versioned strategy every resolution records (council D1 (c)).
pub const STRATEGY: &str = "union-insertions/1";

/// The largest blob any side of a smoothed path may be.
pub const MAX_BLOB_BYTES: usize = 1 << 20;

/// The most conflicted paths one smoothable merge may have.
pub const MAX_SMOOTHED_PATHS: usize = 20;

/// File names the deny floor holds at any depth, compared case-folded:
/// agent instructions and files that run on checkout or configure the
/// verifier. No `[batch] smooth` entry can admit them (council D1 (a)).
const DENIED_NAMES: &[&str] = &[
    "claude.md",
    "agents.md",
    "gemini.md",
    "skill.md",
    ".storyhook.toml",
    ".gitattributes",
    ".gitmodules",
    ".envrc",
];

/// Directory names the deny floor holds at any depth, compared case-folded:
/// agent configuration and what CI or a checkout runs.
const DENIED_DIRS: &[&str] = &[
    ".claude",
    ".codex",
    ".cursor",
    ".github",
    ".githooks",
    ".husky",
    ".cargo",
];

/// The paths a project lets the verifier smooth: `[batch] smooth` in its
/// committed `.storyhook.toml`. Empty, the default, smooths nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SmoothPolicy {
    rules: Vec<PathRule>,
}

/// One `[batch] smooth` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathRule {
    /// Exactly this path, such as `.gitignore`.
    Exact(String),
    /// Every path under this directory; written with its trailing `/`, such
    /// as `docs/spec/`.
    Under(String),
}

impl SmoothPolicy {
    /// Parses the entries of `[batch] smooth`. Each is a repository-relative
    /// path, or a directory ending in `/`, in printable ASCII, with no empty,
    /// `.` or `..` component and no leading `/`. The `Err` names the entry
    /// and why, for the person who wrote it.
    pub fn parse(entries: &[String]) -> Result<Self, String> {
        let rules = entries
            .iter()
            .map(|entry| {
                parse_rule(entry).map_err(|why| {
                    format!(
                        "[batch] smooth entry {entry:?} {why}; write a repository-relative \
                         path such as \".gitignore\", or a directory ending in \"/\" such as \
                         \"docs/spec/\""
                    )
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { rules })
    }

    /// Whether no path is admitted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Whether an entry admits `path`. Case-sensitive, as Git's paths are;
    /// the deny floor, which must never miss, is the case-folded check.
    #[must_use]
    pub fn admits(&self, path: &str) -> bool {
        self.rules.iter().any(|rule| match rule {
            PathRule::Exact(exact) => path == exact,
            PathRule::Under(directory) => {
                path.len() > directory.len() && path.starts_with(directory.as_str())
            }
        })
    }

    /// The entries as written, for a record or a message.
    #[must_use]
    pub fn entries(&self) -> Vec<&str> {
        self.rules
            .iter()
            .map(|rule| match rule {
                PathRule::Exact(path) | PathRule::Under(path) => path.as_str(),
            })
            .collect()
    }
}

/// Why a conflicted path can never be smoothed, whatever the allowlist
/// says, or `None` when its name passes: it is printable ASCII with no
/// backtick (the name goes into a commit trailer and a pull request body,
/// and ASCII leaves no normalization trick past the floor, SH-834 D2), and
/// the deny floor does not hold it.
#[must_use]
pub fn path_refusal(path: &str) -> Option<String> {
    if path.is_empty() {
        return Some("the path is empty".into());
    }
    if !path.bytes().all(|byte| (0x20..=0x7e).contains(&byte)) {
        return Some(format!(
            "{path:?} is not printable ASCII, so its name cannot be checked against the deny floor"
        ));
    }
    if path.contains('`') {
        return Some(format!("{path:?} holds a backtick"));
    }
    let folded = path.to_ascii_lowercase();
    let name = folded.rsplit('/').next().unwrap_or_default();
    if let Some(denied) = DENIED_NAMES.iter().find(|denied| **denied == name) {
        return Some(format!("{path} is on the deny floor ({denied})"));
    }
    if let Some(denied) = folded
        .split('/')
        .find_map(|component| DENIED_DIRS.iter().find(|denied| **denied == component))
    {
        return Some(format!("{path} is on the deny floor ({denied}/)"));
    }
    None
}

/// Checks one side of a conflicted path (`what` names it in the `Err`):
/// at most [`MAX_BLOB_BYTES`], no NUL, UTF-8, and no line the hunk parser
/// would read as a conflict marker. Answers the text.
pub fn check_text<'a>(what: &str, bytes: &'a [u8]) -> Result<&'a str, String> {
    if bytes.len() > MAX_BLOB_BYTES {
        return Err(format!(
            "{what} is {} bytes, over the {MAX_BLOB_BYTES}-byte limit",
            bytes.len()
        ));
    }
    if bytes.contains(&0) {
        return Err(format!("{what} holds a NUL byte, so it is not text"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| format!("{what} is not UTF-8 text"))?;
    if let Some(line) = text.lines().find(|line| marker(line).is_some()) {
        return Err(format!(
            "{what} already holds a line that reads as a conflict marker ({line:?}), so its \
             hunks could not be told apart"
        ));
    }
    Ok(text)
}

/// The four diff3 conflict markers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Marker {
    /// `<<<<<<<`: a hunk and its first side begin.
    Begin,
    /// `|||||||`: the base section begins.
    Base,
    /// `=======`: the second side begins.
    Separator,
    /// `>>>>>>>`: the hunk ends.
    End,
}

/// The marker `line` is, if any: exactly seven of one marker character,
/// then the end of the line or a space and a label. A carriage return that
/// ends the line is ignored, as Git writes markers in a CRLF file's own
/// endings.
fn marker(line: &str) -> Option<Marker> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let kind = match line.as_bytes().first()? {
        b'<' => Marker::Begin,
        b'|' => Marker::Base,
        b'=' => Marker::Separator,
        b'>' => Marker::End,
        _ => return None,
    };
    let first = line.as_bytes()[0];
    let run = line.bytes().take_while(|byte| *byte == first).count();
    let rest = &line[run..];
    (run == 7 && (rest.is_empty() || rest.starts_with(' '))).then_some(kind)
}

fn parse_rule(entry: &str) -> Result<PathRule, String> {
    if entry.is_empty() {
        return Err("is empty".into());
    }
    if !entry.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
        return Err("is not printable ASCII without spaces".into());
    }
    if let Some(special) = entry
        .chars()
        .find(|c| matches!(c, '\\' | '*' | '?' | '[' | ']'))
    {
        return Err(format!(
            "holds {special:?}; entries are exact paths or directories, never globs"
        ));
    }
    if entry.starts_with('/') {
        return Err("starts with \"/\"".into());
    }
    let (body, directory) = match entry.strip_suffix('/') {
        Some(body) => (body, true),
        None => (entry, false),
    };
    if body
        .split('/')
        .any(|component| matches!(component, "" | "." | ".."))
    {
        return Err("has an empty, \".\" or \"..\" component".into());
    }
    Ok(if directory {
        PathRule::Under(entry.to_owned())
    } else {
        PathRule::Exact(entry.to_owned())
    })
}

/// Why a conflicted file's hunks cannot be united.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HunkRefusal {
    /// A hunk's base section is not empty: a side changed or removed a base
    /// line, so a union would keep two versions of it. A model might
    /// resolve it; no rule here does (council D1: `agent-candidate`).
    Modifies,
    /// The markers do not form diff3 hunks; the reason says where.
    Malformed(String),
}

impl fmt::Display for HunkRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Modifies => f.write_str("a hunk changes lines both sides share"),
            Self::Malformed(why) => write!(f, "its conflict markers are not diff3 hunks: {why}"),
        }
    }
}

/// Unites the insertion-only diff3 hunks of a conflicted file: the file
/// with every marker line removed, so each hunk keeps the side merged onto
/// and then the side merged in. Refuses unless the file holds at least one
/// hunk and every hunk is `<<<<<<<`, the first side, `|||||||` with an empty
/// base section, `=======`, the second side, `>>>>>>>`.
pub fn union_insertions(conflicted: &str) -> Result<String, HunkRefusal> {
    /// Where the parser is: outside a hunk, or in one of its sections.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Section {
        Outside,
        First,
        Base,
        Second,
    }
    let malformed =
        |number: usize, why: &str| Err(HunkRefusal::Malformed(format!("line {number}: {why}")));
    let mut united = String::with_capacity(conflicted.len());
    let mut section = Section::Outside;
    let mut hunks = 0_usize;
    let mut modifies = false;
    for (index, line) in conflicted.split_inclusive('\n').enumerate() {
        let number = index + 1;
        let content = line.strip_suffix('\n').unwrap_or(line);
        section = match (section, marker(content)) {
            (Section::Outside, Some(Marker::Begin)) => Section::First,
            (Section::First, Some(Marker::Base)) => Section::Base,
            (Section::First, Some(Marker::Separator)) => {
                return malformed(number, "a hunk has no ||||||| base section");
            }
            (Section::Base, Some(Marker::Separator)) => Section::Second,
            (Section::Second, Some(Marker::End)) => {
                hunks += 1;
                Section::Outside
            }
            (_, Some(found)) => {
                return malformed(number, &format!("{found:?} marker out of order"));
            }
            (Section::Base, None) => {
                modifies = true;
                Section::Base
            }
            (current, None) => {
                united.push_str(line);
                current
            }
        };
    }
    if section != Section::Outside {
        return malformed(
            conflicted.split_inclusive('\n').count(),
            "the last hunk never ends",
        );
    }
    if hunks == 0 {
        return Err(HunkRefusal::Malformed(
            "the file holds no conflict hunk".into(),
        ));
    }
    if modifies {
        return Err(HunkRefusal::Modifies);
    }
    Ok(united)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(entries: &[&str]) -> SmoothPolicy {
        SmoothPolicy::parse(&entries.iter().map(|e| (*e).to_owned()).collect::<Vec<_>>())
            .expect("a valid policy")
    }

    #[test]
    fn a_policy_admits_exact_paths_and_paths_under_a_directory() {
        let policy = policy(&["docs/spec/", ".gitignore", "notes.txt"]);
        assert!(policy.admits("docs/spec/a.md"));
        assert!(policy.admits("docs/spec/deep/b.md"));
        assert!(policy.admits(".gitignore"));
        assert!(policy.admits("notes.txt"));
        assert!(
            !policy.admits("docs/spec"),
            "the directory itself is no file"
        );
        assert!(
            !policy.admits("docs/specs/a.md"),
            "a prefix is a directory, not a string"
        );
        assert!(!policy.admits("docs/a.md"));
        assert!(
            !policy.admits("sub/.gitignore"),
            "an exact entry is not a basename"
        );
        assert!(!policy.admits("notes.txt.bak"));
        assert!(
            !policy.admits("Docs/spec/a.md"),
            "entries are case-sensitive"
        );
        assert!(!policy.admits(""));
        assert_eq!(policy.entries(), ["docs/spec/", ".gitignore", "notes.txt"]);
    }

    #[test]
    fn an_empty_policy_admits_nothing() {
        let empty = SmoothPolicy::default();
        assert!(empty.is_empty());
        assert!(!empty.admits("docs/a.md"));
        assert_eq!(SmoothPolicy::parse(&[]).unwrap(), empty);
    }

    #[test]
    fn a_policy_entry_that_is_not_a_plain_relative_path_is_refused_by_name() {
        for bad in [
            "",
            "/",
            "/docs/",
            "docs//spec/",
            "./docs/",
            "docs/./spec/",
            "../docs/",
            "docs/../src/",
            "docs\\spec\\",
            "docs/spé/",
            "docs/\tspec/",
            "*.md",
            "docs/**",
            "docs/[ab].md",
            "docs/a?.md",
        ] {
            let error =
                SmoothPolicy::parse(&[bad.to_owned()]).expect_err(&format!("{bad:?} was accepted"));
            assert!(error.contains(&format!("{bad:?}")), "{error}");
        }
        let error = SmoothPolicy::parse(&["docs/".into(), "*".into()]).unwrap_err();
        assert!(error.contains("\"*\""), "{error}");
    }

    #[test]
    fn the_deny_floor_holds_agent_instructions_and_executed_files_at_any_depth_in_any_case() {
        for denied in [
            "CLAUDE.md",
            "docs/claude.md",
            "AGENTS.md",
            "a/b/Agents.MD",
            "GEMINI.md",
            "plugins/story/skills/story/SKILL.md",
            "skill.md",
            ".storyhook.toml",
            "sub/.storyhook.toml",
            ".gitattributes",
            "docs/.gitattributes",
            ".gitmodules",
            ".envrc",
            ".claude/settings.json",
            "docs/.claude/notes.md",
            ".codex/config.toml",
            ".cursor/rules.md",
            ".github/workflows/ci.yml",
            ".GitHub/CODEOWNERS",
            ".githooks/pre-push",
            ".husky/pre-commit",
            ".cargo/config.toml",
        ] {
            let refusal = path_refusal(denied).unwrap_or_else(|| panic!("{denied} passed"));
            assert!(refusal.contains("deny floor"), "{denied}: {refusal}");
        }
        for allowed in [
            "docs/spec/a.md",
            ".gitignore",
            "README.md",
            "docs/claude.md.bak",
            "docs/github/notes.md",
            "docs/my.claude/x.md",
        ] {
            assert_eq!(path_refusal(allowed), None, "{allowed}");
        }
    }

    #[test]
    fn a_path_that_is_not_printable_ascii_or_holds_a_backtick_is_refused() {
        for bad in [
            "",
            "docs/sp\u{e9}c.md",
            "docs/\u{2024}.md",
            "docs/a\tb.md",
            "docs/a\nb.md",
            "docs/a\u{7f}.md",
            "docs/`x`.md",
        ] {
            assert!(path_refusal(bad).is_some(), "{bad:?} passed");
        }
    }

    #[test]
    fn text_is_checked_for_size_nul_utf8_and_marker_lines() {
        assert_eq!(check_text("ours", b"plain\ntext\n"), Ok("plain\ntext\n"));
        assert_eq!(check_text("ours", b""), Ok(""));
        assert_eq!(
            check_text("ours", b"setext\n========\nlonger underline is fine\n"),
            Ok("setext\n========\nlonger underline is fine\n")
        );
        let too_big = vec![b'a'; MAX_BLOB_BYTES + 1];
        assert!(check_text("ours", &too_big).unwrap_err().contains("ours"));
        assert!(check_text("ours", &vec![b'a'; MAX_BLOB_BYTES]).is_ok());
        assert!(check_text("base", b"a\0b").unwrap_err().contains("NUL"));
        assert!(
            check_text("theirs", b"\xff\xfe")
                .unwrap_err()
                .contains("UTF-8")
        );
        for marker in [
            "<<<<<<<\n",
            "<<<<<<< HEAD\n",
            "|||||||\n",
            "||||||| base\n",
            "=======\n",
            "=======\r\n",
            ">>>>>>>\n",
            ">>>>>>> theirs",
        ] {
            let text = format!("before\n{marker}after\n");
            let error = check_text("theirs", text.as_bytes()).unwrap_err();
            assert!(error.contains("marker"), "{marker:?}: {error}");
        }
        for not_marker in [
            "<<<<<<<<\n",
            "<<<<<<\n",
            "<<<<<<<x\n",
            " =======\n",
            "==== ===\n",
        ] {
            let text = format!("before\n{not_marker}after\n");
            assert!(
                check_text("ours", text.as_bytes()).is_ok(),
                "{not_marker:?}"
            );
        }
    }

    #[test]
    fn insertion_only_hunks_keep_the_first_side_then_the_second() {
        let conflicted = "# Spec\n\n\
            <<<<<<< 1111\n## X\n\nx body\n\n||||||| 0000\n=======\n## Y\n\ny body\n\n>>>>>>> 2222\n\
            ## End\n";
        assert_eq!(
            union_insertions(conflicted).unwrap(),
            "# Spec\n\n## X\n\nx body\n\n## Y\n\ny body\n\n## End\n"
        );
    }

    #[test]
    fn several_hunks_each_unite_and_text_between_them_is_kept() {
        let conflicted = "a\n<<<<<<< o\nx1\n|||||||\n=======\ny1\n>>>>>>> t\nb\n\
            <<<<<<< o\nx2\n||||||| b\n=======\n>>>>>>> t\nc\n";
        assert_eq!(
            union_insertions(conflicted).unwrap(),
            "a\nx1\ny1\nb\nx2\nc\n"
        );
    }

    #[test]
    fn crlf_markers_and_lines_keep_their_endings() {
        let conflicted = "a\r\n<<<<<<< o\r\nx\r\n||||||| b\r\n=======\r\ny\r\n>>>>>>> t\r\nz";
        assert_eq!(union_insertions(conflicted).unwrap(), "a\r\nx\r\ny\r\nz");
    }

    #[test]
    fn a_hunk_with_base_lines_is_a_modification_not_a_union() {
        let conflicted = "<<<<<<< o\nmine\n||||||| b\nshared\n=======\nyours\n>>>>>>> t\n";
        assert_eq!(union_insertions(conflicted), Err(HunkRefusal::Modifies));
        let one_of_two = "<<<<<<< o\nx\n|||||||\n=======\ny\n>>>>>>> t\n\
            <<<<<<< o\nm\n||||||| b\nold\n=======\n>>>>>>> t\n";
        assert_eq!(union_insertions(one_of_two), Err(HunkRefusal::Modifies));
    }

    #[test]
    fn markers_that_do_not_form_diff3_hunks_are_malformed() {
        for bad in [
            "no markers at all\n",
            "",
            "<<<<<<< o\nx\n=======\ny\n>>>>>>> t\n",
            "<<<<<<< o\nx\n|||||||\n=======\ny\n",
            "<<<<<<< o\nx\n|||||||\n",
            "<<<<<<< o\n<<<<<<< o\nx\n|||||||\n=======\n>>>>>>> t\n",
            "x\n=======\ny\n",
            "x\n>>>>>>> t\n",
            "x\n||||||| b\n",
            "<<<<<<< o\nx\n|||||||\n=======\ny\n=======\n>>>>>>> t\n",
            "<<<<<<< o\nx\n|||||||\n|||||||\n=======\n>>>>>>> t\n",
            "<<<<<<< o\nx\n>>>>>>> t\n",
        ] {
            assert!(
                matches!(union_insertions(bad), Err(HunkRefusal::Malformed(_))),
                "{bad:?}: {:?}",
                union_insertions(bad)
            );
        }
    }

    #[test]
    fn the_strategy_is_versioned() {
        assert_eq!(STRATEGY, "union-insertions/1");
    }
}
