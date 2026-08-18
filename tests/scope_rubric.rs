//! Every surface that could file a story instead says how to decide whether to (SH-402).
//!
//! No shipped surface said anything about what to do with a problem found mid-work
//! — not the dispatch charters, not the scaffolded `AGENTS.md`/`.cursorrules`, not
//! any of the 53 help topics that existed before this one. The doctrine that
//! actually governed it ("defects become stories before they become fixes"; "sweep
//! for siblings, file issues for those"; "two hats") lived only in the operator's
//! own global config, outside this repository, and every rule in it pushed toward
//! filing — which is a large part of why this project's own backlog grew faster
//! than it closed.
//!
//! This is the SH-354 shape one rule over: nothing that decides whether to FILE a
//! story could read a rule about when to. The repair is the same one SH-354 used —
//! promote the generic decision into the binary as a shipped help topic
//! (`scope-rubric`) and have every surface that could file instead point at it,
//! rather than mirror a paraphrase into five places and hope the copies stay
//! honest. This project's own precedent and its numeric calibration stay in
//! `CLAUDE.md`, exactly where `priority-rubric`'s did.
//!
//! **`*-rubric` is now a family, not a one-off.** `priority-rubric` and
//! `scope-rubric` share two invariants — self-naming, and no project-private
//! precedent — and this file checks both as a derived scan over every topic whose
//! key ends in `-rubric`, so a third doctrine topic inherits the fence for free
//! rather than needing its own hand-copied test.
//!
//! Some checks here are deliberately NOT derived over a broad corpus. A blanket
//! scan for `"story new"` across every help topic or every plugin document was
//! tried first and rejected: `story new` appears in cross-reference rows on
//! topics that never offer the filing decision at all (`decompose`,
//! `json-format`, `priority-rubric`'s own `Related:` block), and in plugin index
//! pages (`README.md`, `skills/story/SKILL.md`'s frontmatter) that merely describe
//! what the command does. A marker precise enough to exclude those false positives
//! matches only the `new` topic itself, which makes the derived form no more
//! informative than naming the handful of doors this doctrine actually governs
//! directly — `new`, `delete`, the scaffolded templates, the two plugin docs this
//! story edits, and the autonomous charter. Each is checked by name, the same way
//! `tests/priority_rubric.rs` checks `the_priority_alias_still_redirects_to_
//! prioritize` by name rather than by scan.

use std::path::Path;

use storyhook::help_topics::{get_help_topic, list_topics};
use storyhook::service::templates;

/// What a surface must name to have discharged its duty.
const POINTER: &str = "story help scope-rubric";

/// The topic key the pointer resolves to.
const TOPIC: &str = "scope-rubric";

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn the_scope_topic_exists_and_names_itself() {
    let body = get_help_topic(TOPIC).expect("`story help scope-rubric` must be a topic");
    assert!(
        body.starts_with(POINTER),
        "the topic's first line must be its own invocation, matching every other topic \
         and every other member of the -rubric family"
    );
}

/// Every story id in `text`, by shape: two or more capitals, a hyphen, digits.
///
/// Copied from `tests/priority_rubric.rs::story_ids_in` rather than shared — this
/// repo already accepts that duplication between independent test binaries, and a
/// shared helper crate for two call sites would be more machinery than the
/// duplication it removes.
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
    // The positive control — see `tests/priority_rubric.rs`'s own copy of this test
    // for why a scan that could pass vacuously needs one.
    assert_eq!(
        story_ids_in("filed at none, the case study is SH-283, dead last of 26"),
        vec!["SH-283".to_string()],
        "the scan must find a story id in prose"
    );
    assert!(
        story_ids_in("a defect already on rungs 1-3, priced as a class").is_empty(),
        "a numeric range is not a story id"
    );
    assert!(
        story_ids_in("blocked-by transmits nothing, and next-5 is not an id").is_empty(),
        "a lowercase word before the hyphen is not a story id"
    );
}

/// This project's own machinery, meaningless in a stranger's repository.
const PROJECT_PRIVATE: [&str; 4] = ["make test", ".githooks", "scripts/", "HARDENING_PROGRESS"];

/// Which entries of [`PROJECT_PRIVATE`] appear in `body`, if any.
fn project_private_hits(body: &str) -> Vec<&'static str> {
    PROJECT_PRIVATE
        .into_iter()
        .filter(|private| body.contains(private))
        .collect()
}

