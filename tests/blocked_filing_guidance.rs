//! Every surface that teaches both filing a story and recording its blocker
//! teaches doing both in one write (SH-779).
//!
//! `story new --blocked-by` exists because filing a story and then relating it
//! is two commits, and the first one wakes the Full Auto engine, which claimed
//! MT-32 while it was still ready. The flag closes the gap only for a filer who
//! uses it — and the lane agent that filed MT-32 did exactly what every guide
//! then taught: `story new`, then `story relate … blocked-by …`. So the guides
//! are the detector this class lacked.
//!
//! **Derived, not enumerated,** as in `tests/priority_rubric.rs`: a surface is
//! in scope when its own text teaches filing ([`FILES`]) and teaches recording a
//! blocker ([`BLOCKS`]); it then owes the reader [`ATOMIC`]. Add a surface that
//! teaches both and this test asks about it with no edit here.

use std::collections::BTreeMap;
use std::path::Path;

use storyhook::help_topics::{compact_reference, get_help_topic, list_topics};
use storyhook::service::templates;

/// The flag every such surface must name.
const ATOMIC: &str = "--blocked-by";

/// Markers of a surface that teaches filing a story: the CLI verb, and the
/// plugin helper's own filing verb.
const FILES: [&str; 2] = ["story new", "\" create"];

/// A surface teaches recording a blocker when it shows a command that writes
/// one after the fact: `story relate <a> blocked-by|blocks <b>`, or any
/// `story block <id> …` — with `--on`, or with a bare reason, which is a hold
/// written after filing and so the same two-write race. Command-shaped rather
/// than the bare word, because prose about edges — the priority rubric's
/// "if X is blocked-by Y" — teaches no filer to write one.
fn teaches_a_blocker(text: &str) -> bool {
    let command =
        regex::Regex::new(r"relate\s+\S+\s+(?:blocked-by|blocks)\s|story block\s+\S|--on\s+\S")
            .expect("a valid pattern");
    command.is_match(text)
}

fn in_scope(text: &str) -> bool {
    FILES.iter().any(|marker| text.contains(marker)) && teaches_a_blocker(text)
}

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Every tracked file matching `pattern`, keyed by its repository-relative path.
fn tracked(pattern: &str) -> BTreeMap<String, String> {
    let listed = std::process::Command::new("git")
        .current_dir(repo_root())
        .args(["ls-files", "-z", "--", pattern])
        .output()
        .expect("listing this repository's tracked files");
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
            let text = std::fs::read_to_string(repo_root().join(&relative))
                .unwrap_or_else(|e| panic!("reading {relative}: {e}"));
            (relative, text)
        })
        .collect()
}

/// Every shipped surface an agent or person reads before filing: each help
/// topic, the compact session-start reference, the scaffolded instruction
/// files, and the plugin's skills and references.
fn surfaces() -> BTreeMap<String, String> {
    let mut all = BTreeMap::new();
    for topic in list_topics() {
        let body = get_help_topic(topic).expect("a listed topic must resolve");
        all.insert(format!("story help {topic}"), body.to_string());
    }
    all.insert(
        "story help --compact".to_string(),
        compact_reference().to_string(),
    );
    all.insert(
        "scaffolded AGENTS.md".to_string(),
        templates::agents_md("SH", "done"),
    );
    all.insert(
        "scaffolded .cursorrules".to_string(),
        templates::cursor_rules(),
    );
    all.insert("scaffolded CLAUDE.md".to_string(), templates::claude_md());
    all.extend(tracked("plugins/story/references/*.md"));
    all.extend(tracked("plugins/story/skills/*/SKILL.md"));
    all
}

#[test]
fn the_scope_markers_can_see_a_two_write_filing() {
    // Positive control: the sequence MT-32's filer was taught is in scope, so
    // a guide that teaches it without the flag cannot slip past as unscanned.
    let two_writes = "story new \"X\"\nstory relate SH-2 blocked-by SH-1";
    assert!(in_scope(two_writes));
    assert!(!two_writes.contains(ATOMIC));
    assert!(
        !in_scope("story show SH-1"),
        "a surface that files nothing is out of scope"
    );
    assert!(
        in_scope("story new \"X\"\nstory block SH-2 --on SH-1 \"why\""),
        "the `block --on` spelling of the second write is in scope too"
    );
    assert!(
        in_scope("story new \"X\"\nstory block <id> \"reason\""),
        "a prose hold written after filing is the same race"
    );
    assert!(
        !in_scope("story new --priority <level>\nif X is blocked-by Y, raise Y"),
        "prose about an edge teaches no filer to write one"
    );
}

#[test]
fn the_known_filing_guides_are_in_scope() {
    // Guards the derivation against silently scanning nothing: the surfaces
    // SH-779 changed must each be picked up by the markers themselves.
    let all = surfaces();
    for name in [
        "story help new",
        "story help block",
        "story help agent-guide",
        "scaffolded AGENTS.md",
        "scaffolded .cursorrules",
    ] {
        let text = all
            .get(name)
            .unwrap_or_else(|| panic!("`{name}` is not among the scanned surfaces"));
        assert!(
            in_scope(text),
            "`{name}` no longer reads as a filing guide with blockers"
        );
    }
}

#[test]
fn every_guide_that_files_and_blocks_teaches_the_atomic_form() {
    let silent: Vec<String> = surfaces()
        .into_iter()
        .filter(|(_, text)| in_scope(text) && !text.contains(ATOMIC))
        .map(|(name, _)| name)
        .collect();
    assert!(
        silent.is_empty(),
        "these surfaces teach filing a story and recording a blocker without `{ATOMIC}`, \
         so a reader files the story ready and relates it afterwards — two writes a Full \
         Auto run can claim between (SH-779): {silent:?}"
    );
}

#[test]
fn the_plugin_filing_flow_files_a_blocker_with_the_story() {
    // `/story new`'s own reference shows no after-the-fact blocker command at
    // all, so the derived scan above does not reach it — which is the point.
    // What it must do instead is carry the blocker into the one create call.
    let reference = tracked("plugins/story/references/story-new.md")
        .remove("plugins/story/references/story-new.md")
        .expect("the plugin's filing reference must exist");
    assert!(
        reference.contains("[--blocked-by <id> ...]"),
        "the filing command must offer --blocked-by"
    );
    assert!(
        reference.contains("A blocker is never a follow-up"),
        "the reference must not defer a blocker to a relate after filing"
    );
    assert!(
        !teaches_a_blocker(&reference),
        "the reference must not show a blocker written after filing"
    );
}
