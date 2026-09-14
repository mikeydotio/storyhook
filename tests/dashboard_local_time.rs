//! Every absolute time the dashboard shows goes through `timeNode()` (SH-679).
//!
//! The server sends every timestamp as an RFC3339 UTC string, and until SH-679
//! the dashboard printed that string raw or sliced with its `Z` chopped off,
//! so a UTC wall-clock read as if it were local. The browser knows the
//! viewer's zone; `localStamp()` converts and `timeNode()` wraps the result in
//! `<time datetime="…Z">` with the stored instant in its title. This is the
//! static mirror of `e2e/specs/local-time.spec.ts`: complete over the file
//! for the idioms named here, so the next raw `*_at` field fails at its source
//! line rather than waiting for a browser scenario to happen to render it.
//!
//! The idioms:
//! - `.replace("T", " ")` — the old "UTC to the minute, no marker" spelling;
//! - a `*_at` / `.at` field sliced to a date or minute on the same line;
//! - a `*_at` / `.at` field passed as the sole child of `el()` (`[x.at]`).
//!
//! `timeNode()` itself is the one door and is held to the same rules: its body
//! must produce a `<time>` with a `datetime` and must read the instant through
//! `localStamp()`, never by slicing the wire string.

use regex::Regex;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RawTimeIdiom {
    /// `.replace("T", " ")`
    ReplaceT,
    /// `(x.updated_at || "").slice(0, 10)` and kin, on one line.
    SlicedField,
    /// `[incident.first_failed_at]` — the raw field as an `el()` child.
    RawChild,
}

#[derive(Debug)]
struct Hit {
    idiom: RawTimeIdiom,
    offset: usize,
    line: usize,
    source: String,
}

fn dashboard() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/web_dashboard.html");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
}

/// Blanks JavaScript comments while preserving line numbers -- the same
/// string-unaware stripper `tests/dashboard_dom_removal.rs` carries, copied
/// because each `tests/*.rs` file compiles as its own crate. The positive
/// controls below prove it still sees the code it polices.
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

fn hit(source: &str, offset: usize, idiom: RawTimeIdiom) -> Hit {
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

/// A timestamp field reference: `foo.created_at`, `c.at`, `p.linked_at`.
const FIELD: &str = r"\.(?:[a-z_]*_at|at)\b";

fn scan(source: &str) -> Vec<Hit> {
    let replace_t = Regex::new(r#"\.replace\(\s*"T"\s*,\s*" "\s*\)"#).expect("valid replace regex");
    let sliced_field = Regex::new(&format!(r"{FIELD}[^\n]*?\.slice\(\s*0\s*,\s*\d+\s*\)"))
        .expect("valid slice regex");
    let raw_child =
        Regex::new(&format!(r"\[\s*[A-Za-z_$][\w$]*{FIELD}\s*\]")).expect("valid child regex");
    let mut hits = Vec::new();

    for found in replace_t.find_iter(source) {
        hits.push(hit(source, found.start(), RawTimeIdiom::ReplaceT));
    }
    for found in sliced_field.find_iter(source) {
        hits.push(hit(source, found.start(), RawTimeIdiom::SlicedField));
    }
    for found in raw_child.find_iter(source) {
        hits.push(hit(source, found.start(), RawTimeIdiom::RawChild));
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

fn describe(hits: &[Hit]) -> String {
    hits.iter()
        .map(|hit| format!("  line {}: {:?}: {}", hit.line, hit.idiom, hit.source))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn no_absolute_time_is_rendered_from_the_raw_wire_string() {
    let code = strip_comments(&dashboard());
    let hits = scan(&code);

    assert!(
        hits.is_empty(),
        "src/web_dashboard.html renders a timestamp from the raw UTC wire string instead of \
         through `timeNode()` (SH-679):\n{}",
        describe(&hits)
    );
}

#[test]
fn the_door_renders_a_time_element_with_the_stored_instant() {
    let code = strip_comments(&dashboard());
    let door = function_body(&code, "timeNode");

    assert!(
        door.contains(r#"el("time""#),
        "`timeNode()` must build a <time> element, found:\n{door}"
    );
    assert!(
        door.contains("datetime"),
        "`timeNode()` must carry the stored instant in `datetime`, found:\n{door}"
    );
    assert!(
        door.contains("title"),
        "`timeNode()` must expose the stored instant and zone in `title`, found:\n{door}"
    );
    assert!(
        door.contains("localStamp("),
        "`timeNode()` must read the instant through `localStamp()`, found:\n{door}"
    );

    let stamp = function_body(&code, "localStamp");
    assert!(
        stamp.contains("Date.parse("),
        "`localStamp()` must parse the instant rather than slice the wire string, found:\n{stamp}"
    );
    for local_getter in [
        "getFullYear()",
        "getMonth()",
        "getDate()",
        "getHours()",
        "getMinutes()",
    ] {
        assert!(
            stamp.contains(local_getter),
            "`localStamp()` must read the browser-local `{local_getter}`, found:\n{stamp}"
        );
    }
    assert!(
        !stamp.contains("getUTC"),
        "`localStamp()` must not read UTC fields, found:\n{stamp}"
    );
}

/// The scanner sees each idiom it exists to catch. Assembled at run time so
/// no literal in this file can trip the scan over the real dashboard.
#[test]
fn the_scan_recognises_every_idiom_it_polices() {
    let sliced = format!("var updated = (st.updated{} || \"\").slice(0, 10);", "_at");
    let replaced = format!("[(c.at || \"\").replace(\"T\", \" \").slice(0, 16)]{}", "");
    let raw = format!("el(\"code\", {{}}, [incident.first_failed{}])", "_at");
    let clean = "el(\"code\", {}, [timeNode(incident.first_failed_at, \"second\")])";
    let sample = format!("{sliced}\n{replaced}\n{raw}\n{clean}\n");

    let idioms: Vec<(usize, RawTimeIdiom)> = scan(&sample)
        .iter()
        .map(|hit| (hit.line, hit.idiom))
        .collect();
    assert_eq!(
        idioms,
        vec![
            (1, RawTimeIdiom::SlicedField),
            (2, RawTimeIdiom::SlicedField),
            (2, RawTimeIdiom::ReplaceT),
            (3, RawTimeIdiom::RawChild),
        ]
    );
}

/// The comment stripper does not hide code: an idiom outside a comment is
/// still found once comments around it are blanked.
#[test]
fn the_comment_stripper_keeps_code_and_line_numbers() {
    let sample = format!(
        "/* (x.at || \"\").slice(0, 10) */\n// [y.closed{}]\nvar z = (w.at || \"\").slice(0, 16);\n",
        "_at"
    );
    let hits = scan(&strip_comments(&sample));
    assert_eq!(hits.len(), 1, "{}", describe(&hits));
    assert_eq!(hits[0].line, 3);
    assert_eq!(hits[0].idiom, RawTimeIdiom::SlicedField);
}
