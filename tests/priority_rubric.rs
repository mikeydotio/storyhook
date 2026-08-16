//! Every surface that offers a priority level says what the levels mean (SH-354).
//!
//! A priority is not a label. It is `story next`'s sort key — `domain::ready_order`
//! orders by level, then by story number ascending — so choosing one is a scheduling
//! decision about what the next session picks up. Until this story, no path that
//! *assigns* a level said what one meant: `story new --priority`, `story set
//! --priority`, `story prioritize`, `story list --priority`, the scaffolded
//! `AGENTS.md` and `.cursorrules`, and the plugin's own filing and triage
//! instructions all took the word and none defined it. The rubric governed a human
//! reading one project's `CLAUDE.md` and nothing else, which is to say it governed
//! no automated decision at all.
//!
//! The repair was to **promote** the rubric's generic half into the binary as the
//! `priority-rubric` help topic and have every other surface point at it, rather
//! than mirror a condensed copy into five files and try to keep the copies honest.
//! A drift test over a paraphrase can only prove two texts differ, never that
//! either is right; the only mirror that would truly detect drift is a
//! byte-verbatim block, which is a single source wearing a disguise. Decided
//! unanimously by a three-seat council, whose verdict SH-354 carries.
//!
//! **Derived, not enumerated.** The in-scope set is computed from the shipped text
//! itself: anything naming `--priority` or `prioritiz` is offering the operator a
//! choice among levels and therefore owes them the criteria. A hand-maintained list
//! of such sites is exactly what let SH-136 drift three times and SH-198 accumulate
//! ten dead items, so there is no list here to fall out of date — add a surface that
//! mentions the flag and this test starts asking about it with no edit.
//!
//! The one apparent exception is not one: the rubric topic itself contains
//! `--priority`, and satisfies the rule because its first line *is*
//! `story help priority-rubric`. The source pointing at itself costs nothing and
//! spares this scan a carve-out, which is the thing carve-outs are for.

use std::collections::BTreeMap;
use std::path::Path;

use storyhook::domain::Priority;
use storyhook::help_topics::{compact_reference, get_help_topic, list_topics};
use storyhook::service::templates;

/// What a surface must name to have discharged its duty.
const POINTER: &str = "story help priority-rubric";

/// The topic key the pointer resolves to.
const TOPIC: &str = "priority-rubric";

/// The markers that put a surface in scope.
///
/// `--priority` is the flag every setting path spells; `prioritiz` catches the
/// verb (`story prioritize`, "reprioritize", "prioritization") without also
/// catching the bare noun "priority", which appears in prose that offers no
/// choice at all ("the highest-priority ready story").
const MARKERS: [&str; 2] = ["--priority", "prioritiz"];

