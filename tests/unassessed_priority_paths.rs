//! Every door that can leave a story unassessed is named, not hand-listed
//! (SH-358).
//!
//! SH-358 was filed because SH-354 fixed exactly one creation path
//! (`story new`) and the story's own description hand-listed four more —
//! `decompose`, `import`, the dashboard, the MCP tool — as the fix's scope.
//! A hand-listed set is exactly what this project has paid for four times
//! before (SH-136, SH-258, SH-198, SH-260/276): the day a ninth door opens,
//! nothing notices unless the list is derived from the source rather than
//! remembered by a person. This file is that derivation, in the style of
//! `tests/type_slug_call_sites.rs` and `tests/priority_rubric.rs`.
//!
//! Two complementary checks, because no single one covers the class:
//!
//! 1. [`story_created_events_are_written_from_a_known_allowlist_of_files`] —
//!    every tracked source file that writes
//!    [`StoryEvent::StoryCreated`](storyhook::domain::StoryEvent::StoryCreated)
//!    must be named in [`ALLOWED`] with the reason it does not need its own
//!    unassessed-priority warning — because it delegates to a door that
//!    already checks (`service::story::creation_events`,
//!    `service::transfer::import_events`), or because it is not a live
//!    creation door at all (the legacy rollback path, a test fixture). This
//!    is what catches a *new module* writing the event directly.
//! 2. [`every_invocation_that_can_create_a_story_is_classified`] — a
//!    compile-time exhaustive `match` over every [`Invocation`] variant, with
//!    no wildcard arm, mirroring `mcp::tools::tool_for_variant`'s own
//!    anti-drift shape over the identical enum. A 65th variant is a compile
//!    error here until its author says whether it creates a story — which is
//!    what catches a *new command* built on an existing, already-allowed
//!    module (the actual shape SH-358 itself had: nothing new was written
//!    under `src/service/`, four callers of what already existed just never
//!    asked the question).

use std::path::Path;

use storyhook::cli::Invocation;

/// Every `.rs` file under `dir`, as (path, text). Same walk
/// `tests/type_slug_call_sites.rs::sources` uses.
fn sources(dir: &Path, into: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).expect("reading a source directory") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            sources(&path, into);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let text = std::fs::read_to_string(&path).expect("reading a source file");
            into.push((path.to_string_lossy().into_owned(), text));
        }
    }
}

/// `src/`-relative path, for a readable failure message.
fn relative(path: &str) -> String {
    path.rsplit_once("/src/")
        .map_or_else(|| path.to_string(), |(_, rest)| format!("src/{rest}"))
}

/// Every occurrence of `StoryEvent::StoryCreated {` outside a `//` comment,
/// as `"path:line: code"`. Extracted from the test below so the positive
/// control (`the_scan_would_notice_a_new_writer`) can drive it with synthetic
/// input rather than the real tree, the same reason
/// `tests/priority_rubric.rs::story_ids_in` and
/// `tests/release_targets.rs::cfg_target_oses` are their own functions.
fn story_created_sites(files: &[(String, String)], allowed: &[(&str, &str)]) -> Vec<String> {
    const ALLOWED_PREFIX: &str = "src/store/";
    const MARKER: &str = "StoryEvent::StoryCreated {";

    let mut breaches = Vec::new();
    for (path, text) in files {
        let relative = relative(path);
        if relative.starts_with(ALLOWED_PREFIX) || allowed.iter().any(|(a, _)| *a == relative) {
            continue;
        }
        for (number, line) in text.lines().enumerate() {
            let code = line.trim_start();
            if code.starts_with("//") || code.starts_with('*') {
                continue;
            }
            if code.contains(MARKER) {
                breaches.push(format!("{relative}:{}: {}", number + 1, code.trim()));
            }
        }
    }
    breaches
}

