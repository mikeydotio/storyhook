//! Pins the dashboard's reserved-label tint to the domain's own reserved
//! names, and to a token that resolves in every theme (SH-454).
//!
//! SH-453 reserved two label names in [`storyhook::domain::RESERVED_LABELS`]
//! and gave one of them behaviour on the server. SH-454 gives both of them a
//! rendering: an orange chip everywhere the dashboard draws a label, because
//! decision D12 of the Full Auto epic (`docs/spec/full-auto-engine.md`) says
//! they mean "a person is required here" and must read that way at a glance.
//!
//! Three things can silently undo that, and none of them is visible to the
//! compiler, because the dashboard is one large HTML file:
//!
//! 1. **The name lists drift.** The browser cannot call
//!    `domain::is_human_only`, so it carries its own copy of the two names.
//!    A rename on the Rust side leaves the browser tinting a label nothing
//!    reserves any more — the exact failure class this repository has paid
//!    for repeatedly (SH-136, SH-198, SH-258, SH-260/276, SH-360), and the
//!    one `tests/dashboard_mutation_deadline.rs` (SH-312) already fences for
//!    a *number*. This file is the same fence for a *vocabulary*.
//! 2. **The tint is defined in three theme resolutions out of four.** This
//!    sheet restates the whole light palette under `:root[data-theme="light"]`
//!    so that it beats the `prefers-color-scheme` media block on specificity,
//!    which means "define it beside the dark tokens" is not enough. An
//!    undefined custom property paints as nothing at all, so the chip would
//!    fall back to the card's own background and read as ordinary — a
//!    failure with no error and no visual cue that anything is missing.
//! 3. **The decision spreads back out across the render sites.** The
//!    dashboard has four places that build a label chip. The story's
//!    acceptance criteria require the tint to be decided in one of them, not
//!    hand-listed in four, which is only checkable from outside the file.
//!
//! Plain string parsing rather than a regex or an HTML/CSS parser
//! dependency, matching the precedent set by
//! `tests/dashboard_mutation_deadline.rs` and `tests/dashboard_focus_coverage.rs`.
//! Every parser here carries a positive control, so a scan that stopped
//! matching anything fails instead of reporting a clean file.

use std::collections::BTreeSet;
use std::path::PathBuf;

/// The repository root, which is this package's manifest directory: the root
/// package and the workspace root are the same crate here.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The dashboard's source, read from the repository rather than from any
/// build artifact — a missing file is a finding, not a reason to skip.
fn dashboard() -> String {
    let path: PathBuf = repo_root().join("src/web_dashboard.html");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
}

// ---------------------------------------------------------------------------
// Parsers
// ---------------------------------------------------------------------------

/// The interior of the page's single `<style>` element.
///
/// The CSS scans below are scoped to it rather than run over the whole file,
/// and that is a correctness requirement rather than an optimisation: `/*` is
/// a comment opener in CSS and an ordinary two characters inside a JavaScript
/// string, and this file's script contains one with no closer after it — a
/// comment stripper let loose on the whole document swallows everything from
/// there to EOF and reports whatever survives as the truth.
fn stylesheet(html: &str) -> &str {
    let open = html
        .find("<style>")
        .expect("src/web_dashboard.html must have a <style> element")
        + "<style>".len();
    let close = html[open..]
        .find("</style>")
        .expect("the <style> element must be closed");
    &html[open..open + close]
}

/// Strips `/* … */` comments, replacing each with a single space.
///
/// Load-bearing for the CSS scans below rather than tidiness: this sheet's
/// comments routinely sit inside a declaration block and contain braces and
/// property names of their own, so a brace-matching scan that reads them is
/// reading prose as structure. `tests/dashboard_focus_coverage.rs` learned
/// this the hard way — two real selectors were swallowed into a doc comment
/// the first time it ran.
fn without_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            match source[i + 2..].find("*/") {
                Some(end) => {
                    out.push(' ');
                    i += 2 + end + 2;
                }
                // An unterminated comment swallows the rest of the file; say
                // so rather than silently truncating what every scan below
                // then reads.
                None => panic!("unterminated /* comment in the stylesheet at byte {i}"),
            }
        } else {
            out.push(source[i..].chars().next().expect("a char boundary"));
            i += source[i..]
                .chars()
                .next()
                .expect("a char boundary")
                .len_utf8();
        }
    }
    out
}