fn offers_a_level(text: &str) -> bool {
    MARKERS.iter().any(|marker| text.contains(marker))
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

#[test]
fn the_rubric_topic_exists_and_names_itself() {
    let body = get_help_topic(TOPIC).expect("`story help priority-rubric` must be a topic");
    assert!(
        body.starts_with(POINTER),
        "the topic's first line must be its own invocation, both because that is how \
         every other topic opens and because it is what keeps this file's scan free of \
         a self-reference carve-out"
    );
}

#[test]
fn every_help_topic_that_offers_a_level_points_at_the_rubric() {
    let mut silent = Vec::new();
    for topic in list_topics() {
        let body = get_help_topic(topic).expect("a listed topic must resolve");
        if offers_a_level(body) && !body.contains(POINTER) {
            silent.push(topic);
        }
    }
    assert!(
        silent.is_empty(),
        "these help topics let the operator choose a priority level without saying what \
         one means — each needs a `{POINTER}` pointer: {silent:?}"
    );
}

#[test]
fn the_compact_reference_points_at_the_rubric() {
    // The session-start context an agent reads before it has run anything, and
    // so the first place a level gets chosen from.
    //
    // It used to spell the five levels inline — `critical|high|medium|low|none`
    // — and now names the rubric instead. That was forced rather than chosen:
    // `compact_reference_under_3000_chars` is a real byte budget with eleven
    // bytes spare, because this text is loaded into every session's context, and
    // the pointer only fits in the space the vocabulary was using. It is also
    // the better half to keep. A list of five words tells an agent what parses;
    // it does not tell it which one is true, which is the whole of SH-354.
    let text = compact_reference();
    assert!(
        offers_a_level(text),
        "guard assumption: the compact reference is expected to name the priority verb"
    );
    assert!(
        text.contains(POINTER),
        "the compact reference lists the levels; it must say where their meanings are"
    );
}

#[test]
fn every_scaffolded_instruction_file_points_at_the_rubric() {
    // These go into *other people's* repositories. Whatever they say is
    // storyhook's opinion, not this project's — which is the reason they carry a
    // pointer rather than the rubric's text.
    for (name, text) in [
        ("AGENTS.md", templates::agents_md("SH", "done")),
        (".cursorrules", templates::cursor_rules()),
    ] {
        assert!(
            offers_a_level(&text),
            "guard assumption: scaffolded {name} is expected to name the priority verb"
        );
        assert!(
            text.contains(POINTER),
            "scaffolded {name} tells an agent to set priorities; it must say where the \
             criteria are"
        );
    }
}

#[test]
fn every_plugin_document_that_offers_a_level_points_at_the_rubric() {
    let silent: Vec<String> = tracked("plugin/claude-code/**/*.md")
        .into_iter()
        .filter(|(_, text)| offers_a_level(text) && !text.contains(POINTER))
        .map(|(path, _)| path)
        .collect();
    assert!(
        silent.is_empty(),
        "these plugin documents file or re-level stories without a rubric to consult — \
         each needs a `{POINTER}` pointer: {silent:?}"
    );
}

#[test]
fn the_rubric_topic_names_every_level_the_domain_has() {
    let body = get_help_topic(TOPIC).expect("the topic must exist");
    // Iterated over the enum rather than a literal list, so a sixth level cannot
    // ship undescribed: adding a variant fails this test until the topic explains
    // it. `Priority` has no `IntoEnumIterator`, so the arms are spelled here —
    // exhaustively, which the compiler checks in `every_level_is_covered_by_the
    // _levels_under_test` below.
    for level in LEVELS {
        assert!(
            body.contains(level.as_str()),
            "`story help priority-rubric` must say what `{}` means",
            level.as_str()
        );
    }
}

/// Every level, for the topic scan above.
const LEVELS: [Priority; 5] = [
    Priority::Critical,
    Priority::High,
    Priority::Medium,
    Priority::Low,
    Priority::None,
];

#[test]
fn every_level_is_covered_by_the_levels_under_test() {
    // The compiler's half of the check above: a new `Priority` variant makes this
    // match non-exhaustive and the build fails, which is what stops `LEVELS` from
    // silently describing four of five levels.
    for level in LEVELS {
        match level {
            Priority::Critical
            | Priority::High
            | Priority::Medium
            | Priority::Low
            | Priority::None => {}
        }
    }
    assert_eq!(
        LEVELS.len(),
        5,
        "a level was added to the domain without being added here"
    );
}

/// Every story id in `text`, by shape: two or more capitals, a hyphen, digits.
///
/// Shape rather than the literal `SH` prefix, so it keeps working if this
/// project ever renames its own — the thing being fenced is "a story id from
/// *somebody's* tracker", and the shipped rubric should carry none.
///
/// **Deliberately biased towards a false alarm.** `UTF-8` and `ISO-8601` match
/// this shape and are not story ids. That is the safe direction here: a false
/// alarm fails loudly, in one place, and whoever wrote the line adjusts it,
/// whereas a false negative means a precedent shipped into strangers'
/// repositories and nobody found out. Other scans in this repo document the
/// opposite bias (`tests/dead_public_surface.rs` fails silent by design); the
/// difference is that this one guards an outbound promise rather than an
/// inventory.
fn story_ids_in(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut found = Vec::new();
    for (index, window) in chars.windows(2).enumerate() {
        if window[0] != '-' || !window[1].is_ascii_digit() {
            continue;
        }
        let prefix: String = chars[..index]
            .iter()
            .rev()
            .take_while(|c| c.is_ascii_uppercase())
            .collect();
        if prefix.chars().count() < 2 {
            continue;
        }
        let digits: String = chars[index + 1..]
            .iter()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        found.push(format!(
            "{}-{digits}",
            prefix.chars().rev().collect::<String>()
        ));
    }
    found
}

#[test]
fn the_story_id_scan_would_notice_one() {
    // The positive control. Without it, `..._ships_no_project_private_precedent`
    // is a test that passes for two possible reasons — the rubric is clean, or
    // the scan cannot see anything — and those are not the same result. This
    // repo prices a vacuous pass at the severity of the claim it fails to
    // deliver, plus one level; `tests/readme_command_reference.rs`'s
    // `the_readme_scan_would_notice_a_command_that_does_not_exist` is the same
    // move for the same reason.
    assert_eq!(
        story_ids_in("filed at none, the case study is SH-283, dead last of 26"),
        vec!["SH-283".to_string()],
        "the scan must find a story id in prose"
    );
    assert_eq!(
        story_ids_in("SH-136, SH-258 and MY-APP-9 all cost something"),
        vec![
            "SH-136".to_string(),
            "SH-258".to_string(),
            "APP-9".to_string()
        ],
        "several ids, including a multi-segment prefix, must all be found"
    );

    // And the shapes that are not ids: a lowercase word before the hyphen, and
    // a bare numeric range.
    assert!(
        story_ids_in("a defect already on rungs 1-3, priced as a class").is_empty(),
        "a numeric range is not a story id"
    );
    assert!(
        story_ids_in("blocked-by transmits nothing, and next-5 is not an id").is_empty(),
        "a lowercase word before the hyphen is not a story id"
    );
}

#[test]
fn the_rubric_topic_ships_no_project_private_precedent() {
    let body = get_help_topic(TOPIC).expect("the topic must exist");

    // The rubric was written over this backlog and its prose is full of its own
    // case studies; shipping those in the binary would paste a stranger's
    // history into a third party's `AGENTS.md` as storyhook's opinion. They stay
    // in CLAUDE.md, where they are evidence rather than doctrine.
    let found = story_ids_in(body);
    assert!(
        found.is_empty(),
        "the shipped rubric names story ids from this project's own tracker: {found:?} — \
         they belong in CLAUDE.md, not in every user's binary"
    );

    // This project's own machinery, for the same reason. A reader of the shipped
    // topic has no `make test`, no `.githooks/pre-push` and no `scripts/`.
    for private in ["make test", ".githooks", "scripts/", "HARDENING_PROGRESS"] {
        assert!(
            !body.contains(private),
            "the shipped rubric names `{private}`, which is this project's own \
             machinery and means nothing in the repository it will be read in"
        );
    }
}

#[test]
fn claude_md_points_at_the_topic_and_does_not_restate_the_level_table() {
    let text = std::fs::read_to_string(repo_root().join("CLAUDE.md")).expect("reading CLAUDE.md");
    assert!(
        text.contains(POINTER),
        "CLAUDE.md must send a reader to the shipped rubric rather than being the only \
         place it exists"
    );

    // The markdown table that used to hold the five criteria. Its header is the
    // signature of a restatement — if it comes back, there are two rubrics again
    // and the drift this story closed has reopened.
    assert!(
        !text.contains("| Level | The story must |"),
        "CLAUDE.md has restated the level table; the shipped topic is the source now, \
         and a second copy is the SH-136 class all over again"
    );
}

#[test]
fn the_priority_alias_still_redirects_to_prioritize() {
    // `priority` was already an alias for `prioritize` before this story
    // (`src/help_topics.rs`), which is why the new topic is `priority-rubric` and
    // not `priority`. Inserting over the alias would have silently retargeted
    // every existing `story help priority` a user has in their fingers.
    let alias = get_help_topic("priority").expect("`priority` must remain a topic");
    let prioritize = get_help_topic("prioritize").expect("`prioritize` must remain a topic");
    assert_eq!(
        alias, prioritize,
        "`story help priority` must still answer with the `prioritize` topic"
    );
    assert_ne!(
        alias,
        get_help_topic(TOPIC).expect("the rubric topic must exist"),
        "`priority` must not have been repurposed as the rubric topic"
    );
}
