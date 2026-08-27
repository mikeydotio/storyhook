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
//! ## `no_raw_disclosure_triangle_is_left`
//!
//! SH-447 supersedes SH-444's original exception for U+25B8 BLACK
//! RIGHT-POINTING SMALL TRIANGLE and U+25BE BLACK DOWN-POINTING SMALL
//! TRIANGLE. They have deterministic text presentation, but the reported
//! dashboard proved that was not sufficient: at the 9-10px font sizes used
//! by the Filters and dropdown controls, the visible triangle occupied only
//! a fraction of its already-small font box. Every control whose indicator
//! means “reveals hidden content” now draws the same fixed 14px SVG chevron.
//! The source scan below prevents either raw character from silently
//! returning through static markup or a JS-built control; the browser spec
//! proves the resulting shapes' size and direction.
//!
//! ## `no_pictographic_character_is_left_unqualified`
//!
//! A second, independent instance of the same undetermined-presentation
//! defect was found during this investigation: U+1F5C4 FILE CABINET (the
//! archived flag/banner) shipped with no U+FE0F, unlike this file's other
//! emoji, U+1F3F7 LABEL (`typeGlyph()`'s fallback), which was already
//! correctly qualified -- proving the convention already existed and simply
//! wasn't applied everywhere. Fixed by qualifying it (`🗄` -> `🗄️`) rather
//! than converting it to an `<svg>`: it sits inline inside a text badge and
//! banner sentence, unlike the seven controls above, and this file's own
//! qualified-emoji convention already covers exactly this case.
//!
//! This fence generalizes past that one instance: every character in
//! Unicode blocks that hold pictographic symbols -- Misc Technical
//! (U+2300-23FF), Misc Symbols (U+2600-26FF), Dingbats (U+2700-27BF), Misc
//! Symbols and Arrows (U+2B00-2BFF), and the supplemental pictographic
//! plane (U+1F000-1FAFF) -- must be immediately followed by U+FE0F, UNLESS
//! it is U+2713 CHECK MARK, this file's one documented exception:
//! `Emoji_Presentation=No` like the two non-emoji characters the first fence
//! above removed, covered by every UI font, and its exact text is a pinned
//! contract across several `e2e/specs/*.spec.ts` files (a character-for-
//! character change there is out of this story's scope). The rule this
//! enforces: a pictographic character either carries U+FE0F -- someone
//! deliberately chose a colour emoji -- or it does not belong in this file
//! at all; draw a shape instead.
//!
//! Scans the file's raw bytes, comments included, on purpose: an unqualified
//! pictographic character sitting in a doc comment is not rendered to a
//! reader, but it is exactly the kind of copy-paste seed that ends up in
//! live markup next -- and it is why `svg.icon`'s own doc comment above
//! `.btn-icon` in `src/web_dashboard.html` names its three former glyphs by
//! codepoint rather than pasting them.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn dashboard() -> String {
    let path = repo_root().join("src/web_dashboard.html");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
}

const VARIATION_SELECTOR_16: char = '\u{FE0F}';

/// U+2713 CHECK MARK: this file's one documented pictographic exception --
/// see this module's own doc comment for why.
const DOCUMENTED_EXCEPTION: char = '\u{2713}';

/// True if `c` sits in a Unicode block this file's convention requires a
/// trailing U+FE0F for (unless it is the documented exception).
fn is_pictographic(c: char) -> bool {
    let cp = c as u32;
    matches!(cp,
        0x2300..=0x23FF   // Miscellaneous Technical
        | 0x2600..=0x26FF // Miscellaneous Symbols
        | 0x2700..=0x27BF // Dingbats
        | 0x2B00..=0x2BFF // Miscellaneous Symbols and Arrows
        | 0x1F000..=0x1FAFF // Supplemental pictographic planes
    )
}

/// Every pictographic character in `source` that is not immediately
/// followed by U+FE0F and is not the documented exception, paired with its
/// codepoint for a readable failure message.
fn unqualified_pictographs(source: &str) -> Vec<(char, u32)> {
    let chars: Vec<char> = source.chars().collect();
    let mut found = Vec::new();
    for (i, &c) in chars.iter().enumerate() {
        if !is_pictographic(c) || c == DOCUMENTED_EXCEPTION {
            continue;
        }
        let next = chars.get(i + 1).copied();
        if next != Some(VARIATION_SELECTOR_16) {
            found.push((c, c as u32));
        }
    }
    found
}

#[test]
fn no_pictographic_character_is_left_unqualified() {
    let source = dashboard();
    let offenders = unqualified_pictographs(&source);
    assert!(
        offenders.is_empty(),
        "src/web_dashboard.html contains {} pictographic character(s) with no trailing U+FE0F \
         and no exemption: {offenders:?}. Either it is not actually an emoji and belongs in an \
         inline <svg class=\"icon\"> instead (SH-444), or it is a deliberate colour emoji and \
         needs qualifying with U+FE0F the way typeGlyph()'s label fallback already is.",
        offenders.len()
    );
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

fn raw_disclosure_triangles(source: &str) -> Vec<(char, u32)> {
    source
        .chars()
        .filter(|c| matches!(*c as u32, 0x25B8 | 0x25BE))
        .map(|c| (c, c as u32))
        .collect()
}

#[test]
fn no_raw_disclosure_triangle_is_left() {
    let source = dashboard();
    let offenders = raw_disclosure_triangles(&source);
    assert!(
        offenders.is_empty(),
        "src/web_dashboard.html contains raw disclosure triangle character(s): {offenders:?}. \
         Hidden-content controls must use the fixed-size inline SVG disclosure icon (SH-447)."
    );
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

#[test]
fn the_pictograph_scan_can_still_see_an_offender() {
    // Assembled at run time, the same style
    // `dashboard_error_reporting.rs::the_scan_can_still_see_an_offending_line`
    // uses, so the literal offending sequence never sits in this source file
    // as something the real scan over the real tree could trip on.
    let gear = char::from_u32(0x2699).unwrap();
    let offending = format!("<span>{gear}</span>");
    let offenders = unqualified_pictographs(&offending);
    assert_eq!(offenders, vec![(gear, 0x2699)]);

    // Qualifying it with U+FE0F must clear the finding.
    let qualified = format!("<span>{gear}{VARIATION_SELECTOR_16}</span>");
    assert!(unqualified_pictographs(&qualified).is_empty());

    // The documented exception must never be flagged, qualified or not.
    let check = format!("<span>{DOCUMENTED_EXCEPTION}</span>");
    assert!(unqualified_pictographs(&check).is_empty());
}

#[test]
fn the_disclosure_scan_can_still_see_both_directions() {
    let right = char::from_u32(0x25B8).unwrap();
    let down = char::from_u32(0x25BE).unwrap();
    let offending = format!("<button>{right} Filters</button><button>State {down}</button>");
    assert_eq!(
        raw_disclosure_triangles(&offending),
        vec![(right, 0x25B8), (down, 0x25BE)]
    );
    assert!(raw_disclosure_triangles("<button><svg></svg> Filters</button>").is_empty());
}

#[test]
fn the_scan_finds_no_offenders_in_the_real_tree() {
    // Belt-and-braces over the two #[test] fences above: proves the corpus
    // floor in `every_btn_icon_span_holds_a_shape` was not met by luck --
    // the real file's spans and pictographs both come back clean, not just
    // the assembled fixtures.
    let source = dashboard();
    assert!(unqualified_pictographs(&source).is_empty());
    assert!(raw_disclosure_triangles(&source).is_empty());
    for span in btn_icon_span_contents(&source) {
        assert!(span.contains("<svg"));
    }
}
