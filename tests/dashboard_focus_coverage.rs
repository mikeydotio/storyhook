//! Fails when a focus rule in `src/web_dashboard.html` has nothing measuring
//! it, and when a measurement in `e2e/specs/*.spec.ts` names a selector the
//! sheet no longer declares (SH-360).
//!
//! SH-338 built an instrument -- `measureFocusIndicator` in
//! `e2e/specs/support.ts` -- that reaches a control with real Tab presses,
//! reads the resolved indicator colour off `getComputedStyle`, composites
//! the backdrop from the real ancestor chain, and fails below WCAG SC
//! 1.4.11's 3:1. It shipped pointed at exactly two of the sheet's nine
//! `:focus`/`:focus-visible` selectors. SH-360 measured the other seven --
//! but a hand-maintained "the seven we found" list is exactly the shape that
//! let the original two-control gap sit unnoticed, so this file derives BOTH
//! sides instead of naming either one.
//!
//! ## The two sides, and why both are derived
//!
//! **Declared**: every selector in the dashboard's single `<style>` block
//! whose text contains a focus pseudo-class, reduced to its BASE selector
//! (everything before the pseudo-class) -- `.toast-dismiss:focus-visible`
//! and `.toast-dismiss:focus` both reduce to `.toast-dismiss`, which is what
//! lets a spec measure "whatever holds focus" (SH-338's own convention, kept
//! on `measureFocusIndicator`'s doc comment) without the two sides disagreeing
//! on a pseudo-class spelling neither cares about.
//!
//! **Measured**: every string-literal second argument to a
//! `measureFocusIndicator(` call across the tracked browser specs, reduced
//! the same way.
//!
//! Set equality, both directions:
//!
//! - a declared selector with no measurement is what closes SH-338's own gap
//!   -- `every_declared_focus_rule_is_measured_by_a_spec`;
//! - a measured selector the sheet no longer declares is a stale claim a
//!   HAND-maintained registry could not have caught either --
//!   `every_measured_selector_is_still_declared_by_the_sheet`.
//!
//! ## Why Rust, and not a CSSOM scan from inside the browser suite
//!
//! CSSOM would split grouped selectors and resolve `@media` nesting for
//! free, both real advantages -- but neither is actually needed here: a
//! focus rule inside `@media` still has to be measured exactly like one
//! outside it, so there is no different treatment to gate on, and the brace
//! scan below (`focus_rule_selector_spans`) finds a rule's selector text
//! regardless of nesting depth without ever tracking it. What CSSOM cannot
//! do is check the OTHER half of this claim: which selectors are measured,
//! and by what. Reading spec source from inside a running Playwright spec to
//! check that would report a repo-hygiene finding as a two-engine browser
//! failure, inside a ~9-minute leg, instead of Rust's own sub-second
//! `cargo test`. The `tests/dead_public_surface.rs` / `tests/store_
//! isolation.rs` style this story asks for is Rust, `git ls-files`-derived,
//! for the same reason.
//!
//! ## What this fence cannot catch -- knowingly traded away
//!
//! It is a WIRING fence, not a behaviour fence: it proves every declared
//! focus rule has a call site naming it, never that the call reaches the
//! right pixel -- that is `e2e/specs/focus-indicator.spec.ts`'s job, backed
//! by its own mutation battery (recorded in its commit history, not
//! restated here).
//!
//! It enumerates DECLARED focus rules, so it structurally cannot find
//! SH-338's original defect class: a focusable control with NO rule at all,
//! falling back to the user agent's colourless ring. `.btn` and
//! `.ctxmenu-item` are exactly that today, deliberately, page-wide -- they
//! declare no focus rule, so they never enter the declared set and this
//! fence has nothing to say about them. Closing that gap needs a live
//! DOM/tabindex walk, a different instrument, and a story of its own.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Reads a repository file, failing with the path rather than `None` -- a
/// missing file is a finding, not a reason to skip (same idiom as
/// `tests/dashboard_enter_submit_guard.rs::read`).
fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
}

