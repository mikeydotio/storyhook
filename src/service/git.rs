//! `story commit-sync`: linking git commits to the stories they name.
//!
//! Reads the repository's log through `git` itself, finds story ids anywhere in
//! a commit *message*, and records each one as a `[git] <short-hash>: <subject>`
//! comment on the story — moving it into the project's active state the first
//! time a commit **claims** it, if the project has one and has not turned that
//! off.
//!
//! # A mention is not a claim (SH-124)
//!
//! Every id used to move its story. A trailer meaning "this change relates to
//! that story" — `Refs CAL-21, CAL-28, CAL-29` — was read as "I am working on
//! those three now", and because `story next` and `story list --ready` both
//! exclude the active state, each wrongly-moved story was silently removed from
//! the set a contributor picks work from. The failure is quiet and it
//! accumulates: five stories in one sibling project, one of them moved three
//! times from prose alone.
//!
//! [`scan_story_refs`] now answers with an intent per id, and only a
//! [`Claim`](crate::domain::ReferenceIntent::Claim) may move a story. Linking is
//! unchanged and still universal — the comment trail is the half that was always
//! wanted. The grammar lives on `scan_story_refs`; what matters here is that the
//! run **reports what it declined to move**, because a project whose commits use
//! no claim word would otherwise see auto-transition simply stop, with nothing
//! to tell "off" from "broken".
//!
//! This was never a new promise. `story help commit-sync` has always said it
//! "auto-transitions stories based on commit patterns (e.g. `closes SH-1`)";
//! the keyword half had just never been implemented.
//!
//! # The whole message, not the subject line (SH-58)
//!
//! `--format=%H %s` gave the subject and nothing else, so a commit whose body
//! said `Closes SH-12` was invisible. That is not a corner case: this project
//! mandates Conventional Commits, whose subject line is reserved for a summary
//! under 72 characters, so a story reference belongs in the body — and
//! following the style guide therefore guaranteed the feature did nothing.
//!
//! Widening the scan was blocked for two waves for a reason worth recording.
//! Under `.storyhook/`, a link comment dirtied a version-controlled file, so
//! recording it took another commit, whose message named the same stories, so
//! scanning bodies would have linked *that* commit too, ad infinitum (SH-61).
//! What terminated the loop was the accident that bookkeeping commits kept
//! their story ids out of the subject. After the flip a story lives in the
//! store, a link comment writes nothing into the repository, and the loop has
//! no fuel — which is what
//! `tests/commit_sync_termination.rs` exists to prove rather than assume.
//!
//! # Parsing `%B` needs a record separator (also SH-58)
//!
//! `%B` emits multi-line records, and the parser was line-oriented: splitting
//! each line on its first space turns a body line `Closes SH-12` into
//! hash=`Closes`, subject=`SH-12`. That is worse than the miss it replaces,
//! because the short hash is the idempotency key, so a garbage hash breaks
//! "safe to run repeatedly" as well. [`read_log`] therefore asks for
//! `%H%x1f%B%x1e` and splits on the ASCII record and unit separators — control
//! characters git will not emit from a commit message — so **the hash comes
//! from `%H` and never from message text**.
//!
//! # Idempotency
//!
//! A story already carrying a comment that starts `[git] <short-hash>:` is
//! skipped, so re-running over an overlapping window adds nothing. The check is
//! against the story's *events*, which is where the legacy path looked, and the
//! prefix stops at the colon so that a reworded commit does not duplicate.

use std::collections::BTreeSet;

use crate::domain::{StateDef, StoryEvent, SuperState, parse_duration, scan_story_refs, short_sha};
use crate::error::AppError;
use crate::store::{ExpectedSeq, ProjectId, ReadOps, Store, StoreError, StoryNo};

use super::story::state_transition_events;
use super::{Ctx, append_and_fold, project_prefix, resolve_open_story};

/// The default scanning window, when `--since` is not given.
const DEFAULT_WINDOW: &str = "7d";

