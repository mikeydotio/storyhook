//! `story commit-sync`: linking git commits to the stories they name.
//!
//! Reads the repository's log through `git` itself, finds story ids in commit
//! *subjects*, and records each one as a `[git] <short-hash>: <subject>` comment
//! on the story — moving it into the project's active state the first time it is
//! mentioned, if the project has one and has not turned that off.
//!
//! # Scanning subjects only is a known limitation, not an oversight
//!
//! `--format=%H %s` gives the subject line and nothing else, so a commit whose
//! body says `Closes SH-12` is invisible here. That is reproduced deliberately:
//! widening the scan changes which stories get comments, which is a behaviour
//! change, and it is scheduled as its own wave rather than smuggled in with a
//! port.
//!
//! # Idempotency
//!
//! A story already carrying a comment that starts `[git] <short-hash>:` is
//! skipped, so re-running over an overlapping window adds nothing. The check is
//! against the story's *events*, which is where the legacy path looked, and the
//! prefix stops at the colon so that a reworded commit does not duplicate.

use std::collections::BTreeSet;
use std::process::Command;

use crate::domain::{StateDef, StoryEvent, SuperState, extract_story_ids, parse_duration};
use crate::error::AppError;
use crate::store::{ExpectedSeq, ReadOps, Store};

use super::story::state_transition_events;
use super::{Ctx, append_and_fold, project_prefix, resolve_open_story};

/// The default scanning window, when `--since` is not given.
const DEFAULT_WINDOW: &str = "7d";

/// One commit as `git log` reported it.
struct Commit {
    /// The abbreviated hash, as it appears in the comment.
    short_hash: String,
    /// The subject line.
    subject: String,
}

/// A transition `commit-sync` performed, for its report.
struct Transition {
    story_id: String,
    from: String,
    to: String,
    short_hash: String,
}

/// Git integration over one project in one store.
pub struct GitService<'ctx, S: Store> {
    ctx: &'ctx Ctx<'ctx, S>,
}

impl<'ctx, S: Store> GitService<'ctx, S> {
    /// A git service bound to `ctx`.
    pub fn new(ctx: &'ctx Ctx<'ctx, S>) -> Self {
        Self { ctx }
    }

    /// Scans the repository's recent commits and records the stories they name.
    ///
    /// `since` is a duration such as `2h`, `1d` or `1w`; absent, the window is
    /// seven days.
    ///
    /// Each story that a commit names is updated in **one transaction**: its
    /// comment and, when this is the first commit to mention it, its move into
    /// the active state land together or not at all. The legacy path wrote them
    /// as two separate appends, so an interruption between them left a story
    /// commented but not moved.
    ///
    /// Fires no event hooks, which is what the legacy path did — a `commit-sync`
    /// over a week of history would otherwise fire a burst of `comment` and
    /// `state_change` hooks for work that happened days ago.
    pub fn commit_sync(&self, since: Option<&str>) -> Result<String, AppError> {
        require_git_repository(self.ctx.cwd())?;
        let window = since.unwrap_or(DEFAULT_WINDOW);
        let duration = parse_duration(window).ok_or_else(|| {
            AppError::Validation(format!("invalid duration `{window}` (use e.g. 2h, 1d, 1w)"))
        })?;
        let cutoff =
            (chrono::Utc::now() - duration).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let commits = read_log(self.ctx.cwd(), &cutoff)?;

        let project = self.ctx.project();
        let (prefix, active, default_open, auto_transition) = self.ctx.store().read(|tx| {
            let states = tx.states(project)?;
            Ok((
                project_prefix(tx, project)?,
                active_state(&states),
                default_open_state(&states),
                tx.settings(project)?.sync_auto_transition.unwrap_or(true),
            ))
        })?;

        let mut comments_added = 0usize;
        let mut stories_touched: BTreeSet<String> = BTreeSet::new();
        let mut transitions: Vec<Transition> = Vec::new();

        for commit in &commits {
            for story_id in extract_story_ids(&prefix, &commit.subject) {
                let moved = self.record_commit(
                    &prefix,
                    &story_id,
                    commit,
                    auto_transition
                        .then_some(active.as_ref())
                        .flatten()
                        .filter(|_| !transitions.iter().any(|t| t.story_id == story_id)),
                    default_open.as_ref(),
                )?;
                let Some(moved) = moved else {
                    continue;
                };
                comments_added += 1;
                stories_touched.insert(story_id.clone());
                if let Some((from, to)) = moved {
                    transitions.push(Transition {
                        story_id,
                        from,
                        to,
                        short_hash: commit.short_hash.clone(),
                    });
                }
            }
        }

        let mut message = format!(
            "scanned {} commits, added {} comments to {} stories",
            commits.len(),
            comments_added,
            stories_touched.len()
        );
        for transition in &transitions {
            message.push_str(&format!(
                "\n{}: {} \u{2192} {} (referenced in {})",
                transition.story_id, transition.from, transition.to, transition.short_hash
            ));
        }
        Ok(message)
    }

