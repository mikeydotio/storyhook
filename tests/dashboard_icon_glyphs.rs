//! Fences the dashboard's semantic emoji vocabulary (SH-620).
//!
//! The dashboard deliberately uses emoji for visual character instead of a
//! private set of hand-drawn SVG glyphs. The emoji identify the control's
//! purpose -- home, settings, saved writing, search, sorting, tools, and so
//! on -- rather than merely imitating the geometry of the removed artwork.
//! Structural state remains in `aria-expanded`, `aria-haspopup`, and
//! `data-direction`; every emoji is decorative, so a control's text or
//! `aria-label` remains its accessible name.
//!
//! This file verifies the source contract. Browser behavior and rendering are
//! covered by `e2e/specs/icon-shapes.spec.ts` and
//! `icon-shapes.mobile.spec.ts`.

use std::collections::BTreeMap;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn dashboard() -> String {
    let path = repo_root().join("src/web_dashboard.html");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
}

fn emoji_vocabulary(source: &str) -> BTreeMap<String, String> {
    let after_start = source
        .split_once("var UI_EMOJI = {")
        .map(|(_, rest)| rest)
        .expect("src/web_dashboard.html must define `var UI_EMOJI = {`");
    let body = after_start
        .split_once("\n  };")
        .map(|(body, _)| body)
        .expect("UI_EMOJI must close with the expected two-space-indented `};`");

    body.lines()
        .filter_map(|line| {
            let line = line.trim().trim_end_matches(',');
            if line.is_empty() {
                return None;
            }
            let (name, quoted) = line
                .split_once(':')
                .unwrap_or_else(|| panic!("UI_EMOJI entry has no colon: {line:?}"));
            let value = quoted.trim();
            assert!(
                value.starts_with('"') && value.ends_with('"'),
                "UI_EMOJI value must be a quoted literal: {line:?}"
            );
            Some((
                name.trim().to_string(),
                value[1..value.len() - 1].to_string(),
            ))
        })
        .collect()
}

fn expected_vocabulary() -> BTreeMap<String, String> {
    [
        ("actions", "🛠️"),
        ("back", "⬅️"),
        ("close", "✖️"),
        ("collapse", "➖"),
        ("drafts", "📝"),
        ("dropdown", "🔽"),
        ("expand", "➕"),
        ("filters", "🎛️"),
        ("home", "🏠"),
        ("search", "🔍"),
        ("settings", "⚙️"),
        ("sort", "↕️"),
        ("submenu", "➡️"),
        ("warning", "⚠️"),
    ]
    .into_iter()
    .map(|(name, emoji)| (name.to_string(), emoji.to_string()))
    .collect()
}

#[test]
fn dashboard_declares_the_complete_semantic_emoji_vocabulary() {
    assert_eq!(emoji_vocabulary(&dashboard()), expected_vocabulary());
}

#[test]
fn dashboard_contains_no_private_svg_glyph_system() {
    let source = dashboard();
    for forbidden in ["<svg", "SVG_NS", "svgIcon("] {
        assert!(
            !source.contains(forbidden),
            "src/web_dashboard.html still contains {forbidden:?}; SH-620 replaces the private SVG glyph system with semantic emoji"
        );
    }
}

#[test]
fn emoji_icons_are_decorative_and_semantically_identified() {
    let source = dashboard();
    assert!(
        source.contains("function emojiIcon(kind, opts)"),
        "dynamic controls must use the shared emojiIcon constructor"
    );
    assert!(
        source.contains("\"aria-hidden\": \"true\""),
        "emojiIcon must keep decorative emoji out of accessible names"
    );
    assert!(
        source.contains("emoji: kind"),
        "emojiIcon must expose its semantic kind in data-emoji for tests and inspection"
    );
}

#[test]
fn static_emoji_slots_are_decorative_and_named_by_purpose() {
    let source = dashboard();
    for kind in [
        "home", "settings", "drafts", "search", "dropdown", "filters", "close",
    ] {
        let marker = format!("data-emoji=\"{kind}\" aria-hidden=\"true\"");
        assert!(
            source.contains(&marker),
            "the static {kind:?} emoji must be decorative and carry its semantic data-emoji marker"
        );
    }
}

#[test]
fn the_vocabulary_parser_rejects_silent_drift() {
    let source = "var UI_EMOJI = {\n    home: \"🏠\",\n    settings: \"⚙️\"\n  };";
    assert_eq!(
        emoji_vocabulary(source),
        BTreeMap::from([
            ("home".to_string(), "🏠".to_string()),
            ("settings".to_string(), "⚙️".to_string()),
        ])
    );
    assert_ne!(emoji_vocabulary(source), expected_vocabulary());
}
