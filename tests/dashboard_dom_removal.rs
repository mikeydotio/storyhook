//! Every dashboard DOM removal goes through `clear()` or `detach()` (SH-421).
//!
//! The press gate's `MutationObserver` catches every removal mechanism on the
//! surfaces a browser test exercises (SH-401). This is its static mirror:
//! complete over this file, but only for the removal idioms named here. A new
//! direct `.remove()`, `removeChild(...)`, `replaceChildren(...)`, or
//! `innerHTML = ...` therefore fails at its source line instead of depending
//! on a browser scenario to happen to exercise it.
//!
//! The two primitive calls that remain are the doors themselves: `clear()`
//! removes every child and `detach()` removes one node. Everything else calls
//! one of those helpers.

use regex::Regex;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemovalIdiom {
    Remove,
    RemoveChild,
    ReplaceChildren,
    InnerHtmlAssignment,
}

#[derive(Debug)]
struct Hit {
    idiom: RemovalIdiom,
    offset: usize,
    line: usize,
    source: String,
}

fn dashboard() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/web_dashboard.html");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
}

/// Blanks JavaScript comments while preserving line numbers.
///
/// This deliberately matches the established dashboard scan in
/// `dashboard_enter_submit_guard.rs`: it is string-unaware, so the real-file
/// door assertions below are the positive control proving that it still sees
/// the code it is meant to police.
fn strip_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let chars: Vec<char> = source.chars().collect();
    let mut index = 0;
    let mut in_block = false;
    let mut in_line = false;

    while index < chars.len() {
        let current = chars[index];
        let next = chars.get(index + 1).copied().unwrap_or('\0');
        if current == '\n' {
            in_line = false;
            out.push('\n');
            index += 1;
            continue;
        }
        if in_block {
            if current == '*' && next == '/' {
                in_block = false;
                out.push(' ');
                out.push(' ');
                index += 2;
            } else {
                out.push(' ');
                index += 1;
            }
            continue;
        }
        if in_line {
            out.push(' ');
            index += 1;
            continue;
        }
        if current == '/' && next == '*' {
            in_block = true;
            out.push(' ');
            out.push(' ');
            index += 2;
            continue;
        }
        if current == '/' && next == '/' {
            in_line = true;
            out.push(' ');
            out.push(' ');
            index += 2;
            continue;
        }
        out.push(current);
        index += 1;
    }

    out
}

fn hit(source: &str, offset: usize, idiom: RemovalIdiom) -> Hit {
    let line = source[..offset]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1;
    Hit {
        idiom,
        offset,
        line,
        source: source
            .lines()
            .nth(line - 1)
            .unwrap_or_default()
            .trim()
            .to_string(),
    }
}

fn scan(source: &str) -> Vec<Hit> {
    let remove = Regex::new(r"\.\s*remove\s*\(\s*\)").expect("valid remove regex");
    let remove_child = Regex::new(r"\bremoveChild\s*\(").expect("valid removeChild regex");
    let replace_children =
        Regex::new(r"\breplaceChildren\s*\(").expect("valid replaceChildren regex");
    let inner_html = Regex::new(r"\binnerHTML\s*=").expect("valid innerHTML regex");
    let mut hits = Vec::new();

    for found in remove.find_iter(source) {
        hits.push(hit(source, found.start(), RemovalIdiom::Remove));
    }
    for found in remove_child.find_iter(source) {
        hits.push(hit(source, found.start(), RemovalIdiom::RemoveChild));
    }
    for found in replace_children.find_iter(source) {
        hits.push(hit(source, found.start(), RemovalIdiom::ReplaceChildren));
    }
    for found in inner_html.find_iter(source) {
        if !source[found.end()..].starts_with('=') {
            hits.push(hit(
                source,
                found.start(),
                RemovalIdiom::InnerHtmlAssignment,
            ));
        }
    }

    hits.sort_by_key(|found| found.offset);
    hits
}

fn function_body<'a>(source: &'a str, name: &str) -> &'a str {
    let marker = format!("function {name}(");
    let start = source
        .find(&marker)
        .unwrap_or_else(|| panic!("dashboard must define `{name}()`"));
    let open = start
        + source[start..]
            .find('{')
            .unwrap_or_else(|| panic!("`{name}()` must have a body"));
    let mut depth = 0usize;

    for (offset, character) in source[open..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[open + 1..open + offset];
                }
            }
            _ => {}
        }
    }

    panic!("`{name}()` has an unterminated body")
}

fn idioms(hits: &[Hit]) -> Vec<RemovalIdiom> {
    hits.iter().map(|hit| hit.idiom).collect()
}

fn describe(hits: &[Hit]) -> String {
    hits.iter()
        .map(|hit| format!("  line {}: {:?}: {}", hit.line, hit.idiom, hit.source))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn every_dashboard_removal_uses_clear_or_detach() {
    let code = strip_comments(&dashboard());
    let all_hits = scan(&code);

    assert_eq!(
        all_hits.len(),
        2,
        "src/web_dashboard.html contains low-level DOM removal outside `clear()` and \
         `detach()`:\n{}",
        describe(&all_hits)
    );

    let clear_hits = scan(function_body(&code, "clear"));
    assert_eq!(
        idioms(&clear_hits),
        [RemovalIdiom::RemoveChild],
        "`clear()` must remain the one bulk-removal door"
    );

    let detach_hits = scan(function_body(&code, "detach"));
    assert_eq!(
        idioms(&detach_hits),
        [RemovalIdiom::Remove],
        "`detach()` must remain the one single-node removal door"
    );
}

#[test]
fn the_scan_rejects_each_low_level_removal_idiom() {
    let before = [
        "candidate .\n remove ( );",
        "parent . removeChild\n (candidate);",
        "parent . replaceChildren\n (candidate);",
        "candidate . innerHTML\n = rendered;",
    ]
    .join("\n");

    assert_eq!(
        idioms(&scan(&before)),
        [
            RemovalIdiom::Remove,
            RemovalIdiom::RemoveChild,
            RemovalIdiom::ReplaceChildren,
            RemovalIdiom::InnerHtmlAssignment,
        ]
    );
}

#[test]
fn the_scan_accepts_door_calls_and_ignores_prose() {
    let after = "clear(parent);\ndetach(candidate);\n\
                 // candidate.remove();\n\
                 /* parent.removeChild(candidate); */\n\
                 candidate.innerHTML === rendered;";

    assert!(
        scan(&strip_comments(after)).is_empty(),
        "door calls, comments, and an innerHTML comparison are not removal bypasses"
    );
}
