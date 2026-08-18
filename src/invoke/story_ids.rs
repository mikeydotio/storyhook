//! Canonicalizing the story ids in an [`Invocation`], once and at the door.
//!
//! `story show 5` and `story show SH-5` name the same story, and the expansion
//! happens here — before any dispatch arm runs — so that **every arm inherits
//! it** and nothing downstream ever sees the bare form. That placement is the
//! whole design (SH-118), and the alternatives were rejected for measured
//! reasons rather than taste:
//!
//! * **Not in the CLI parser.** The prefix is a property of the selected
//!   project, which the parser cannot see; and [`Invocation`] crosses
//!   `/api/v1/invoke`, so a rule applied on the client does not exist for the
//!   dashboard, the TUI, or a hand-built request.
//! * **Not in the resolution funnels.** `service::resolve_story` looks like the
//!   natural home until you read [`crate::service::relation`], which resolves
//!   both ids, then compares them with a **raw string** `if a == b` for the
//!   self-relation guard and persists `other_id` as the **raw string** into an
//!   append-only event log. Expanding inside the funnel would let
//!   `story relate SH-1 blocks 1` pass the self-relation guard and write `"1"`
//!   into history — a wrong answer in a log that is by design never rewritten.
//!
//! # Completeness is a compile error, not a checklist
//!
//! [`positions`] matches every [`Invocation`] variant with **no catch-all**, so
//! a variant added later does not compile until somebody decides whether it
//! carries a story id. That property is not decoration: the audit written to
//! enumerate the id positions for this story missed three of them
//! (`HistoryAction::Read`, `HistoryAction::Restore`, `GraphMode::BlockedBy`),
//! and two independent reviewers each found a different subset. A hand-kept
//! list of id-bearing verbs is exactly the thing that was already wrong.

use crate::cli::{AttachmentAction, EpicAction, GraphMode, HistoryAction, Invocation, PhaseAction};
use crate::error::AppError;
use crate::service::Ctx;
use crate::store::{ProjectRecord, ReadOps, Store, StoreError, StoryRef};

/// Rewrites every story id in `invocation` into its canonical form.
///
/// A bare integer becomes `PREFIX-n`; a canonical id under the selected
/// project's prefix is left as it is; a canonical id under a *different* prefix
/// is refused, naming both; anything else is passed through untouched, so a
/// token that was never an id still reaches the arm that will report it as not
/// found, quoted the way the user typed it.
///
/// # Errors
///
/// [`AppError::Validation`] — exit 2 — for an id naming another project. Not
/// [`AppError::NotFound`]: the story is not missing, the command is wrong, and
/// the two invite opposite next steps.
///
/// # Cost
///
/// One project-row read, and **only** when the invocation carries at least one
/// id position — so `list`, `next` and `summary` pay nothing. A whole-command
/// baseline is 8.8 ms; the arms that carry ids already take at least one read
/// of their own.
pub(crate) fn canonicalize<S: Store>(
    ctx: &Ctx<'_, S>,
    invocation: &mut Invocation,
) -> Result<(), AppError> {
    let mut slots = positions(invocation);
    if slots.is_empty() {
        return Ok(());
    }
    let project = selected_project(ctx)?;
    for slot in &mut slots {
        let token = slot.clone();
        match StoryRef::classify(&project.prefix, &token) {
            StoryRef::Here(number) => **slot = number.to_id(&project.prefix),
            StoryRef::Unrecognized => {}
            StoryRef::Elsewhere { prefix } => {
                return Err(foreign_prefix_refusal(ctx, &project, &token, &prefix)?);
            }
        }
    }
    Ok(())
}

/// [`canonicalize`] for a single id, for the one route that does not build an
/// [`Invocation`].
///
/// `PATCH /api/repos/{id}/story/{story}` calls `StoryService::set_fields` and
/// reads the view itself rather than dispatching twice, so it never passes the
/// gate in [`crate::invoke::dispatch`]. Rather than leave that route as the one
/// place a bare id does not work, it calls this — the same classifier, the same
/// refusal, one call site more.
///
/// # Errors
///
/// As [`canonicalize`].
pub(crate) fn canonicalize_one<S: Store>(
    ctx: &Ctx<'_, S>,
    token: &str,
) -> Result<String, AppError> {
    let project = selected_project(ctx)?;
    match StoryRef::classify(&project.prefix, token) {
        StoryRef::Here(number) => Ok(number.to_id(&project.prefix)),
        StoryRef::Unrecognized => Ok(token.to_string()),
        StoryRef::Elsewhere { prefix } => {
            Err(foreign_prefix_refusal(ctx, &project, token, &prefix)?)
        }
    }
}