/// The files that may write a `StoryCreated` event, and why each does not
/// need its own unassessed-priority check.
///
/// `src/store/` is exempt for the same reason `type_slug_call_sites.rs` and
/// `state_set_funnel.rs` exempt it: that layer declares and implements
/// storage, and its conformance suite writes deliberately degenerate
/// histories through it to prove what the storage layer guarantees.
const ALLOWED: [(&str, &str); 6] = [
    (
        "src/domain.rs",
        "the event enum's own definition, fold_story's match arm (event_kind, \
         the fold itself), and that function's own unit-test fixtures -- not a \
         creation door",
    ),
    (
        "src/service/story.rs",
        "creation_events -- the door story new, the TUI's create form, and \
         service::grouping's create_phase/create_epic all share. \
         Invocation::New's dispatch arm (src/invoke.rs) checks \
         StorySnapshot::priority_assessed on its answer (SH-354/SH-359); \
         phase/epic creation is exempt under the rubric itself -- an epic's \
         priority is the max of its children and is reporting-only, and \
         story next never surfaces a story with children",
    ),
    (
        "src/service/transfer.rs",
        "import_events -- the door story import and story decompose share. \
         invoke::batch_priority_warnings checks every created view's \
         priority_assessed and raises the aggregate warning (SH-358)",
    ),
    (
        "src/storage.rs",
        "the legacy rollback path (store -> export -> a legacy tree), kept on \
         purpose per this project's own standing rule -- not reachable through \
         any live creation command, so there is no operator decision to warn \
         about",
    ),
    (
        "src/daemon/watch.rs",
        "test-only fixtures inside #[cfg(test)] mod tests, feeding \
         ChangeWatcher::notice -- not a production creation door",
    ),
    (
        "src/github/mod.rs",
        "create_story_from_issue calls sync.create_story, which is \
         StoreSyncStorage::create_story -> StoryService::create -- the same \
         creation_events door as above, not a second write site. \
         run_sync_with reads back priority_assessed for every pulled story and \
         warns (SH-358)",
    ),
];

