//! Every search for "a CLOSED state" in `src/` lives inside the one resolver
//! that is allowed to make it (SH-652).
//!
//! The completion state is **named**, never searched for: it is
//! `domain::COMPLETION_STATE_SLUG`, resolved by `domain::completion_state`.
//! SH-505 wrote the same rule for the abandonment state and warned that a
//! `.find(|s| s.super_state == Closed)` "answers whichever literal happens to
//! come first". It then failed exactly that way, silently: `service::pr_check`
//! ran that search over `state_map` — a `BTreeMap` — and closed every merged
//! story into `closed`, the abandonment state, on every default catalog,
//! while three comments asserted catalog order protected it. Five sites
//! carried the shape when SH-652 was filed (`pr_check.rs`, `project.rs`,
//! both verifier writers, and the TUI's `>`-key walk twice); this scan is
//! what stops the next `state_map`-versus-`states` refactor from re-landing
//! merged work in the abandonment state.
//!
//! Derived over `git ls-files`, comment-stripped, in the SH-198/SH-360 style
//! — never a hand-kept list of the sites that do this, which is the shape
//! that let the five accumulate. The exemption is named by **enclosing
//! function**, not by file (SH-345: a file-level fence is permanently
//! satisfied by the file that already contains the resolver): only
//! `domain::completion_state` itself and `domain::resting_state_for_closure`'s
//! third rung, where the fold must stay total over a legacy catalog with no
//! `closed` and no `done`, may search on `SuperState::Closed`.
//!
//! **Limit, stated rather than glossed:** the fence is lexical. It sees a
//! `.find(` closure that names `SuperState::Closed`; a search spelled as
//! `.filter(..).next()`, a `match`, or a helper predicate walks past it. That
//! is why the resolver's consumers also carry behavioural straddle tests — a
//! catalog where the positionally first and alphabetically first CLOSED
//! states both differ from `done` — in `tests/verification_queue.rs`,
//! `tests/service_pr_check.rs`, `tests/service_system.rs` and the TUI
//! component tests, so a resolver that is wrong for any reason fails there.

use std::collections::BTreeMap;
use std::path::Path;

/// The searches this repository permits: `(file, enclosing fn)`.
const ALLOWED: [(&str, &str); 2] = [
    ("src/domain.rs", "completion_state"),
    ("src/domain.rs", "resting_state_for_closure"),
];

/// One `.find(` whose closure names `SuperState::Closed`.
#[derive(Debug, PartialEq, Eq)]
struct Hit {
    file: String,
    line: usize,
    enclosing_fn: String,
}

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Every tracked `.rs` file under `src/`, keyed by repository-relative path.
fn tracked_src_sources(root: &Path) -> BTreeMap<String, String> {
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z", "--", "src/*.rs"])
        .output()
        .expect("listing this repository's tracked Rust sources");
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

/// `text` with every `//` line comment blanked (the newline kept, so line
/// numbers survive) and every `/* */` block comment blanked the same way.
///
/// A `//` inside a string literal is left alone — `"https://…"` is common in
/// this tree — by tracking double quotes per line. Comments are stripped so
/// that prose *describing* the search (this crate documents its own
/// hazards at length) cannot register as a search.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_block = false;
    for line in text.split_inclusive('\n') {
        let mut in_string = false;
        let mut escaped = false;
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if in_block {
                if c == '*' && chars.get(i + 1) == Some(&'/') {
                    in_block = false;
                    i += 2;
                    continue;
                }
                if c == '\n' {
                    out.push('\n');
                }
                i += 1;
                continue;
            }
            if in_string {
                out.push(c);
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_string = false;
                }
                i += 1;
                continue;
            }
            if c == '"' {
                in_string = true;
                out.push(c);
            } else if c == '/' && chars.get(i + 1) == Some(&'/') {
                // Blank the rest of the line, keeping its newline.
                if line.ends_with('\n') {
                    out.push('\n');
                }
                break;
            } else if c == '/' && chars.get(i + 1) == Some(&'*') {
                in_block = true;
                i += 2;
                continue;
            } else {
                out.push(c);
            }
            i += 1;
        }
    }
    out
}

