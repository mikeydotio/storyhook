//! Fences the create modal's requests to the project it names, not the
//! project the board happens to have open (SH-439).
//!
//! `openCreateModal()` builds the modal from the *open* board's
//! `apiBase()` = `repoApiBase(state.repoId)`, and every mutating action in
//! the CREATE MODAL section used to read that directly. SH-439 gives the
//! modal its own `#create-project` dropdown, so a story can be filed into a
//! project other than the one on screen — which means every request that
//! section makes has to address `createTargetProject` (via
//! `createApiBase()`), captured once per action, rather than `apiBase()`.
//! A single missed call site would silently file into the board's project
//! regardless of what the dropdown says, which is worse than the pre-SH-439
//! behavior: the dropdown would *claim* one project while the request
//! addressed another, with nothing on screen disagreeing.
//!
//! This is the derived-corpus style `tests/dashboard_deadline_knobs.rs`,
//! `tests/dashboard_mutation_deadline.rs` and `tests/dead_public_surface.rs`
//! already use: read the real file, scan a bounded section, and fail on a
//! pattern rather than trust the fix. Two failure modes are checked, because
//! either alone misses a real regression:
//!
//! * `apiBase()` appearing anywhere in the section — the un-captured,
//!   board-scoped base finding its way back in.
//! * an `api("` call site whose own line does not also name the captured
//!   base variable — catches a hand-rolled path string
//!   (`"/api/repos/" + slug + "/story"`) that bypasses the derivation
//!   entirely without ever writing the literal `apiBase()`.
//!
//! A corpus floor (`>= 5` call sites: the four mutating actions plus
//! `applyLabelDiff`'s add/remove pair) means a section that stopped making
//! requests can't report a clean tree by accident, and a positive control
//! (the four function names) means "section not found" can't either.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn dashboard() -> String {
    let path = repo_root().join("src/web_dashboard.html");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
}