#[test]
fn every_rubric_topic_self_names_and_ships_no_project_private_precedent() {
    // Derived, not enumerated: any topic key ending in `-rubric` joins this scan
    // with no edit here. Vacuity-guarded on family size and membership, so a scan
    // that silently stopped seeing topics reads as a failure rather than a clean
    // family of size zero.
    let family: Vec<&str> = list_topics()
        .into_iter()
        .filter(|topic| topic.ends_with("-rubric"))
        .collect();
    assert!(
        family.len() >= 2,
        "the -rubric family scan found only {family:?} — expected at least \
         priority-rubric and scope-rubric; either a topic was renamed or this scan \
         has stopped seeing topics"
    );
    for expected in ["priority-rubric", "scope-rubric"] {
        assert!(
            family.contains(&expected),
            "the -rubric family scan did not find `{expected}`: {family:?}"
        );
    }

    for topic in family {
        let body = get_help_topic(topic).expect("a listed topic must resolve");
        let self_pointer = format!("story help {topic}");
        assert!(
            body.starts_with(&self_pointer),
            "`{topic}`'s first line must be its own invocation"
        );

        let ids = story_ids_in(body);
        assert!(
            ids.is_empty(),
            "`{topic}` names story ids from this project's own tracker: {ids:?} — they \
             belong in CLAUDE.md, not in every user's binary"
        );
        let private = project_private_hits(body);
        assert!(
            private.is_empty(),
            "`{topic}` names {private:?}, this project's own machinery, meaningless in \
             the repository it will be read in"
        );
    }
}

#[test]
fn the_new_topic_points_at_scope_rubric() {
    // `new` is the door this doctrine most directly governs: the moment a story is
    // about to be filed. Checked by name rather than by a derived scan — see the
    // module doc for why a marker broad enough to find `new` on its own also
    // catches topics that only cross-reference `story new` in passing.
    let body = get_help_topic("new").expect("the `new` topic must exist");
    assert!(
        body.contains(POINTER),
        "`story help new` must point at {POINTER} — the moment before a story is \
         filed is where the adopt-or-file decision belongs"
    );
}

#[test]
fn the_delete_topic_points_at_scope_rubric() {
    // `delete` is the collapse door — `duplicate-of` + `story delete` is how an
    // existing duplicate is retired, which is scope-rubric's own recommended
    // remedy for "stories are multiplying faster than they're being closed".
    let body = get_help_topic("delete").expect("the `delete` topic must exist");
    assert!(
        body.contains(POINTER),
        "`story help delete` must point at {POINTER} — it is the collapse door \
         scope-rubric names as the remedy for a duplicate"
    );
}

#[test]
fn every_scaffolded_instruction_file_points_at_the_rubric() {
    // These go into *other people's* repositories — same reasoning as
    // `tests/priority_rubric.rs`'s own copy of this test, and the same two
    // functions it checks.
    for (name, text) in [
        ("AGENTS.md", templates::agents_md("SH", "done")),
        (".cursorrules", templates::cursor_rules()),
    ] {
        assert!(
            text.contains(POINTER),
            "scaffolded {name} tells an agent how to work a story but not what to do \
             with a problem it finds along the way; it must point at {POINTER}"
        );
    }
}

/// `AUTO_SCOPE_CLAUSE`'s literal text, extracted from the checked-in script the
/// same way `src/api/dispatch.rs::the_shipped_default_templates_are_charter_inert`
/// extracts every other charter constant — one `VAR="…"` assignment on its own
/// line.
fn scope_clause() -> &'static str {
    let script = include_str!("../plugin/claude-code/bin/story.sh");
    let prefix = "AUTO_SCOPE_CLAUSE=\"";
    let line = script
        .lines()
        .find(|l| l.starts_with(prefix))
        .expect("story.sh must define AUTO_SCOPE_CLAUSE on its own line");
    line.strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix('"'))
        .expect("AUTO_SCOPE_CLAUSE's literal-assignment shape changed -- update this extraction")
}