/// The separator between one `git log` record and the next.
///
/// ASCII RS. A commit message cannot contain one — git rejects NUL and this
/// family of control characters never survives an editor — so it cannot be
/// forged from message text, which is the whole point of choosing it over a
/// newline.
const RECORD_SEPARATOR: char = '\u{1e}';

/// The separator between a record's hash and its message. ASCII US; see
/// [`RECORD_SEPARATOR`].
const FIELD_SEPARATOR: char = '\u{1f}';

/// One commit as `git log` reported it.
struct Commit {
    /// The full forty-character hash, from `%H` alone. Nothing in the message
    /// can influence it.
    ///
    /// Full, not abbreviated, because it is what a link record stores: seven
    /// characters are unique in a repository today and not forever, and the
    /// record is permanent. Only the abbreviation is rendered.
    sha: String,
    /// The subject line — the message's first line, and the only part that
    /// reaches the rendered comment.
    subject: String,
    /// The whole message, subject included. This is what is scanned.
    message: String,
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
        let mut linked_only: BTreeSet<String> = BTreeSet::new();

        for commit in &commits {
            // The whole message, subject included — the comment still carries
            // the subject alone, because a multi-paragraph body in `story show`
            // would be unreadable.
            for reference in scan_story_refs(&prefix, &commit.message) {
                let claims = reference.claims();
                let story_id = reference.id;
                let moved = self.record_commit(
                    &prefix,
                    &story_id,
                    commit,
                    // A mention links and nothing more (SH-124). Only a claim
                    // is eligible to move a story, and only the first one in
                    // this run.
                    (auto_transition && claims)
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
                match moved {
                    Some((from, to)) => transitions.push(Transition {
                        story_id,
                        from,
                        to,
                        short_hash: short_sha(&commit.sha).to_string(),
                    }),
                    None => {
                        linked_only.insert(story_id);
                    }
                }
            }
        }

        // A story that a later commit claimed is not "linked only", however
        // many earlier commits merely mentioned it.
        for transition in &transitions {
            linked_only.remove(&transition.story_id);
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
        // Naming what was declined is what keeps this fix from having the same
        // shape as the defect it removes. Without it, a project whose commits
        // never use a claim word sees auto-transition simply stop, with nothing
        // to distinguish "off" from "broken".
        if !linked_only.is_empty() {
            message.push_str(&format!(
                "\nlinked without claiming: {} (no claim word, so state unchanged)",
                linked_only.iter().cloned().collect::<Vec<_>>().join(", ")
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
            if already_linked(&*tx, project, story_no, &commit.sha)? {
                return Ok(None);
            }

            let states = tx.state_map(project)?;
            let mut events = vec![StoryEvent::StoryCommitLinked {
                at: now.clone(),
                sha: commit.sha.clone(),
                subject: commit.subject.clone(),
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

/// Whether this commit is already linked to this story.
///
/// **Two probes, and the second is not redundant.** A record this binary writes
/// carries the full forty-character hash, so that is the first question. Rows
/// backfilled from *pre*-#18 link comments carry the seven-character
/// abbreviation, because `[git] <short>: <subject>` is all such a comment ever
/// preserved — and those rows are not merely historical: every unmigrated
/// `.storyhook` tree is full of them, and `story migrate` projects them the day
/// it reads one. Asking only about the full hash would re-link every commit a
/// migrated project already knew about.
///
/// Both are indexed primary-key probes, so this replaces a scan of every event
/// on the story with two lookups — and, unlike that scan, it cannot be
/// satisfied by a user typing a comment that starts `[git] `.
fn already_linked(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
    sha: &str,
) -> Result<bool, StoreError> {
    Ok(tx.commit_linked(project, story, sha)?
        || tx.commit_linked(project, story, short_sha(sha))?)
}

/// Fails unless `root` is inside a git repository.
///
/// Reported as [`AppError::Validation`] with the same sentence the legacy path
/// used: `commit-sync` outside a repository is a usage mistake, not a storage
/// failure, and scripts test for it.
fn require_git_repository(root: &std::path::Path) -> Result<(), AppError> {
    let probed = crate::env::git_env::command(root)
        .args(["rev-parse", "--git-dir"])
        .output();
    match probed {
        Ok(output) if output.status.success() => Ok(()),
        _ => Err(AppError::Validation("not a git repository".to_string())),
    }
}

/// The commits reachable from HEAD that are newer than `cutoff`.
///
/// One record per commit, `<full hash>\x1f<whole message>\x1e`, parsed by
/// separator rather than by line. See this module's header for why the format
/// is not `%H %B`: a line-oriented parse of a multi-line message reads body
/// text as a commit hash, and the hash is the idempotency key.
fn read_log(root: &std::path::Path, cutoff: &str) -> Result<Vec<Commit>, AppError> {
    let format = format!("--format=%H{FIELD_SEPARATOR}%B{RECORD_SEPARATOR}");
    let output = crate::env::git_env::command(root)
        .args(["log", &format, &format!("--since={cutoff}")])
        .output()
        .map_err(|error| AppError::Storage(format!("failed to run git: {error}")))?;
    if !output.status.success() {
        return Err(AppError::Storage(format!(
            "git log failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(parse_log(&String::from_utf8_lossy(&output.stdout)))
}

/// Splits `git log`'s separator-delimited output into commits.
///
/// Separated from [`read_log`] so the parse — the part SH-58 got wrong — can be
/// tested against fabricated output, including the shapes a real repository
/// makes it awkward to produce.
fn parse_log(text: &str) -> Vec<Commit> {
    let mut commits = Vec::new();
    for record in text.split(RECORD_SEPARATOR) {
        // git puts a newline between one record's terminator and the next
        // record's hash. Leading whitespace is therefore separator noise, and
        // the trailing newline `%B` always appends is not part of the message.
        let record = record.trim_start_matches(['\n', '\r']);
        if record.is_empty() {
            continue;
        }
        // A record with no field separator is not something git can produce.
        // Skipping it is deliberate: the alternative is inventing a hash, and a
        // wrong hash is the idempotency bug this parse exists to avoid.
        let Some((hash, message)) = record.split_once(FIELD_SEPARATOR) else {
            continue;
        };
        let message = message.trim_end_matches(['\n', '\r']);
        commits.push(Commit {
            sha: hash.to_string(),
            // A commit with an empty message has no first line. It is still
            // *scanned* — the count has always included it — but names nothing.
            subject: message.lines().next().unwrap_or("").to_string(),
            message: message.to_string(),
        });
    }
    commits
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

#[cfg(test)]
mod tests {
    use super::*;
    // `active_state` matches the literal `"active"`; the tests name the
    // constant, so the two agreeing is asserted rather than assumed.
    use crate::domain::STATE_ROLE_ACTIVE;

    /// Builds one `git log` record in the format [`read_log`] asks for.
    fn record(hash: &str, message: &str) -> String {
        format!("{hash}{FIELD_SEPARATOR}{message}\n{RECORD_SEPARATOR}")
    }

    fn state(slug: &str, super_state: SuperState, role: Option<&str>) -> StateDef {
        StateDef {
            slug: slug.to_string(),
            super_state,
            role: role.map(str::to_string),
            description: None,
        }
    }

    #[test]
    fn an_explicit_active_role_decides_where_a_claimed_story_moves() {
        let states = [
            state("todo", SuperState::Open, None),
            state("in-progress", SuperState::Open, Some(STATE_ROLE_ACTIVE)),
            state("blocked", SuperState::Open, None),
            state("done", SuperState::Closed, None),
        ];
        assert_eq!(
            active_state(&states).map(|state| state.slug),
            Some("in-progress".to_string())
        );
    }

    /// The inherited two-OPEN fallback, tested here because it can no longer
    /// be reached through the CLI.
    ///
    /// It answers only for a project with exactly two OPEN states and no role,
    /// and the required-state floor (SH-125) obliges every project to hold
    /// `todo`, `in-progress` **and** `blocked` as OPEN — three. So the input
    /// below is a catalog a conforming project cannot have: it survives for
    /// data written before the floor, which reaches this code through a read
    /// rather than through `story state`.
    #[test]
    fn two_open_states_and_no_role_means_the_second_one() {
        let states = [
            state("todo", SuperState::Open, None),
            state("doing", SuperState::Open, None),
            state("done", SuperState::Closed, None),
        ];
        assert_eq!(
            active_state(&states).map(|state| state.slug),
            Some("doing".to_string())
        );
    }

    #[test]
    fn a_project_with_no_role_and_three_open_states_gets_no_guess() {
        // What a conforming project looks like when nothing carries the role:
        // `commit-sync` comments and links, and moves nothing.
        let states = [
            state("todo", SuperState::Open, None),
            state("in-progress", SuperState::Open, None),
            state("blocked", SuperState::Open, None),
            state("done", SuperState::Closed, None),
        ];
        assert_eq!(active_state(&states), None);
    }

    #[test]
    fn one_open_state_and_no_role_gets_no_guess() {
        let states = [
            state("todo", SuperState::Open, None),
            state("done", SuperState::Closed, None),
        ];
        assert_eq!(active_state(&states), None);
    }

    #[test]
    fn a_record_yields_its_hash_subject_and_whole_message() {
        let commits = parse_log(&record(
            "0123456789abcdef0123456789abcdef01234567",
            "feat: a thing\n\nCloses SH-1",
        ));
        assert_eq!(commits.len(), 1);
        assert_eq!(short_sha(&commits[0].sha), "0123456");
        assert_eq!(commits[0].subject, "feat: a thing");
        assert_eq!(commits[0].message, "feat: a thing\n\nCloses SH-1");
    }

    /// The SH-58 trap: a body line that looks like the *old* format's output.
    ///
    /// Under `--format=%H %s` parsed per line, `deadbeef SH-1` became a commit
    /// with hash `deadbeef`. The separator format cannot express that, and this
    /// is what says so.
    #[test]
    fn body_text_shaped_like_a_log_line_is_message_not_hash() {
        let commits = parse_log(&record(
            "0123456789abcdef0123456789abcdef01234567",
            "fix: something\n\ndeadbeef SH-1 was the old parse's idea of a commit",
        ));
        assert_eq!(commits.len(), 1, "one commit, not two");
        assert_eq!(short_sha(&commits[0].sha), "0123456");
    }

    #[test]
    fn records_are_separated_by_the_record_separator_not_by_newlines() {
        let text = format!(
            "{}{}",
            record("aaaaaaaaaaaaaaaa", "one\n\nline two\nline three"),
            record("bbbbbbbbbbbbbbbb", "two")
        );
        let commits = parse_log(&text);
        assert_eq!(commits.len(), 2);
        assert_eq!(short_sha(&commits[0].sha), "aaaaaaa");
        assert_eq!(short_sha(&commits[1].sha), "bbbbbbb");
        assert_eq!(commits[1].subject, "two");
    }

    #[test]
    fn an_empty_log_yields_no_commits() {
        assert!(parse_log("").is_empty());
        assert!(parse_log("\n").is_empty());
    }

    /// A commit with an empty message is still counted, and names nothing.
    #[test]
    fn an_empty_message_is_a_commit_with_an_empty_subject() {
        let commits = parse_log(&record("cccccccccccccccc", ""));
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "");
        assert_eq!(commits[0].message, "");
    }

    /// Nothing git produces, so nothing is invented for it: a record with no
    /// field separator is skipped rather than given a fabricated hash.
    #[test]
    fn a_record_without_a_field_separator_is_skipped() {
        let text = format!("no-separator-here\n{RECORD_SEPARATOR}");
        assert!(parse_log(&text).is_empty());
    }
}
