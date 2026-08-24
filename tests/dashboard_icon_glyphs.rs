//! Fences the toolbar's character-as-icon defect in `src/web_dashboard.html`
//! (SH-444).
//!
//! The dashboard's Home/Settings/Drafts toolbar icons were bare Unicode
//! characters: U+2302 HOUSE and U+270E LOWER RIGHT PENCIL are not emoji at
//! all (neither has an emoji presentation, and neither is covered by any
//! font in the page's `--sans` stack, so both rendered from an unspecified
//! platform fallback font at an arbitrary weight, unrelated to the
//! 600-weight text beside them); U+2699 GEAR *is* emoji-capable but shipped
//! **unqualified** -- no trailing U+FE0F (VARIATION SELECTOR-16) -- so its
//! text-vs-colour presentation was undetermined per platform on top of
//! that. The fix redraws every character-as-icon control as an inline `<svg
//! class="icon" ...>` (the pattern `.search-wrap svg` already used).
//!
//! `every_btn_icon_span_holds_a_shape`, below, is the derived-corpus style
//! `tests/dashboard_deadline_knobs.rs` already uses: every `class="btn-icon"`
//! span's inner content contains `<svg` and no non-ASCII character, derived
//! from the class's own occurrences rather than a hand-kept list of button
//! ids -- a fourth `.btn-icon` control added later is covered for free. It
//! carries its own positive control and a corpus floor, so a parser that
//! stopped matching cannot report a clean tree by accident (SH-364's own
//! doctrine).
//!
//! This is a wiring fence, not a behaviour one, the same limit
//! `tests/dashboard_focus_coverage.rs` states for itself: it proves the
//! topbar's icon slots hold shapes, never that a shape is legible, sized
//! correctly, or draws the right icon. That half belongs to
//! `e2e/specs/icon-shapes.spec.ts` and `icon-shapes.mobile.spec.ts`.
//!
//! A second, independent instance of the same undetermined-presentation
//! defect (U+1F5C4 FILE CABINET, the archived flag/banner, shipped with no
//! U+FE0F) was found during this investigation and is fixed and fenced
//! separately, in its own commit -- see this file's own later addition.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn dashboard() -> String {
    let path = repo_root().join("src/web_dashboard.html");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
}

/// Every `<span class="btn-icon" ...>...</span>` occurrence's inner content,
/// derived from the class's own occurrences rather than a hand-kept list of
/// button ids.
fn btn_icon_span_contents(source: &str) -> Vec<String> {
    const MARKER: &str = "class=\"btn-icon\"";
    let mut found = Vec::new();
    let mut rest = source;
    while let Some(at) = rest.find(MARKER) {
        let after_class = &rest[at + MARKER.len()..];
        let Some(gt) = after_class.find('>') else {
            break;
        };
        let after_open = &after_class[gt + 1..];
        let Some(close) = after_open.find("</span>") else {
            panic!(
                "a `class=\"btn-icon\"` span never closes with </span> -- this scan's assumption \
                 about the tag shape has drifted from the file"
            );
        };
        found.push(after_open[..close].to_string());
        rest = &after_open[close..];
    }
    found
}

#[test]
fn every_btn_icon_span_holds_a_shape() {
    let source = dashboard();
    let spans = btn_icon_span_contents(&source);

    assert!(
        spans.len() >= 3,
        "expected to find at least 3 `class=\"btn-icon\"` spans (Home, Settings, Drafts), found \
         {}: either they were deleted, or btn_icon_span_contents() no longer matches this file's \
         shape -- in which case this fence is clearing a tree it never read",
        spans.len()
    );

    for span in &spans {
        assert!(
            span.contains("<svg"),
            "a `.btn-icon` span holds {span:?}, which contains no <svg -- every icon-only button \
             must draw a shape, not a character (SH-444)"
        );
        assert!(
            span.is_ascii(),
            "a `.btn-icon` span holds {span:?}, which contains a non-ASCII character -- an icon \
             drawn as an inline <svg> needs none; a stray character here is exactly the defect \
             this fence exists to catch"
        );
    }
}

// ---------------------------------------------------------------------------
// Positive control -- SH-364's doctrine: a parser that stopped matching must
// not report a clean tree by accident.
// ---------------------------------------------------------------------------

#[test]
fn the_scan_can_still_see_a_bare_character() {
    let offending = r#"<span class="btn-icon" aria-hidden="true">⌂</span>"#;
    let spans = btn_icon_span_contents(offending);
    assert_eq!(spans, vec!["⌂".to_string()]);

    let clean = r#"<span class="btn-icon" aria-hidden="true"><svg class="icon"></svg></span>"#;
    let spans = btn_icon_span_contents(clean);
    assert_eq!(spans, vec!["<svg class=\"icon\"></svg>".to_string()]);
}