/// The `{ … }` block that opens at the first `{` at or after `from`, with its
/// braces balanced. Returns the block's interior.
fn block_after(source: &str, from: usize) -> Option<&str> {
    let open = from + source[from..].find('{')?;
    let mut depth = 0usize;
    for (offset, ch) in source[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&source[open + 1..open + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

/// The interior of the palette block introduced by `anchor`.
///
/// `anchor` is matched literally and the block taken from the next `{`, so
/// the `@media` wrapper is addressed by naming the `:root` *inside* it.
fn palette_block<'a>(css: &'a str, anchor: &str) -> &'a str {
    let at = css.find(anchor).unwrap_or_else(|| {
        panic!("src/web_dashboard.html must contain the palette block `{anchor}`")
    });
    block_after(css, at)
        .unwrap_or_else(|| panic!("the palette block `{anchor}` must have balanced braces"))
}

/// The names in the dashboard's own `var RESERVED_LABELS = [...]` literal.
fn js_reserved_labels(html: &str) -> BTreeSet<String> {
    const DECL: &str = "var RESERVED_LABELS = [";
    let at = html.find(DECL).unwrap_or_else(|| {
        panic!("src/web_dashboard.html must declare `{DECL}…]` — the browser's copy of domain::RESERVED_LABELS")
    });
    let body_start = at + DECL.len();
    let body_end = body_start
        + html[body_start..]
            .find(']')
            .expect("`var RESERVED_LABELS = [` must be closed by `]` on the same declaration");
    let names: BTreeSet<String> = html[body_start..body_end]
        .split(',')
        .map(str::trim)
        .filter(|piece| !piece.is_empty())
        .map(|piece| piece.trim_matches(|c| c == '"' || c == '\'').to_string())
        .collect();
    assert!(
        !names.is_empty(),
        "parsed no names out of `{DECL}…]` — the parser has stopped matching, \
         which would make every assertion below pass vacuously"
    );
    names
}

/// Byte offsets of every occurrence of `needle` in `haystack`.
fn occurrences(haystack: &str, needle: &str) -> Vec<usize> {
    let mut found = Vec::new();
    let mut from = 0usize;
    while let Some(at) = haystack[from..].find(needle) {
        found.push(from + at);
        from += at + needle.len();
    }
    found
}

/// Whether the byte at `at` sits inside a quoted string on its own line.
///
/// Quote parity from the start of the line: an odd number of quotes before a
/// position means the position is inside one. Crude by design and adequate
/// for the question actually being asked — *is this class name being
/// constructed, or merely talked about* — because the thing this file fences
/// is a class string handed to an element, and prose that names the class (a
/// CSS selector, a comment explaining who reads the token) is not that. A
/// scan that could not tell the two apart would fail on the sheet's own
/// documentation, which is a fence nobody can keep.
fn inside_a_string_literal(source: &str, at: usize) -> bool {
    let line_start = source[..at].rfind('\n').map_or(0, |nl| nl + 1);
    source[line_start..at]
        .chars()
        .filter(|c| *c == '"' || *c == '\'')
        .count()
        % 2
        == 1
}

// ---------------------------------------------------------------------------
// The fences
// ---------------------------------------------------------------------------

/// The browser's copy of the reserved names is exactly the domain's.
///
/// Set equality in both directions: a name added on either side, removed from
/// either side, or respelled on either side fails this — the dashboard cannot
/// call `domain::is_human_only`, so this is the only thing keeping the two
/// vocabularies honest.
#[test]
fn the_dashboards_reserved_names_match_the_domains() {
    let expected: BTreeSet<String> = storyhook::domain::RESERVED_LABELS
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    let actual = js_reserved_labels(&dashboard());
    assert_eq!(
        actual, expected,
        "src/web_dashboard.html's RESERVED_LABELS must name exactly \
         domain::RESERVED_LABELS; the browser tints what this list says and \
         the server acts on what domain.rs says"
    );
}

/// The tint token is defined in all four theme resolutions.
///
/// Not three: `:root[data-theme="light"]` restates the whole light palette to
/// beat the media block on specificity, so a token defined only on bare
/// `:root` is undefined the moment a reader picks light explicitly. An
/// undefined custom property paints as nothing, so the chip silently reverts
/// to looking ordinary.
#[test]
fn every_theme_resolution_defines_the_reserved_tint() {
    let html = dashboard();
    let css = without_comments(stylesheet(&html));
    // The `@media` block is addressed through the `:root` inside it; the bare
    // `:root` is the file's first, so it is found by searching from 0.
    let media_at = css
        .find("@media (prefers-color-scheme: dark)")
        .expect("the dark media query must exist");
    for (name, block) in [
        ("bare :root", palette_block(&css, ":root {")),
        (
            "@media (prefers-color-scheme: dark) :root",
            palette_block(&css[media_at..], ":root"),
        ),
        (
            r#":root[data-theme="dark"]"#,
            palette_block(&css, r#":root[data-theme="dark"]"#),
        ),
        (
            r#":root[data-theme="light"]"#,
            palette_block(&css, r#":root[data-theme="light"]"#),
        ),
    ] {
        // Positive control: if the block finder ever addressed something that
        // is not a palette block, `--warn` would be missing too and the
        // `--warn-soft` assertion below would be reporting on the wrong text.
        assert!(
            block.contains("--warn:"),
            "the `{name}` block does not define `--warn`, so this scan is not \
             reading a palette block and its verdict means nothing"
        );
        assert!(
            block.contains("--warn-soft:"),
            "the `{name}` block defines `--warn` but not `--warn-soft`; the \
             reserved-label chip resolves to nothing in that theme and reads \
             as an ordinary label"
        );
    }
}

/// Exactly one place in the file *constructs* the reserved chip's class.
///
/// The dashboard draws labels in four places. The story's acceptance criteria
/// require the tint to be decided once and read four times, rather than each
/// render site testing the names for itself — the hand-kept-list shape this
/// project has paid for five times over. Nothing inside the file can check
/// that, because a second site would be perfectly valid JavaScript.
///
/// "Constructs" means the class name appears in a string literal. A CSS
/// selector and a comment naming the class are prose about the mechanism, not
/// a second copy of it, and are deliberately not counted.
#[test]
fn only_one_place_decides_the_reserved_tint() {
    let html = dashboard();
    let constructed: Vec<usize> = occurrences(&html, "chip-reserved")
        .into_iter()
        .filter(|at| inside_a_string_literal(&html, *at))
        .collect();
    assert_eq!(
        constructed.len(),
        1,
        "`chip-reserved` must be built into a class string in exactly one place \
         — labelChipClass — but {} places do it; a render site that decides the \
         tint itself is the per-site hand-kept list SH-454's acceptance \
         criteria forbid",
        constructed.len()
    );

    // Positive control on the sheet's own half: the class the one call site
    // hands out has to be a rule somebody wrote, or every chip carries a name
    // that styles nothing.
    let css = without_comments(stylesheet(&html));
    assert_eq!(
        occurrences(&css, ".chip-reserved").len(),
        1,
        "the stylesheet must declare `.chip-reserved` exactly once"
    );

    const FN: &str = "function labelChipClass(";
    let fn_at = html.find(FN).expect(
        "src/web_dashboard.html must define `labelChipClass` — the one place the tint is decided",
    );
    let fn_body = block_after(&html, fn_at).expect("labelChipClass must have balanced braces");
    let body_at = fn_at
        + html[fn_at..]
            .find(fn_body)
            .expect("the body follows its own signature");
    assert!(
        (body_at..body_at + fn_body.len()).contains(&constructed[0]),
        "the constructed `chip-reserved` must sit inside `labelChipClass`, not at a render site"
    );
}

/// The comment stripper and the block finder both actually work.
///
/// A scan that quietly matched nothing would let every test above pass on a
/// file with no tint in it at all, which is the failure mode this project
/// files positive controls for (SH-364).
#[test]
fn the_parsers_report_a_planted_fault() {
    let planted = r#"
:root { /* a comment with a } brace and --warn-soft: #000 in it */ --warn: #b6540a; }
:root[data-theme="dark"] { --warn: #d97a1f; --warn-soft: #2e1f10; }
"#;
    let css = without_comments(planted);
    assert!(
        !palette_block(&css, ":root {").contains("--warn-soft:"),
        "the comment stripper must remove a `--warn-soft` that only appears \
         inside a comment, or the theme scan can be satisfied by prose"
    );
    assert!(
        palette_block(&css, r#":root[data-theme="dark"]"#).contains("--warn-soft:"),
        "the block finder must see a real declaration in the block it addresses"
    );

    assert_eq!(
        js_reserved_labels(r#"  var RESERVED_LABELS = ["a", "b"];"#),
        BTreeSet::from(["a".to_string(), "b".to_string()]),
        "the reserved-name parser must read a two-name array literal"
    );

    assert_eq!(
        occurrences("chip-reserved .chip-reserved", "chip-reserved"),
        vec![0, 15],
        "the occurrence scan must find every hit, not just the first"
    );

    let quoted = r#"x = "chip-reserved"; /* .chip-reserved in prose */"#;
    let hits: Vec<bool> = occurrences(quoted, "chip-reserved")
        .into_iter()
        .map(|at| inside_a_string_literal(quoted, at))
        .collect();
    assert_eq!(
        hits,
        vec![true, false],
        "the string-literal reader must count a constructed class and skip prose \
         that merely names it"
    );
}