/// The row of the project this invocation is scoped to.
fn selected_project<S: Store>(ctx: &Ctx<'_, S>) -> Result<ProjectRecord, AppError> {
    let project = ctx.project();
    Ok(ctx.store().read(|tx| {
        tx.project(project)?
            .ok_or_else(|| StoreError::NotFound(format!("project {project} does not exist")))
    })?)
}

/// What to tell someone whose id names a prefix the selected project does not
/// have.
///
/// Names **both** sides always — the id with its prefix, and the scope with
/// its — because both are known without asking the store anything. Only the
/// remedy varies, over how many projects hold the foreign prefix, and that is
/// the one thing worth a second read: `projects.prefix` carries no unique
/// index, and the store's own conformance suite seeds three projects with the
/// same prefix, so 0, 1 and *many* are all reachable states.
///
/// The lookup is paid only here, on a command that is already failing — the
/// same bargain [`crate::service::project::no_project_refusal`] strikes with
/// its origin line.
///
/// **It never switches project, not even when exactly one project holds the
/// prefix.** `--project` is binding (SH-116), and a rule that resolved by
/// prefix would let `story delete OTH-5`, typed in the wrong shell, delete in
/// the other project — while behaving differently the day somebody creates a
/// second project with that prefix.
fn foreign_prefix_refusal<S: Store>(
    ctx: &Ctx<'_, S>,
    scope: &ProjectRecord,
    token: &str,
    foreign: &str,
) -> Result<AppError, AppError> {
    let mut claimants: Vec<String> = ctx
        .store()
        .read(|tx| {
            Ok(tx
                .projects()?
                .into_iter()
                .filter(|project| project.prefix.eq_ignore_ascii_case(foreign))
                .map(|project| project.slug)
                .collect::<Vec<_>>())
        })
        .map_err(AppError::from)?;
    claimants.sort();

    let (held_by, remedy) = match claimants.as_slice() {
        [] => (
            format!("prefix `{foreign}`, which no project in this store holds"),
            "`story project list` shows the projects it does have.".to_string(),
        ),
        [only] => (
            format!("prefix `{foreign}`, held by project `{only}`"),
            format!(
                "Re-run it naming the story's own project:\n\n  story --project {only} <command>"
            ),
        ),
        many => (
            format!(
                "prefix `{foreign}`, held by {} projects: {}",
                many.len(),
                many.join(", ")
            ),
            "A prefix does not identify a project, so name the one you mean:\n\n  story \
             --project <slug> <command>"
                .to_string(),
        ),
    };

    Ok(AppError::Validation(format!(
        "story id `{token}` does not belong to project `{}`.\n\n  id     {token} - {held_by}\n  \
         scope  {} - prefix `{}`\n\nNothing has been read or written. {remedy}",
        scope.slug, scope.slug, scope.prefix
    )))
}