    /// Records one commit against one story, in one transaction.
    ///
    /// `Ok(None)` means there was nothing to do: the story is not open, or it
    /// already carries this commit's comment. `Ok(Some(None))` means a comment
    /// was added; `Ok(Some(Some((from, to))))` means it was added and the story
    /// moved.
    #[allow(clippy::type_complexity)]
    fn record_commit(
        &self,
        prefix: &str,
        story_id: &str,
        commit: &Commit,
        active: Option<&StateDef>,
        default_open: Option<&StateDef>,
    ) -> Result<Option<Option<(String, String)>>, AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();
        Ok(self.ctx.store().write(|tx| {
            let Ok((story_no, row)) = resolve_open_story(&*tx, project, prefix, story_id) else {
                // Not open, or not a story of this project. The legacy path
                // skipped it silently and so does this: a commit may name a
                // story that has since been closed, or one belonging to another
                // repository entirely.
                return Ok(None);
            };
            let marker = format!("[git] {}:", commit.short_hash);
            let stored = tx.events_for(project, story_no)?;
            let already = stored.iter().any(|event| {
                matches!(
                    event.known(),
                    Some(StoryEvent::StoryCommentAdded { text, .. }) if text.starts_with(&marker)
                )
            });
            if already {
                return Ok(None);
            }

            let states = tx.state_map(project)?;
            let mut events = vec![StoryEvent::StoryCommentAdded {
                at: now.clone(),
                text: format!("[git] {}: {}", commit.short_hash, commit.subject),
            }];
            // A story moves on the *first* commit that mentions it and only out
            // of the project's default open state — a story someone has already
            // moved on to review is not dragged back.
            let moved = match (active, default_open) {
                (Some(active), Some(default_open)) if row.snapshot.state == default_open.slug => {
                    events.extend(state_transition_events(
                        active,
                        row.snapshot.awaiting.is_some(),
                        &now,
                        Vec::new(),
                    ));
                    Some((row.snapshot.state.clone(), active.slug.clone()))
                }
                _ => None,
            };

            append_and_fold(
                tx,
                project,
                story_no,
                prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &events,
            )?;
            Ok(Some(moved))
        })?)
    }
}

/// Fails unless `root` is inside a git repository.
///
/// Reported as [`AppError::Validation`] with the same sentence the legacy path
/// used: `commit-sync` outside a repository is a usage mistake, not a storage
/// failure, and scripts test for it.
fn require_git_repository(root: &std::path::Path) -> Result<(), AppError> {
    let probed = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(root)
        .output();
    match probed {
        Ok(output) if output.status.success() => Ok(()),
        _ => Err(AppError::Validation("not a git repository".to_string())),
    }
}

/// The commits reachable from HEAD that are newer than `cutoff`.
fn read_log(root: &std::path::Path, cutoff: &str) -> Result<Vec<Commit>, AppError> {
    let output = Command::new("git")
        .args(["log", "--format=%H %s", &format!("--since={cutoff}")])
        .current_dir(root)
        .output()
        .map_err(|error| AppError::Storage(format!("failed to run git: {error}")))?;
    if !output.status.success() {
        return Err(AppError::Storage(format!(
            "git log failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut commits = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // A commit with an empty subject has no space to split on. It is still
        // *scanned* — the legacy count included it — but names no stories.
        let (hash, subject) = line.split_once(' ').unwrap_or((line, ""));
        commits.push(Commit {
            short_hash: hash[..7.min(hash.len())].to_string(),
            subject: subject.to_string(),
        });
    }
    Ok(commits)
}

/// The state a story moves into when a commit first mentions it.
///
/// The explicit `active` role wins; failing that, a project with exactly two
/// OPEN states is assumed to mean "todo, then the other one". The heuristic is
/// inherited, and it is why `active_state` can answer for a project that has
/// never configured a role.
fn active_state(states: &[StateDef]) -> Option<StateDef> {
    if let Some(state) = states
        .iter()
        .find(|state| state.role.as_deref() == Some("active"))
    {
        return Some(state.clone());
    }
    let open: Vec<&StateDef> = states
        .iter()
        .filter(|state| state.super_state == SuperState::Open)
        .collect();
    (open.len() == 2).then(|| open[1].clone())
}

/// The project's first configured OPEN state.
fn default_open_state(states: &[StateDef]) -> Option<StateDef> {
    states
        .iter()
        .find(|state| state.super_state == SuperState::Open)
        .cloned()
}