// ---------------------------------------------------------------------------
// Declared side: src/web_dashboard.html's own <style> block
// ---------------------------------------------------------------------------

/// The text strictly between the dashboard's `<style>` and `</style>` tags.
/// Panics unless there is exactly one of each -- a second style block is a
/// second place to look, and this function would otherwise silently vouch
/// for only the first (the same doctrine `tests/e2e_fixture_hygiene.rs`
/// states for its own single-file assumptions).
fn style_block(html: &str) -> &str {
    const OPEN: &str = "<style>";
    const CLOSE: &str = "</style>";
    let opens = html.matches(OPEN).count();
    let closes = html.matches(CLOSE).count();
    assert_eq!(
        (opens, closes),
        (1, 1),
        "src/web_dashboard.html has {opens} <style> tag(s) and {closes} </style> tag(s) -- this \
         scan assumes exactly one of each"
    );
    let start = html.find(OPEN).unwrap() + OPEN.len();
    let end = html.find(CLOSE).unwrap();
    &html[start..end]
}

/// `css` with `/* ... */` block comments blanked out (spaces, never
/// removed), so line/byte structure survives for callers that report a
/// position back to the reader. CSS has no `//` line-comment syntax, unlike
/// `tests/dashboard_enter_submit_guard.rs`'s JS stripper, which this
/// function is otherwise the same shape as.
///
/// Blanking rather than deleting matters here for a reason JS comments
/// don't create: a doc comment sits directly above nearly every rule in this
/// sheet, with no intervening brace, so `focus_rule_selector_spans`'s
/// "text since the last brace" span would otherwise swallow the ENTIRE
/// preceding comment along with the real selector -- caught by
/// `the_scan_still_sees_the_style_blocks_own_focus_rules` failing to find
/// `.dispatch-history-dismiss` and `.notice-scroll`, both doc-commented
/// immediately above their rule, with the "selector" it actually captured
/// being the comment's own prose.
fn strip_css_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let chars: Vec<char> = css.chars().collect();
    let mut i = 0;
    let mut in_comment = false;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied().unwrap_or('\0');
        if c == '\n' {
            // Unlike the JS stripper's `in_line`, `in_comment` is NOT reset
            // here: CSS has no line-comment syntax, so a `/* */` block is
            // the only comment kind, and it routinely spans several lines
            // in this sheet's own doc comments -- resetting on every
            // newline would end one at its first line break instead of its
            // closing `*/`.
            out.push('\n');
            i += 1;
            continue;
        }
        if in_comment {
            if c == '*' && next == '/' {
                in_comment = false;
                out.push(' ');
                out.push(' ');
                i += 2;
                continue;
            }
            out.push(' ');
            i += 1;
            continue;
        }
        if c == '/' && next == '*' {
            in_comment = true;
            out.push(' ');
            out.push(' ');
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Every selector-like span in `style_block` (comments already stripped)
/// that contains a focus pseudo-class -- the raw text between one brace and
/// the next, kept whenever it names `:focus`.
///
/// No brace-depth tracking is needed to tell a rule's own selector apart
/// from an enclosing `@media (...)` prelude: the span since the LAST `{` or
/// `}` (whichever came later), up to THIS `{`, is always exactly one rule's
/// selector text or one at-rule's prelude, at any nesting depth -- an
/// at-rule's prelude never contains `:focus`, so it is filtered out for
/// free rather than by recognising `@media` specifically.
fn focus_rule_selector_spans(style_block: &str) -> Vec<String> {
    let mut spans = Vec::new();
    let mut last = 0usize;
    for (i, byte) in style_block.bytes().enumerate() {
        match byte {
            b'{' => {
                let span = style_block[last..i].trim();
                if span.contains(":focus") {
                    spans.push(span.to_string());
                }
                last = i + 1;
            }
            b'}' => {
                last = i + 1;
            }
            _ => {}
        }
    }
    spans
}

/// Splits a comma-separated selector list at paren-depth 0, so a functional
/// pseudo-class hiding its own comma (`:is(.a, .b):focus`) is kept as one
/// piece rather than torn in half.
fn split_selector_list(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(text[start..i].trim().to_string());
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(text[start..].trim().to_string());
    parts
}

/// Reduces one compound selector ending in a focus pseudo-class to its BASE
/// selector -- everything before that pseudo-class. Checked longest-first
/// (`:focus-visible`/`:focus-within` before bare `:focus`), since the bare
/// form is a literal substring of the other two and would otherwise split
/// in the wrong place.
///
/// Panics rather than guessing on a shape not taught: trailing content after
/// the pseudo-class (a descendant combinator, `.a:focus-visible .b`, styles
/// a DESCENDANT of `.a:focus-visible` -- not a claim about `.a`'s own
/// indicator, so there is no single base identity to return) and a string
/// with no recognised focus pseudo-class at all.
fn base_selector(selector: &str) -> String {
    let selector = selector.trim();
    for suffix in [":focus-visible", ":focus-within", ":focus"] {
        if let Some(idx) = selector.find(suffix) {
            let before = &selector[..idx];
            let after = &selector[idx + suffix.len()..];
            assert!(
                after.trim().is_empty(),
                "base_selector({selector:?}): trailing content {after:?} after {suffix:?} -- a \
                 focus pseudo-class followed by more selector (a descendant combinator, an \
                 immediately chained pseudo-class) has no single base identity this function has \
                 been taught to return; teach it the right reduction before this scan can vouch \
                 for a selector shaped like this"
            );
            return before.trim().to_string();
        }
    }
    panic!(
        "base_selector({selector:?}): no recognised focus pseudo-class (:focus, :focus-visible, :focus-within)"
    );
}

/// Every focus rule `src/web_dashboard.html`'s `<style>` block declares,
/// reduced to base selectors.
fn declared_focus_selectors() -> BTreeSet<String> {
    let html = read("src/web_dashboard.html");
    let block = style_block(&html);
    let stripped = strip_css_comments(block);
    let mut result = BTreeSet::new();
    for span in focus_rule_selector_spans(&stripped) {
        for piece in split_selector_list(&span) {
            if piece.contains(":focus") {
                result.insert(base_selector(&piece));
            }
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Measured side: measureFocusIndicator( call sites across the browser specs
// ---------------------------------------------------------------------------

/// Every tracked browser spec, paired with its contents -- same enumerator
/// shape as `tests/e2e_browser_coverage.rs::all_specs` and `tests/
/// e2e_fixture_hygiene.rs::all_specs`, kept local rather than shared: each
/// `tests/*.rs` file compiles as its own crate.
fn all_specs(root: &Path) -> Vec<(String, String)> {
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z", "--", "e2e/specs/*.spec.ts"])
        .output()
        .expect("listing this repository's tracked browser specs");
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

const CALL_MARKER: &str = "measureFocusIndicator(";

/// Every selector-argument string literal passed as the SECOND argument to
/// `measureFocusIndicator(` in `text` (a spec's own source), in the order
/// they appear.
///
/// Panics -- rather than skipping -- on a call whose second argument is not
/// a string literal (a template string, a variable, an expression): this
/// scan reads call sites STATICALLY, so an argument it cannot read is
/// exactly the shape that would silently exempt whatever it names from
/// coverage. `measureFocusIndicator`'s own doc comment states the same
/// constraint from the spec-author's side.
fn call_arg_selectors(text: &str, spec_path: &str) -> Vec<String> {
    let mut selectors = Vec::new();
    let mut search_from = 0usize;
    while let Some(rel_idx) = text[search_from..].find(CALL_MARKER) {
        let call_start = search_from + rel_idx + CALL_MARKER.len();
        let comma_offset = text[call_start..].find(',').unwrap_or_else(|| {
            panic!(
                "{spec_path}: a `{CALL_MARKER}` call has no comma after its first argument -- \
                 the call is malformed, or this scan's assumption about its shape has drifted"
            )
        });
        let second_arg_region = &text[call_start + comma_offset + 1..];
        let trimmed = second_arg_region.trim_start();
        let leading_ws = second_arg_region.len() - trimmed.len();
        let quote_pos = call_start + comma_offset + 1 + leading_ws;
        assert_eq!(
            text.as_bytes().get(quote_pos),
            Some(&b'"'),
            "{spec_path}: measureFocusIndicator's selector argument at byte {quote_pos} is not a \
             string literal (found {:?}) -- it must be a literal so this scan can read it \
             statically; a variable or template expression here would silently exempt whatever \
             selector it names from coverage",
            &text[quote_pos..(quote_pos + 40).min(text.len())]
        );
        let string_start = quote_pos + 1;
        let close_offset = text[string_start..].find('"').unwrap_or_else(|| {
            panic!("{spec_path}: a measureFocusIndicator( call's selector string never closes its quote")
        });
        selectors.push(text[string_start..string_start + close_offset].to_string());
        search_from = string_start + close_offset;
    }
    selectors
}

/// Every selector `measureFocusIndicator(` is called with, anywhere in the
/// tracked browser specs, reduced to base selectors.
fn measured_focus_selectors() -> BTreeSet<String> {
    let root = repo_root();
    let mut result = BTreeSet::new();
    for (path, text) in all_specs(&root) {
        for literal in call_arg_selectors(&text, &path) {
            result.insert(base_selector(&literal));
        }
    }
    result
}

// ---------------------------------------------------------------------------
// The two fences
// ---------------------------------------------------------------------------

#[test]
fn every_declared_focus_rule_is_measured_by_a_spec() {
    let declared = declared_focus_selectors();
    let measured = measured_focus_selectors();
    let unmeasured: Vec<&String> = declared.difference(&measured).collect();
    assert!(
        unmeasured.is_empty(),
        "src/web_dashboard.html declares a focus rule for {unmeasured:?} that no browser spec \
         measures. Every author-declared focus indicator must be measured in all four theme \
         resolutions via `measureFocusIndicator` (SH-360, SH-338) -- a hand-maintained list of \
         \"the focus rules we remembered\" is exactly the shape that let the original two-control \
         gap sit unnoticed. Declared selectors: {declared:?}. Measured selectors: {measured:?}."
    );
}

#[test]
fn every_measured_selector_is_still_declared_by_the_sheet() {
    let declared = declared_focus_selectors();
    let measured = measured_focus_selectors();
    let stale: Vec<&String> = measured.difference(&declared).collect();
    assert!(
        stale.is_empty(),
        "a browser spec calls measureFocusIndicator naming {stale:?}, but src/web_dashboard.html \
         declares no focus rule with that base selector any more -- a stale measurement, which a \
         hand-maintained coverage registry could not have caught either. Either the CSS rule was \
         renamed/removed and the spec needs updating, or this scan's base_selector() reduction has \
         drifted from the sheet's actual selectors. Declared selectors: {declared:?}. Measured \
         selectors: {measured:?}."
    );
}

// ---------------------------------------------------------------------------
// Positive control -- SH-364's lesson: a parser that stopped seeing rules
// would otherwise report a clean, wrong tree.
// ---------------------------------------------------------------------------

#[test]
fn the_scan_still_sees_the_style_blocks_own_focus_rules() {
    let html = read("src/web_dashboard.html");
    let block = style_block(&html);
    assert!(
        block.len() > 10_000,
        "the extracted <style> block is only {} bytes -- style_block() is not finding the real \
         block",
        block.len()
    );

    let declared = declared_focus_selectors();
    assert!(
        declared.len() >= 8,
        "declared_focus_selectors() found only {} selector(s): {declared:?} -- too few to trust \
         the pattern; the comma/brace scan may have drifted from the sheet's actual shape",
        declared.len()
    );

    // The grouped rule at `.dispatch-history-dismiss:focus-visible,
    // .toast-dismiss:focus-visible` must split into exactly these two base
    // selectors -- the comma-split's own positive control.
    assert!(declared.contains(".dispatch-history-dismiss"));
    assert!(declared.contains(".toast-dismiss"));

    // Every one of SH-360's own seven, confirming this scan reads the real
    // file rather than a fixture that happens to satisfy the two asserts
    // above.
    for selector in [
        ".card",
        "tbody tr",
        ".description-view",
        ".notice-scroll",
        ".search-input",
        ".drawer-title",
        ".description-field",
    ] {
        assert!(
            declared.contains(selector),
            "declared_focus_selectors() did not find {selector:?}, which src/web_dashboard.html \
             is known to declare a focus rule for. Found: {declared:?}"
        );
    }
}

#[test]
fn the_scan_still_sees_every_real_call_site() {
    let measured = measured_focus_selectors();
    assert!(
        measured.len() >= 9,
        "measured_focus_selectors() found only {} selector(s): {measured:?} -- fewer than the \
         nine focus rules this suite is known to measure; call_arg_selectors()'s pattern may have \
         drifted from the specs' actual shape",
        measured.len()
    );
}

// ---------------------------------------------------------------------------
// Unit tests for the parsing primitives, including both-direction mutation
// checks -- this project's own standard for a new assertion.
// ---------------------------------------------------------------------------

#[test]
fn style_block_extracts_only_the_text_between_the_tags() {
    let html = "before<style>.a:focus{outline:none}</style>after";
    assert_eq!(style_block(html), ".a:focus{outline:none}");
}

#[test]
fn style_block_panics_on_anything_but_exactly_one_pair() {
    assert!(std::panic::catch_unwind(|| style_block("<style>a</style><style>b</style>")).is_err());
    assert!(std::panic::catch_unwind(|| style_block("no style tags here")).is_err());
}

#[test]
fn strip_css_comments_blanks_a_block_comment_without_hiding_the_selector_beside_it() {
    // Reproduces the actual defect this scan shipped with once: a doc
    // comment sitting directly above a rule, with no intervening brace, so
    // an unstripped scan's "text since the last brace" span swallows the
    // ENTIRE comment along with the real selector -- exactly the shape
    // `.notice-scroll` and `.dispatch-history-dismiss` have in the real
    // sheet (measured, not invented: `git grep -B2 'notice-scroll:focus'`).
    let css = "/* A scroller that becomes a tab stop\n   needs a name too. */\n.notice-scroll:focus-visible { outline: 2px solid red; }";
    let stripped = strip_css_comments(css);
    let spans = focus_rule_selector_spans(&stripped);
    assert_eq!(
        spans,
        vec![".notice-scroll:focus-visible".to_string()],
        "a preceding multi-line doc comment must not become part of the rule's own selector span"
    );
}

#[test]
fn strip_css_comments_preserves_a_multi_line_block_comment_across_its_own_newlines() {
    // The bug this project's own JS stripper does NOT have and this CSS one
    // very nearly shipped with: resetting the "inside a comment" flag on
    // every newline (right for JS's `//`, which DOES end at one) would end
    // a CSS `/* */` block at its first internal line break instead of its
    // real closing `*/` -- leaking "line two */" as if it were code, since
    // CSS has no line-comment syntax to have actually ended anything there.
    let css = "/* line one\n   line two */\n.selector:focus{}";
    let stripped = strip_css_comments(css);
    assert!(
        !stripped.contains("line two"),
        "comment content must stay blanked across its own internal newline; got: {stripped:?}"
    );
    assert!(
        stripped.contains(".selector:focus"),
        "real code after a multi-line comment must survive stripping; got: {stripped:?}"
    );
}

#[test]
fn focus_rule_selector_spans_finds_a_rule_at_any_media_nesting_depth() {
    // No brace-depth tracking, on purpose (this file's own doc comment
    // argues why) -- this is the test that proves the claim rather than
    // merely asserting it.
    let css = r#"
        .plain:focus { outline: none; }
        @media (prefers-color-scheme: dark) {
          .nested:focus-visible { outline: 2px solid red; }
        }
        @media (min-width: 40rem) {
          @media (prefers-color-scheme: dark) {
            .doubly-nested:focus { outline: 1px solid blue; }
          }
        }
        .not-focused { color: red; }
    "#;
    let spans = focus_rule_selector_spans(css);
    assert_eq!(
        spans,
        vec![
            ".plain:focus".to_string(),
            ".nested:focus-visible".to_string(),
            ".doubly-nested:focus".to_string(),
        ]
    );
}

#[test]
fn split_selector_list_keeps_a_functional_pseudo_classs_own_comma_together() {
    assert_eq!(
        split_selector_list(".a, .b:focus"),
        vec![".a".to_string(), ".b:focus".to_string()]
    );
    assert_eq!(
        split_selector_list(":is(.a, .b):focus"),
        vec![":is(.a, .b):focus".to_string()],
        "a comma inside a functional pseudo-class must not split the selector in half"
    );
}

#[test]
fn base_selector_reduces_every_shape_this_sheet_actually_uses() {
    assert_eq!(base_selector(".card:focus-visible"), ".card");
    assert_eq!(base_selector("tbody tr:focus-visible"), "tbody tr");
    assert_eq!(base_selector(".toast-dismiss:focus"), ".toast-dismiss");
    assert_eq!(base_selector(".search-input:focus"), ".search-input");
}

#[test]
fn base_selector_panics_on_a_descendant_combinator_after_the_pseudo_class() {
    let panicked = std::panic::catch_unwind(|| base_selector(".a:focus-visible .b")).is_err();
    assert!(
        panicked,
        "base_selector must refuse to guess a base identity for a focus pseudo-class followed by \
         more selector content -- that styles a descendant, not the element itself"
    );
}

#[test]
fn base_selector_panics_on_no_focus_pseudo_class_at_all() {
    let panicked = std::panic::catch_unwind(|| base_selector(".no-focus-here")).is_err();
    assert!(panicked);
}

#[test]
fn call_arg_selectors_reads_the_real_call_shapes_this_suite_writes() {
    let text = r##"
  await measureFocusIndicator(page, ".toast-dismiss:focus", "a toast's ring", () =>
    tabOnto(page, "#toast-dismiss-all", ".toast-dismiss"),
  );
  await measureFocusIndicator(page, ".search-input:focus", "the search box", () =>
    tabOnto(page, "#projsel-btn", ".search-input"),
  );
"##;
    assert_eq!(
        call_arg_selectors(text, "fixture.spec.ts"),
        vec![
            ".toast-dismiss:focus".to_string(),
            ".search-input:focus".to_string()
        ]
    );
}

#[test]
fn call_arg_selectors_panics_on_a_non_literal_second_argument() {
    let text = r#"await measureFocusIndicator(page, someVariable, "where", reach);"#;
    let panicked =
        std::panic::catch_unwind(|| call_arg_selectors(text, "fixture.spec.ts")).is_err();
    assert!(
        panicked,
        "a non-literal selector argument must be refused, not silently skipped -- silently \
         skipping it is exactly how a call site could exempt itself from this scan's coverage"
    );
}

#[test]
fn call_arg_selectors_panics_on_an_unclosed_quote() {
    // No second `"` anywhere after the opening one -- genuinely unclosed,
    // unlike a string that merely closes somewhere unexpected.
    let text = "await measureFocusIndicator(page, \".card:focus";
    let panicked =
        std::panic::catch_unwind(|| call_arg_selectors(text, "fixture.spec.ts")).is_err();
    assert!(panicked);
}