/// Every position in `invocation` that holds a story id, as mutable slots.
///
/// **Exhaustive with no catch-all**, which is the point: a new variant, or a
/// new arm of one of the four nested action enums, stops this file compiling
/// until somebody decides whether it names a story.
///
/// Only genuine id positions belong here. The neighbouring `String` fields —
/// `Relate::relation`, `SetState::state`, `PhaseAction::Add::phase` — are not
/// ids, and canonicalizing one would corrupt it silently rather than fail;
/// `tests/bare_integer_ids.rs` asserts each of those survives an expansion
/// beside it.
fn positions(invocation: &mut Invocation) -> Vec<&mut String> {
    match invocation {
        Invocation::Show { id }
        | Invocation::Log { id }
        | Invocation::Comment { id, .. }
        | Invocation::Assign { id, .. }
        | Invocation::SetState { id, .. }
        | Invocation::SetPriority { id, .. }
        | Invocation::SetLabels { id, .. }
        | Invocation::Reopen { id, .. }
        | Invocation::Hide { id }
        | Invocation::Unhide { id }
        | Invocation::Publish { id }
        | Invocation::Delete { id, .. }
        | Invocation::Purge { id, .. }
        | Invocation::SetFields { id, .. }
        // `url` is a pull request URL, not a story id — excluded the same way
        // `Relate::relation` is.
        | Invocation::LinkPr { id, .. }
        | Invocation::UnlinkPr { id, .. } => vec![id],

        Invocation::Relate { a, b, .. } => vec![a, b],
        // `on` names blockers just as `Relate::b` names a relation target — a
        // bare `story block SH-1 --on 9` must expand `9` the same way
        // `story relate SH-1 blocked-by 9` does (SH-398).
        Invocation::SetAwaiting { id, on, .. } | Invocation::ClearAwaiting { id, on } => {
            std::iter::once(id).chain(on.iter_mut()).collect()
        }
        Invocation::BulkUpdate { updates } => updates.iter_mut().map(|(id, _)| id).collect(),
        Invocation::GithubSync { id, .. } | Invocation::PrCheck { id } => id.iter_mut().collect(),

        Invocation::Phase { action } => match action {
            PhaseAction::Add { id, .. } | PhaseAction::Remove { id } => vec![id],
            PhaseAction::List | PhaseAction::Show { .. } | PhaseAction::Create { .. } => Vec::new(),
        },
        Invocation::Epic { action } => match action {
            EpicAction::Show { id } => vec![id],
            EpicAction::Add { epic_id, story_id } => vec![epic_id, story_id],
            EpicAction::List | EpicAction::Create { .. } => Vec::new(),
        },
        Invocation::Graph { mode } => match mode {
            GraphMode::BlockedBy(id) => vec![id],
            GraphMode::Overview | GraphMode::CriticalPath | GraphMode::ParallelGroups => Vec::new(),
        },
        Invocation::History { action } => match action {
            HistoryAction::Read { id } | HistoryAction::Restore { id, .. } => vec![id],
        },
        Invocation::Attachment { action } => match action {
            AttachmentAction::Add { id, .. }
            | AttachmentAction::List { id }
            | AttachmentAction::Remove { id, .. }
            | AttachmentAction::Save { id, .. } => vec![id],
        },

        Invocation::Help
        | Invocation::Project { .. }
        | Invocation::New { .. }
        | Invocation::MemberAdd { .. }
        // `state` is a state slug, not a story id — same reason `SetState::state`
        // above is excluded rather than swept in with `id`.
        | Invocation::HideState { .. }
        | Invocation::State { .. }
        | Invocation::List { .. }
        | Invocation::Search { .. }
        | Invocation::Next { .. }
        | Invocation::Summary
        | Invocation::Report { .. }
        | Invocation::Doctor { .. }
        | Invocation::DoctorAbandoned { .. }
        | Invocation::DoctorCrashes { .. }
        | Invocation::Import { .. }
        | Invocation::Decompose { .. }
        | Invocation::Export
        | Invocation::ImportProject { .. }
        | Invocation::Migrate { .. }
        | Invocation::Context { .. }
        | Invocation::Handoff { .. }
        | Invocation::Type { .. }
        | Invocation::Hooks { .. }
        | Invocation::Scaffold { .. }
        | Invocation::CommitSync { .. }
        | Invocation::HelpTopic { .. }
        | Invocation::HelpCompact
        | Invocation::HelpAll
        | Invocation::Plugin { .. }
        | Invocation::Web { .. }
        | Invocation::Daemon { .. }
        | Invocation::Token { .. }
        | Invocation::Store { .. }
        | Invocation::SessionStart
        | Invocation::Update { .. }
        | Invocation::Version
        | Invocation::ProjectSnapshot
        // Store-wide, not story-scoped — see the type's own doc.
        | Invocation::GithubAuth { .. } => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ConflictSide;

    /// Drives a bare `1` through every id position and returns what came back,
    /// with a fake expansion so the assertions read as text rather than as
    /// calls.
    fn expanded(mut invocation: Invocation) -> Invocation {
        for slot in &mut positions(&mut invocation) {
            if **slot == "1" {
                **slot = "SH-1".to_string();
            }
        }
        invocation
    }

    #[test]
    fn every_id_position_is_reachable() {
        // One case per id-bearing variant. The list is what `positions` claims
        // to cover; if a variant is added and given an arm here that returns
        // nothing, this test says so.
        let cases: Vec<(&str, Invocation, usize)> = vec![
            ("show", Invocation::Show { id: "1".into() }, 1),
            (
                "comment",
                Invocation::Comment {
                    id: "1".into(),
                    text: "t".into(),
                },
                1,
            ),
            (
                "relate",
                Invocation::Relate {
                    a: "1".into(),
                    relation: "blocks".into(),
                    b: "1".into(),
                    remove: false,
                },
                2,
            ),
            (
                "bulk-update",
                Invocation::BulkUpdate {
                    updates: vec![("1".into(), "done".into()), ("1".into(), "todo".into())],
                },
                2,
            ),
            (
                "github-sync",
                Invocation::GithubSync {
                    id: Some("1".into()),
                    dry_run: false,
                    resolve: Some(ConflictSide::Local),
                    strategy: None,
                    mode: None,
                },
                1,
            ),
            (
                "link-pr",
                Invocation::LinkPr {
                    id: "1".into(),
                    url: "https://github.com/acme/widgets/pull/7".into(),
                    close_on_merge: true,
                },
                1,
            ),
            (
                "unlink-pr",
                Invocation::UnlinkPr {
                    id: "1".into(),
                    url: "https://github.com/acme/widgets/pull/7".into(),
                },
                1,
            ),
            (
                "pr-check",
                Invocation::PrCheck {
                    id: Some("1".into()),
                },
                1,
            ),
            (
                "phase add",
                Invocation::Phase {
                    action: PhaseAction::Add {
                        id: "1".into(),
                        phase: "3".into(),
                    },
                },
                1,
            ),
            (
                "phase remove",
                Invocation::Phase {
                    action: PhaseAction::Remove { id: "1".into() },
                },
                1,
            ),
            (
                "epic show",
                Invocation::Epic {
                    action: EpicAction::Show { id: "1".into() },
                },
                1,
            ),
            (
                "epic add",
                Invocation::Epic {
                    action: EpicAction::Add {
                        epic_id: "1".into(),
                        story_id: "1".into(),
                    },
                },
                2,
            ),
            (
                "graph --blocked-by",
                Invocation::Graph {
                    mode: GraphMode::BlockedBy("1".into()),
                },
                1,
            ),
            (
                "history read",
                Invocation::History {
                    action: HistoryAction::Read { id: "1".into() },
                },
                1,
            ),
            (
                "history restore",
                Invocation::History {
                    action: HistoryAction::Restore {
                        id: "1".into(),
                        events: Vec::new(),
                    },
                },
                1,
            ),
        ];

        for (name, mut invocation, expected) in cases {
            assert_eq!(
                positions(&mut invocation).len(),
                expected,
                "`{name}` should expose {expected} id position(s)"
            );
        }
    }

    #[test]
    fn a_verb_with_no_story_id_exposes_nothing() {
        // The read is skipped entirely for these, so this is a cost assertion
        // as much as a correctness one.
        let mut cases = vec![
            Invocation::Summary,
            Invocation::Export,
            Invocation::SessionStart,
            Invocation::Search { query: "1".into() },
            Invocation::New {
                title: "1".into(),
                state: None,
                story_type: None,
                description: None,
                priority: None,
                labels: None,
                assignee: None,
                draft: false,
            },
            Invocation::Graph {
                mode: GraphMode::Overview,
            },
            Invocation::Phase {
                action: PhaseAction::Show { phase: "1".into() },
            },
        ];
        for invocation in &mut cases {
            assert!(
                positions(invocation).is_empty(),
                "{invocation:?} carries no story id"
            );
        }
    }

    #[test]
    fn the_argument_beside_an_id_is_never_touched() {
        // The too-far guard, at the level the mistake would be made. Each of
        // these has a short string next to the id that a careless `positions`
        // arm could hand out — and `phase` is the sharp one, because it is
        // itself a bare integer.
        let relate = expanded(Invocation::Relate {
            a: "1".into(),
            relation: "blocks".into(),
            b: "SH-2".into(),
            remove: false,
        });
        assert_eq!(
            relate,
            Invocation::Relate {
                a: "SH-1".into(),
                relation: "blocks".into(),
                b: "SH-2".into(),
                remove: false,
            }
        );

        let phase = expanded(Invocation::Phase {
            action: PhaseAction::Add {
                id: "1".into(),
                phase: "1".into(),
            },
        });
        assert_eq!(
            phase,
            Invocation::Phase {
                action: PhaseAction::Add {
                    id: "SH-1".into(),
                    phase: "1".into(),
                }
            },
            "the phase number is not a story id, however much it looks like one"
        );

        let moved = expanded(Invocation::SetState {
            id: "1".into(),
            state: "todo".into(),
            comment: Some("1".into()),
            if_state: Some("in-progress".into()),
            awaiting: None,
        });
        assert_eq!(
            moved,
            Invocation::SetState {
                id: "SH-1".into(),
                state: "todo".into(),
                comment: Some("1".into()),
                if_state: Some("in-progress".into()),
                awaiting: None,
            },
            "a comment body that happens to read `1` is prose, not a handle"
        );

        let bulk = expanded(Invocation::BulkUpdate {
            updates: vec![("1".into(), "1".into())],
        });
        assert_eq!(
            bulk,
            Invocation::BulkUpdate {
                updates: vec![("SH-1".into(), "1".into())],
            },
            "only the id half of each pair is an id"
        );
    }
}