/// The name of the `fn` whose header most recently precedes byte `offset`.
fn enclosing_fn(text: &str, offset: usize) -> String {
    text[..offset]
        .lines()
        .rev()
        .find_map(|line| {
            let rest = line.trim_start();
            let rest = rest.split("fn ").nth(1)?;
            let header_is_fn = line.trim_start().starts_with("fn ")
                || line.trim_start().starts_with("pub ")
                || line.trim_start().starts_with("async fn ")
                || line.trim_start().starts_with("const fn ");
            if !header_is_fn {
                return None;
            }
            let end = rest
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            (end > 0).then(|| rest[..end].to_string())
        })
        .unwrap_or_else(|| "<no enclosing fn>".to_string())
}

/// Every `.find(` in `text` whose argument (to the matching close paren)
/// names `SuperState::Closed`.
fn hits_in(file: &str, text: &str) -> Vec<Hit> {
    let stripped = strip_comments(text);
    let mut hits = Vec::new();
    let mut from = 0;
    while let Some(rel) = stripped[from..].find(".find(") {
        let start = from + rel + ".find(".len();
        let mut depth = 1usize;
        let mut end = start;
        for (i, c) in stripped[start..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = start + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let argument = &stripped[start..end.max(start)];
        if argument.contains("SuperState::Closed") {
            hits.push(Hit {
                file: file.to_string(),
                line: stripped[..start].matches('\n').count() + 1,
                enclosing_fn: enclosing_fn(&stripped, start),
            });
        }
        from = start;
    }
    hits
}

#[test]
fn every_closed_state_search_in_src_lives_in_the_resolver() {
    let sources = tracked_src_sources(repo_root());
    assert!(
        sources.contains_key("src/domain.rs"),
        "the scan did not see src/domain.rs, so it proved nothing"
    );
    let hits: Vec<Hit> = sources
        .iter()
        .flat_map(|(file, text)| hits_in(file, text))
        .collect();

    // Positive controls: the two permitted sites must be *found*, or a
    // scanner that stopped seeing anything would report a clean tree.
    for (file, function) in ALLOWED {
        assert!(
            hits.iter()
                .any(|hit| hit.file == file && hit.enclosing_fn == function),
            "the permitted search in {file}::{function} was not found; hits: {hits:#?}"
        );
    }

    let offenders: Vec<&Hit> = hits
        .iter()
        .filter(|hit| {
            !ALLOWED
                .iter()
                .any(|(file, function)| hit.file == *file && hit.enclosing_fn == *function)
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "a CLOSED-state search outside domain::completion_state (SH-652). The completion \
         state is named, never searched for: call `domain::completion_state` (or \
         `CLOSED_STATE_SLUG` for abandonment) instead of finding the first CLOSED state — \
         a positional search reads a BTreeMap as readily as a Vec and answered `closed` \
         for every merged story before this fence existed.\n{offenders:#?}"
    );
}

/// The scanner sees the shapes this fence exists for, and only those.
#[test]
fn the_scanner_recognises_the_search_shapes_and_ignores_prose() {
    let snippet = r#"
fn positional(states: &[StateDef]) -> Option<&StateDef> {
    states.iter().find(|state| state.super_state == SuperState::Closed)
}
fn alphabetical(states: &BTreeMap<String, StateDef>) -> Option<&StateDef> {
    states
        .values()
        .find(|state| {
            state.super_state == SuperState::Closed
        })
}
fn open_side(states: &[StateDef]) -> Option<&StateDef> {
    states.iter().find(|state| state.super_state == SuperState::Open)
}
// a comment that says .find(|s| s.super_state == SuperState::Closed)
fn quoted() -> &'static str {
    "a string with .find(SuperState::Closed) in it is a search too"
}
"#;
    let hits = hits_in("snippet.rs", snippet);
    let found: Vec<(&str, usize)> = hits
        .iter()
        .map(|hit| (hit.enclosing_fn.as_str(), hit.line))
        .collect();
    assert_eq!(
        found,
        vec![("positional", 3), ("alphabetical", 8), ("quoted", 17)],
        "{hits:#?}"
    );
}

#[test]
fn a_block_comment_and_a_url_do_not_confuse_the_stripper() {
    let text = "let url = \"https://example.com\"; /* .find(SuperState::Closed) */ let x = 1;\n";
    assert_eq!(
        strip_comments(text),
        "let url = \"https://example.com\";  let x = 1;\n"
    );
}
