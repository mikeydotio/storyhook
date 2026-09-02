//! Every door that can set `awaiting` carries SH-398's nudge, not a
//! hand-listed subset of them.
//!
//! `story block`, `story move <id> blocked --reason`, and `story set
//! --blocked`/`--json '{"blocked":...}'` are three independent dispatch arms
//! that all end up writing [`StoryEvent::StoryAwaitingSet`](storyhook::domain::StoryEvent::StoryAwaitingSet),
//! and each one used to have its own chance to leave a prose blocker mention
//! unlinked with nobody told. This file is a compile-time
//! exhaustive `match` over every [`Invocation`] variant, with no wildcard
//! arm, so a 65th variant is a compile error here until its author says
//! whether it sets `awaiting`.

use storyhook::cli::Invocation;

/// Whether dispatching `invocation` can write `StoryAwaitingSet` with a
/// caller-supplied reason — the condition under which SH-398's nudge must
/// run. No wildcard arm: a variant added later fails this to compile until
/// its author answers the question.
///
/// Only three answer `true`: `SetAwaiting` (`story block`), `SetState` (
/// `story move <id> blocked --reason`), and `SetFields` (`story set
/// --blocked`/`--json`). `ClearAwaiting` never *sets* a reason — it carries
/// the sibling "still blocked after unblocking" check instead, asserted
/// separately below, since there is nothing to classify across variants for
/// a single-variant question.
fn sets_awaiting(invocation: &Invocation) -> bool {
    match invocation {
        Invocation::SetAwaiting { .. }
        | Invocation::SetState { .. }
        | Invocation::SetFields { .. } => true,

        Invocation::Help
        | Invocation::Project { .. }
        | Invocation::New { .. }
        | Invocation::Publish { .. }
        | Invocation::MemberAdd { .. }
        | Invocation::State { .. }
        | Invocation::List { .. }
        | Invocation::Search { .. }
        | Invocation::Next { .. }
        // A claim's comment is a comment, never an `awaiting` reason (SH-476).
        | Invocation::Claim { .. }
        // Nor is an unclaim's (SH-483). It clears no `awaiting` either: the
        // release is an ordinary OPEN-to-OPEN transition, and
        // `state_transition_events` only clears `awaiting` on a close.
        | Invocation::Unclaim { .. }
        | Invocation::Engine { .. }
        | Invocation::Summary
        | Invocation::Report { .. }
        | Invocation::Doctor { .. }
        | Invocation::DoctorInstall
        | Invocation::DoctorAbandoned { .. }
        | Invocation::DoctorCrashes { .. }
        | Invocation::Show { .. }
        | Invocation::Log { .. }
        | Invocation::Comment { .. }
        | Invocation::Assign { .. }
        | Invocation::ClearAwaiting { .. }
        | Invocation::SetPriority { .. }
        | Invocation::SetLabels { .. }
        | Invocation::Reopen { .. }
        | Invocation::Hide { .. }
        | Invocation::Unhide { .. }
        | Invocation::HideState { .. }
        | Invocation::Delete { .. }
        | Invocation::BulkUpdate { .. }
        | Invocation::Import { .. }
        | Invocation::Decompose { .. }
        | Invocation::Export
        | Invocation::ImportProject { .. }
        | Invocation::Migrate { .. }
        | Invocation::Context { .. }
        | Invocation::Handoff { .. }
        | Invocation::Phase { .. }
        | Invocation::Type { .. }
        | Invocation::Epic { .. }
        | Invocation::Graph { .. }
        | Invocation::Relate { .. }
        | Invocation::Hooks { .. }
        | Invocation::Scaffold { .. }
        | Invocation::CommitSync { .. }
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
        | Invocation::History { .. }
        | Invocation::Attachment { .. } => false,
    }
}

#[test]
fn exactly_the_three_known_doors_set_awaiting() {
    let doors = ["SetAwaiting", "SetState", "SetFields"];
    let names: Vec<&str> = variant_names()
        .into_iter()
        .filter(|(_, invocation)| sets_awaiting(invocation))
        .map(|(name, _)| name)
        .collect();
    for door in doors {
        assert!(
            names.contains(&door),
            "{door} must be classified as setting `awaiting` -- either it no longer does, \
             or `sets_awaiting` regressed"
        );
    }
    assert_eq!(
        names.len(),
        doors.len(),
        "a variant other than {doors:?} is now classified `true`: {names:?} -- if it \
         genuinely sets `awaiting`, wire SH-398's nudge at its dispatch arm in src/invoke.rs \
         and add it to `doors` above; otherwise `sets_awaiting` regressed"
    );
}

/// One representative value per [`Invocation`] variant, named the same way
/// `mcp::tools::tool_for_variant`'s own tests do -- enough to drive
/// [`sets_awaiting`] without constructing every field meaningfully.
fn variant_names() -> Vec<(&'static str, Invocation)> {
    let s = || "SH-1".to_string();
    vec![
        ("Help", Invocation::Help),
        (
            "SetAwaiting",
            Invocation::SetAwaiting {
                id: s(),
                awaiting: Some(s()),
                on: Vec::new(),
            },
        ),
        (
            "ClearAwaiting",
            Invocation::ClearAwaiting {
                id: s(),
                on: Vec::new(),
            },
        ),
        (
            "SetState",
            Invocation::SetState {
                id: s(),
                state: s(),
                comment: None,
                if_state: None,
                awaiting: None,
            },
        ),
        (
            "SetFields",
            Invocation::SetFields {
                id: s(),
                title: None,
                state: None,
                priority: None,
                assignee: None,
                labels: None,
                blocked: None,
                unblocked: false,
                json: None,
                story_type: None,
                description: None,
            },
        ),
        ("Show", Invocation::Show { id: s() }),
    ]
}

/// Every dispatch arm in `src/invoke.rs` that [`sets_awaiting`] names true
/// must itself call `crate::block_notice::warnings` — proven by counting
/// call sites rather than trusting the classification alone, the same
/// belt-and-suspenders shape
/// `tests/unassessed_priority_paths.rs::story_created_events_are_written_from_a_known_allowlist_of_files`
/// pairs with its own exhaustive match.
#[test]
fn every_awaiting_setting_arm_calls_the_nudge() {
    let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/invoke.rs"))
        .expect("reading src/invoke.rs");
    let expected = variant_names()
        .into_iter()
        .filter(|(_, invocation)| sets_awaiting(invocation))
        .count();
    let calls = source.matches("crate::block_notice::warnings(").count();
    assert_eq!(
        calls, expected,
        "src/invoke.rs calls `block_notice::warnings` {calls} time(s), but {expected} \
         dispatch arm(s) are classified as setting `awaiting` -- every one of them must call \
         it exactly once, right after building the post-write story view"
    );
    assert!(
        source.contains("crate::block_notice::still_blocked_warning("),
        "`Invocation::ClearAwaiting`'s dispatch arm must call \
         `block_notice::still_blocked_warning` so an unblock that leaves the story blocked \
         by an edge is reported honestly, not as plain success (SH-312's doctrine)"
    );
}