#[test]
fn the_autonomous_charter_carries_the_adoption_clause() {
    let clause = scope_clause();
    for needle in [
        "prefer adopting it into",
        "context window is still unused",
        "leave <n> open",
        "treat it as spent",
    ] {
        assert!(
            clause.contains(needle),
            "the autonomous charter's scope clause no longer states [{needle}] -- \
             inertness must not be bought by removing the instructions"
        );
    }

    // Defined but never wired into either composition would pass every check
    // above while reaching no dispatched session at all.
    let script = include_str!("../plugin/claude-code/bin/story.sh");
    for var in ["AUTO_PROMPT_TPL=", "AUTO_PROMPT_SOLO_TPL="] {
        let composition = script
            .lines()
            .find(|l| l.starts_with(var))
            .unwrap_or_else(|| panic!("story.sh must still define {var} on its own line"));
        assert!(
            composition.contains("$AUTO_SCOPE_CLAUSE"),
            "{var} no longer references $AUTO_SCOPE_CLAUSE"
        );
    }
}

#[test]
fn the_charter_scope_clause_names_no_raw_token_count() {
    // This project's own rule: a ceiling derives from the deadline it
    // disproves, never a bare literal (CLAUDE.md's timing-assertions rule,
    // applied here to a context budget rather than a wall clock). A raw
    // "500000"/"500k" would state an opinion about how big THIS operator's
    // context window is -- not a fact about every install this charter ships
    // to, and vacuously true on a smaller one. That calibration belongs in
    // CLAUDE.md, next to the window it derives from, not in the shipped
    // charter. The clause has no legitimate reason to name any digit at all:
    // "half" and "still unused" carry the condition without picking a number.
    let clause = scope_clause();
    assert!(
        !clause.chars().any(|c| c.is_ascii_digit()),
        "the charter's scope clause names a raw number; the context condition must be \
         expressed as a fraction of the window, never a token count -- see CLAUDE.md \
         for where this project's own calibration belongs"
    );
}

#[test]
fn story_triage_names_a_collapse_resolution() {
    let text = std::fs::read_to_string(
        repo_root().join("plugin/claude-code/skills/story-triage/SKILL.md"),
    )
    .expect("reading story-triage/SKILL.md");
    for needle in ["duplicate-of", "obviates", POINTER] {
        assert!(
            text.contains(needle),
            "story-triage/SKILL.md's resolution list is missing [{needle}] -- \
             'prefer collapsing and combining stories' needs a concrete resolution, \
             not just the relationship vocabulary"
        );
    }
}

#[test]
fn story_new_reference_searches_before_filing() {
    let text =
        std::fs::read_to_string(repo_root().join("plugin/claude-code/references/story-new.md"))
            .expect("reading references/story-new.md");
    assert!(
        text.contains("story search"),
        "references/story-new.md must search the existing backlog before drafting a \
         new story"
    );
    assert!(
        text.contains(POINTER),
        "references/story-new.md must point at {POINTER} when an existing story \
         already covers the request"
    );
    assert!(
        text.contains("Never file without an explicit"),
        "the search-first step must not weaken the human-confirmed nature of this \
         flow -- the user's explicit go-ahead still wins"
    );
}

#[test]
fn claude_md_points_at_the_topic_and_does_not_restate_it() {
    let text = std::fs::read_to_string(repo_root().join("CLAUDE.md")).expect("reading CLAUDE.md");
    assert!(
        text.contains(POINTER),
        "CLAUDE.md must send a reader to the shipped topic rather than being the only \
         place this doctrine exists"
    );

    // The heading the shipped topic uses for its own remedy list. If it appears in
    // CLAUDE.md verbatim, the doctrine has been restated rather than pointed at —
    // the SH-136 class this repo has paid for before.
    assert!(
        !text.contains("== What still gets filed =="),
        "CLAUDE.md has restated the shipped topic's own section; the topic is the \
         source now, and a second copy is the SH-136 class all over again"
    );
}

#[test]
fn this_repos_agents_md_is_what_the_template_generates() {
    // The root AGENTS.md used to be a pre-SH-354 rendering — no Planning
    // section, no priority-rubric pointer, no relationship table — which
    // meant nothing added to templates::agents_md, this story's own
    // scope-rubric pointer included, ever reached this repository's own
    // agents until it was regenerated. Byte-equal against the same
    // "SH"/"done" pair tests/priority_rubric.rs already hardcodes for this
    // project (this project's own prefix and closed state), so a future
    // template edit that forgets to regenerate this file fails here rather
    // than drifting silently again.
    let on_disk =
        std::fs::read_to_string(repo_root().join("AGENTS.md")).expect("reading AGENTS.md");
    assert_eq!(
        on_disk,
        templates::agents_md("SH", "done"),
        "AGENTS.md no longer matches templates::agents_md(\"SH\", \"done\") — regenerate \
         it with `story scaffold agents-md` rather than hand-editing"
    );
}