/// `source` with `//` line comments and `/* … */` blocks blanked out,
/// preserving length and line structure. The identical crude, string-
/// unaware stripper `tests/dashboard_enter_submit_guard.rs` already uses for
/// this file's own embedded script (duplicated here per that file's own
/// precedent of one self-contained copy per derived-fence test, rather than
/// a shared helper with no crate to live in — each `tests/*.rs` file
/// compiles as its own binary). Without it, `createApiBase()`'s own doc
/// comment — which names `apiBase()` in prose, to explain what it replaces —
/// would trip the very check this file exists to run on live code.
fn strip_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0;
    let mut in_block = false;
    let mut in_line = false;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied().unwrap_or('\0');
        if c == '\n' {
            in_line = false;
            out.push('\n');
            i += 1;
            continue;
        }
        if in_block {
            if c == '*' && next == '/' {
                in_block = false;
                out.push(' ');
                out.push(' ');
                i += 2;
                continue;
            }
            out.push(' ');
            i += 1;
            continue;
        }
        if in_line {
            out.push(' ');
            i += 1;
            continue;
        }
        if c == '/' && next == '*' {
            in_block = true;
            out.push(' ');
            out.push(' ');
            i += 2;
            continue;
        }
        if c == '/' && next == '/' {
            in_line = true;
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

const SECTION_START: &str = "// CREATE MODAL (also the Edit Draft modal";
const SECTION_END: &str = "// DRAFTS POPOVER (SH-175)";

/// The CREATE MODAL section's *code* — comments stripped, everything between
/// its own banner comment and the next one (DRAFTS POPOVER) — or `None` if
/// either marker has moved or been renamed, so a caller can refuse to scan
/// an empty stand-in for it rather than silently reporting a clean tree over
/// nothing. The markers themselves are matched against the raw source (a
/// comment naming itself would otherwise blank its own boundary), then the
/// span between them is what gets stripped and returned.
fn create_modal_section(source: &str) -> Option<String> {
    let start = source.find(SECTION_START)?;
    let rest = &source[start..];
    let end = rest.find(SECTION_END)?;
    Some(strip_comments(&rest[..end]))
}

/// The name every mutating action in the section captures its project base
/// into (`var base = createApiBase();`), read back to confirm each request
/// names it rather than assuming the identifier a fix happened to choose.
const CAPTURED_BASE: &str = "base";

/// Every line inside `section` that opens an `api(` call.
fn api_call_lines(section: &str) -> Vec<&str> {
    section
        .lines()
        .filter(|line| line.contains("api(\""))
        .collect()
}

#[test]
fn the_scan_still_sees_the_sections_own_functions() {
    let source = dashboard();
    let section = create_modal_section(&source).unwrap_or_else(|| {
        panic!(
            "could not find the CREATE MODAL section (from {SECTION_START:?} to \
             {SECTION_END:?}) in src/web_dashboard.html -- one of the two banner \
             comments moved or was reworded, and every assertion below would \
             otherwise report a clean tree over nothing"
        )
    });
    for needle in [
        "function submitCreate",
        "function saveDraft",
        "function discardDraft",
        "function applyLabelDiff",
    ] {
        assert!(
            section.contains(needle),
            "the CREATE MODAL section no longer contains `{needle}` -- either it \
             was renamed/moved, in which case this fence's section markers or this \
             positive control need updating, or it was deleted, in which case this \
             file's remaining assertions may be silently checking nothing"
        );
    }
}

#[test]
fn the_scan_can_still_see_a_bare_api_base_call() {
    // Assembled at runtime, the same idiom `dashboard_deadline_knobs.rs` uses
    // for its own bare-literal positive control, so the real scan below can
    // never trip on this file's own source rather than the dashboard's.
    let offender = format!("{}(){}", "apiBase", "");
    assert!(offender.contains("apiBase()"));
}

#[test]
fn the_create_modal_section_never_calls_the_board_scoped_api_base() {
    let source = dashboard();
    let section = create_modal_section(&source)
        .expect("the CREATE MODAL section markers must be present -- see the positive control");
    assert!(
        !section.contains("apiBase()"),
        "the CREATE MODAL section calls the board-scoped `apiBase()` -- every \
         mutating action in this section must address `createTargetProject` via \
         `createApiBase()` (captured once per action into a local), never the open \
         board's project, or SH-439's project dropdown can name one project while \
         the request silently addresses another"
    );
}

#[test]
fn every_api_call_in_the_create_modal_section_names_the_captured_base() {
    let source = dashboard();
    let section = create_modal_section(&source)
        .expect("the CREATE MODAL section markers must be present -- see the positive control");
    let call_lines = api_call_lines(&section);
    assert!(
        call_lines.len() >= 5,
        "expected at least 5 `api(\"...\")` call sites in the CREATE MODAL section \
         (the four mutating actions, plus applyLabelDiff's add/remove pair), found \
         {}: {call_lines:?}. Either a call site was removed, or this scan's line-based \
         `api(\"` match no longer matches this file's shape.",
        call_lines.len()
    );
    let unscoped: Vec<&str> = call_lines
        .iter()
        .filter(|line| !line.contains(CAPTURED_BASE))
        .copied()
        .collect();
    assert!(
        unscoped.is_empty(),
        "these `api(\"...\")` call sites in the CREATE MODAL section do not name the \
         captured `{CAPTURED_BASE}` variable on their own line: {unscoped:?}. A call \
         site that hand-rolls its own path string (e.g. `\"/api/repos/\" + slug + \
         \"/story\"`) bypasses `createApiBase()`'s single point of truth for which \
         project the modal addresses, exactly like a bare `apiBase()` would, without \
         ever writing that literal."
    );
}

#[test]
fn discard_draft_uses_the_permanent_delete_contract() {
    let source = dashboard();
    let section = create_modal_section(&source)
        .expect("the CREATE MODAL section markers must be present -- see the positive control");
    assert!(
        section.contains(
            r#"api("DELETE", base + "/story/" + encodeURIComponent(id), { force: true })"#,
        ),
        "Discard Draft must use the forced second half of the story-delete API; an old \
         reason-shaped request now returns the preview plan and leaves the draft intact"
    );
}