#[test]
fn story_created_events_are_written_from_a_known_allowlist_of_files() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&root, &mut files);
    assert!(
        files.len() > 20,
        "expected the whole tree, got {} -- the walk is broken, not the codebase",
        files.len()
    );

    let breaches = story_created_sites(&files, &ALLOWED);

    assert!(
        breaches.is_empty(),
        "a new writer of StoryEvent::StoryCreated appeared outside the known doors. Decide \
         whether it can leave a story unassessed -- if so it needs \
         priority_notice::unassessed_warning (or the batch/sync equivalent), the way \
         Invocation::New, batch_priority_warnings and run_sync_with all do. Then add it to \
         this test's ALLOWED with the reason.\n  {}\nThe allowlist today:\n  {}",
        breaches.join("\n  "),
        ALLOWED
            .iter()
            .map(|(file, why)| format!("{file} — {why}"))
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

/// The positive control for the scan above: without it, an empty `breaches`
/// list is indistinguishable between "the tree is clean" and "the pattern
/// stopped matching anything." Same move as
/// `tests/priority_rubric.rs::the_story_id_scan_would_notice_one` and
/// `tests/release_targets.rs::the_source_arm_pattern_matches_what_it_claims_to`
/// — this repo prices a vacuous pass at the severity of the claim it fails to
/// deliver, plus one level.
#[test]
fn the_scan_would_notice_a_new_writer_and_ignore_a_comment_or_an_allowed_file() {
    let files = vec![
        (
            "/repo/src/service/some_new_door.rs".to_string(),
            "fn make(&self) {\n    let e = StoryEvent::StoryCreated {\n        at: now,\n    };\n}"
                .to_string(),
        ),
        (
            "/repo/src/service/story.rs".to_string(),
            "// StoryEvent::StoryCreated { would not count even if quoted here\nStoryEvent::StoryCreated {"
                .to_string(),
        ),
    ];
    let allowed: [(&str, &str); 1] = [("src/service/story.rs", "already accounted for")];

    let breaches = story_created_sites(&files, &allowed);

    assert_eq!(
        breaches.len(),
        1,
        "expected exactly the unallowed file's real line to be flagged: {breaches:?}"
    );
    assert!(breaches[0].starts_with("src/service/some_new_door.rs:2:"));
}

/// Whether an [`Invocation`] variant's dispatch arm can leave a *new* story
/// unassessed if nobody stated one.
///
/// No wildcard arm: adding a 65th variant fails this to compile until its
/// author answers the question, the same anti-drift shape
/// `mcp::tools::tool_for_variant` already gives this exact enum for a
/// different question ("which MCP tool built this").
///
/// Only `New`, `Import` and `Decompose` answer `true` — the three doors that
/// construct a [`storyhook::domain::ImportStory`]/priority-bearing input
/// directly from a caller's own words. Every other variant either does not
/// create a story at all, or (`ImportProject`) replays a byte-for-byte export
/// whose priority is a historical fact rather than a decision this dispatch
/// could ask about.
fn creates_a_story_that_could_land_unassessed(invocation: &Invocation) -> bool {
    match invocation {
        Invocation::New { .. } | Invocation::Import { .. } | Invocation::Decompose { .. } => true,

        Invocation::Help
        | Invocation::Project { .. }
        | Invocation::Publish { .. }
        | Invocation::MemberAdd { .. }
        | Invocation::State { .. }
        | Invocation::List { .. }
        | Invocation::Search { .. }
        | Invocation::Next { .. }
        | Invocation::Summary
        | Invocation::Report { .. }
        | Invocation::Doctor { .. }
        | Invocation::DoctorAbandoned { .. }
        | Invocation::DoctorCrashes { .. }
        | Invocation::Show { .. }
        | Invocation::Log { .. }
        | Invocation::Comment { .. }
        | Invocation::Assign { .. }
        | Invocation::SetState { .. }
        | Invocation::SetAwaiting { .. }
        | Invocation::ClearAwaiting { .. }
        | Invocation::SetPriority { .. }
        | Invocation::SetLabels { .. }
        | Invocation::Reopen { .. }
        | Invocation::Hide { .. }
        | Invocation::Unhide { .. }
        | Invocation::HideState { .. }
        | Invocation::Delete { .. }
        | Invocation::Purge { .. }
        | Invocation::BulkUpdate { .. }
        | Invocation::Export
        // A restore, not a creation decision -- see this file's own ALLOWED
        // table and CLAUDE.md's standing rule on src/storage.rs.
        | Invocation::ImportProject { .. }
        | Invocation::Migrate { .. }
        | Invocation::Context { .. }
        | Invocation::Handoff { .. }
        // Phase/epic creation goes through service::grouping, exempt under the
        // rubric itself (see ALLOWED's entry for src/service/story.rs).
        | Invocation::Phase { .. }
        | Invocation::Type { .. }
        | Invocation::Epic { .. }
        | Invocation::Graph { .. }
        | Invocation::SetFields { .. }
        | Invocation::Relate { .. }
        | Invocation::Hooks { .. }
        | Invocation::Scaffold { .. }
        | Invocation::CommitSync { .. }
        // Creates stories on GitHub's behalf, but through the same
        // creation_events door -- and run_sync_with warns on its own answer
        // (SH-358). Not a second decision point for this classification.
        | Invocation::GithubSync { .. }
        | Invocation::LinkPr { .. }
        | Invocation::UnlinkPr { .. }
        | Invocation::PrCheck { .. }
        | Invocation::GithubAuth { .. }
        | Invocation::HelpTopic { .. }
        | Invocation::HelpCompact
        | Invocation::HelpAll
        | Invocation::Plugin { .. }
        | Invocation::Web { .. }
        | Invocation::Token { .. }
        | Invocation::Daemon { .. }
        | Invocation::Store { .. }
        | Invocation::SessionStart
        | Invocation::Update { .. }
        | Invocation::Version
        | Invocation::ProjectSnapshot
        | Invocation::History { .. } => false,
    }
}

#[test]
fn every_invocation_that_can_create_a_story_is_classified() {
    // The compiler already proved the match above is exhaustive; this proves
    // the three `true` arms are exactly the doors SH-358 actually fixed, so a
    // future edit that flips one silently is still caught.
    let creating = ["New", "Import", "Decompose"];
    for name in creating {
        assert!(
            [
                Invocation::New {
                    title: String::new(),
                    state: None,
                    story_type: None,
                    description: None,
                    priority: None,
                    labels: None,
                    assignee: None,
                    draft: false,
                },
                Invocation::Import { file: None },
                Invocation::Decompose {
                    file: None,
                    stdin: false,
                    dry_run: false,
                },
            ]
            .iter()
            .any(|inv| creates_a_story_that_could_land_unassessed(inv)
                && format!("{inv:?}").starts_with(name)),
            "{name} must classify as creating"
        );
    }
    assert!(!creates_a_story_that_could_land_unassessed(
        &Invocation::Help
    ));
    assert!(!creates_a_story_that_could_land_unassessed(
        &Invocation::ImportProject {
            file: String::new(),
            legacy_links: false,
        }
    ));
}
