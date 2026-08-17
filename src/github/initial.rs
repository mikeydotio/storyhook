use std::collections::BTreeMap;

use crate::domain::secret::{self, GithubToken};
use crate::error::AppError;
use crate::output::SetupPlan;

use super::api::GithubApiFactory;
use super::storage::SyncStorage;
use super::sync_state::{
    GithubSyncConfig, StoryIssueMapping, SyncMode, SyncSettings, detect_github_remote,
};
use super::types::GithubIssue;

/// The caller's GitHub credential, or the refusal that names what to do.
///
/// **This used to be a `std::env::var` read, and that was the defect** (SH-153).
/// Since SH-114 this code runs inside the daemon, whose environment is a
/// snapshot of whichever client happened to start it — so a caller who exported
/// a token was told it was unset, and a caller who had not exported one silently
/// spent the token of whoever had. The credential is read by the client now and
/// travels in the request envelope; this function is only the refusal.
///
/// # Errors
///
/// [`AppError::GithubAuth`] when the caller supplied no credential.
pub fn require_github_token(token: Option<&GithubToken>) -> Result<&GithubToken, AppError> {
    token.ok_or_else(|| AppError::GithubAuth(secret::NO_TOKEN.to_string()))
}

/// The initial-sync strategy, once known — the gated twin of
/// [`crate::cli::SetupStrategy`], which must exist in every build while this
/// one only exists where there is an engine to spend it on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitialStrategy {
    ImportAll,
    MatchTitles,
    PushOnly,
    FutureOnly,
}

/// The caller's answer to the initial-setup questions, known in advance —
/// either stated on the command line or gathered by the client's interactive
/// wizard on a round trip (SH-153's D2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupAnswers {
    pub strategy: InitialStrategy,
    pub mode: SyncMode,
}

/// What [`run_initial_setup`] decided.
pub enum InitialSetupOutcome {
    /// Nothing was known in advance: here is what setup would do, unwritten.
    Plan(Box<SetupPlan>),
    /// The caller supplied answers; setup ran and saved.
    Configured {
        /// **Not yet saved.** The caller — `run_sync_with` — decides whether
        /// to persist this, alongside every other write a dry run must not
        /// make. A version of this function once saved unconditionally here,
        /// which meant `story github-sync --dry-run --strategy … --mode …`
        /// wrote configuration to an unconfigured project despite `--dry-run`.
        config: GithubSyncConfig,
        /// Ambiguous match-by-title pairs that were found and left unlinked —
        /// empty unless [`InitialStrategy::MatchTitles`] was chosen. Carried
        /// back rather than printed: this runs in the daemon, where
        /// `eprintln!` reaches a log nobody reads (D3's own condition).
        notes: Vec<String>,
    },
}

/// Decides plan-vs-proceed from counts alone — no network, no storage.
///
/// **This is the one place in the file that makes the decision, and it needs
/// none of a [`super::api::GithubApi`]'s calls to do it** — those still run
/// first, to produce the counts this function decides from.
fn plan_or_answers(
    owner: &str,
    repo: &str,
    local_story_count: usize,
    open_issue_count: usize,
    exact_match_count: usize,
    answers: Option<SetupAnswers>,
) -> Result<SetupAnswers, Box<SetupPlan>> {
    match answers {
        Some(answers) => Ok(answers),
        None => Err(Box::new(SetupPlan {
            owner: owner.to_string(),
            repo: repo.to_string(),
            local_story_count,
            open_issue_count,
            exact_match_count,
        })),
    }
}

/// Computes (or plans) the initial sync setup. Never writes.
///
/// Called when `story github-sync` is run for the first time (no github-sync
/// configuration exists). Steps 1-3 — remote detection, token validation,
/// scanning both sides — are reads and always run, because the counts they
/// produce are what the plan reports. `answers` decides what happens after:
/// `None` returns [`InitialSetupOutcome::Plan`]; `Some` computes the config
/// that setup would produce and returns it inside
/// [`InitialSetupOutcome::Configured`] — unsaved. Persisting it, if at all, is
/// `run_sync_with`'s call: it is the one place that knows whether this run is
/// a dry run.
pub fn run_initial_setup(
    sync: &dyn SyncStorage,
    api_factory: &dyn GithubApiFactory,
    token: Option<&GithubToken>,
    answers: Option<SetupAnswers>,
) -> Result<InitialSetupOutcome, AppError> {
    // 1. Detect remote
    let github_repo = detect_github_remote(sync.root())?.ok_or_else(|| {
        AppError::Validation(
            "No GitHub remote found. Ensure `git remote origin` points to a GitHub repository."
                .to_string(),
        )
    })?;

    // 2. Validate token
    let token = require_github_token(token)?;
    let client = api_factory.build(
        token.expose().to_string(),
        github_repo.owner.clone(),
        github_repo.repo.clone(),
    );
    client.validate_token()?;

    // 3. Scan both sides
    let local_stories = sync.open_stories()?;
    let github_issues: Vec<GithubIssue> = client.list_issues(None, "open")?;
    // Filter out pull requests (list_issues already does this, but be defensive)
    let github_issues: Vec<&GithubIssue> = github_issues
        .iter()
        .filter(|i| !i.is_pull_request())
        .collect();

    let now = sync.now();
    // Computed unconditionally — a plan reports this count so the caller
    // knows what "match-titles" would do before choosing it, and if the
    // caller *did* choose it, the work is already done.
    let (match_mappings, ambiguous) = handle_match_by_title(&local_stories, &github_issues, &now);

    let answers = match plan_or_answers(
        &github_repo.owner,
        &github_repo.repo,
        local_stories.len(),
        github_issues.len(),
        match_mappings.len(),
        answers,
    ) {
        Err(plan) => return Ok(InitialSetupOutcome::Plan(plan)),
        Ok(answers) => answers,
    };

    let mappings = match answers.strategy {
        InitialStrategy::ImportAll => handle_import_all(&github_issues, &now),
        InitialStrategy::MatchTitles => match_mappings,
        InitialStrategy::PushOnly | InitialStrategy::FutureOnly => Vec::new(),
    };
    let notes = if answers.strategy == InitialStrategy::MatchTitles {
        ambiguous
    } else {
        Vec::new()
    };

    let last_sync_at = if answers.strategy == InitialStrategy::FutureOnly {
        Some(now.clone())
    } else {
        None
    };

    let config = GithubSyncConfig {
        github: github_repo,
        sync: SyncSettings {
            mode: answers.mode,
            last_sync_at,
            last_full_sync_at: None,
        },
        etags: BTreeMap::new(),
        mappings,
    };

    // Not saved here. `run_sync_with` decides whether to persist, alongside
    // every other write this file makes, all gated on the same `dry_run` — see
    // its own doc comment for why the write used to live here instead (SH-153,
    // the dry-run fix).
    Ok(InitialSetupOutcome::Configured { config, notes })
}

/// Import all open GitHub issues: create a mapping entry for each.
fn handle_import_all(issues: &[&GithubIssue], now: &str) -> Vec<StoryIssueMapping> {
    issues
        .iter()
        .map(|issue| StoryIssueMapping {
            story_id: String::new(), // will be assigned by orchestrator during import
            issue_number: issue.number,
            last_synced_at: now.to_string(),
            last_local_event_index: None,
        })
        .collect()
}

/// A title, ready to compare: trimmed, lowercased, and internal whitespace
/// collapsed to one space.
fn normalize_title(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Match local stories to GitHub issues by exact title, after normalizing —
/// SH-153's D3.
///
/// **Exact after normalizing, not substring.** The previous rule linked a story
/// titled "Fix parser" to an issue titled "Fix parser crash on empty input" —
/// plausible-looking, and wrong. Normalizing trims, lowercases and collapses
/// internal whitespace; matching then means the normalized strings are equal.
///
/// **Unique on both sides.** A normalized title claimed by more than one local
/// story, or matching more than one open issue, is not evidence of anything —
/// it is reported as ambiguous and neither side is linked. A title present on
/// only one side has no candidate at all and is not reported: that is the
/// ordinary case of an unrelated story or issue, not a finding.
///
/// **Order-invariant, by construction.** Both frequency maps are built once,
/// from the whole input, before any linking decision is made — no decision
/// reads a mapping another decision has already grown. The previous
/// implementation walked stories in order and excluded issues an earlier story
/// had already claimed, so which story got a given issue depended on the order
/// `open_stories()` happened to return them in.
///
/// **Never asked.** There is no per-pair "Link these?" menu any more: the
/// daemon has no terminal to ask from, and a match precise enough to be
/// automatic does not need confirming.
fn handle_match_by_title(
    stories: &[crate::domain::StorySnapshot],
    issues: &[&GithubIssue],
    now: &str,
) -> (Vec<StoryIssueMapping>, Vec<String>) {
    let mut stories_by_title: BTreeMap<String, Vec<&crate::domain::StorySnapshot>> =
        BTreeMap::new();
    for story in stories {
        stories_by_title
            .entry(normalize_title(&story.title))
            .or_default()
            .push(story);
    }

    let mut issues_by_title: BTreeMap<String, Vec<&&GithubIssue>> = BTreeMap::new();
    for issue in issues {
        issues_by_title
            .entry(normalize_title(&issue.title))
            .or_default()
            .push(issue);
    }

    let mut mappings = Vec::new();
    let mut ambiguous = Vec::new();

    for (title, matching_stories) in &stories_by_title {
        let Some(matching_issues) = issues_by_title.get(title) else {
            continue;
        };
        if matching_stories.len() == 1 && matching_issues.len() == 1 {
            mappings.push(StoryIssueMapping {
                story_id: matching_stories[0].id.clone(),
                issue_number: matching_issues[0].number,
                last_synced_at: now.to_string(),
                last_local_event_index: None,
            });
        } else {
            ambiguous.push(format!(
                "\"{title}\": {} local {} matched {} open {} by title -- not linked, ambiguous",
                matching_stories.len(),
                if matching_stories.len() == 1 {
                    "story"
                } else {
                    "stories"
                },
                matching_issues.len(),
                if matching_issues.len() == 1 {
                    "issue"
                } else {
                    "issues"
                },
            ));
        }
    }

    (mappings, ambiguous)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No answers means a plan, carrying exactly the counts given — and
    /// nothing written, which this function cannot even attempt: it has no
    /// access to `sync.save_config`.
    #[test]
    fn no_answers_yields_a_plan_carrying_the_counts() {
        let plan = plan_or_answers("acme", "widgets", 3, 12, 2, None)
            .expect_err("no answers is a plan, not a proceed");
        assert_eq!(
            *plan,
            SetupPlan {
                owner: "acme".to_string(),
                repo: "widgets".to_string(),
                local_story_count: 3,
                open_issue_count: 12,
                exact_match_count: 2,
            }
        );
    }

    /// Answers given means proceed, unchanged — this function does not
    /// interpret them, only decides whether there are any.
    #[test]
    fn answers_given_pass_through_unchanged() {
        let answers = SetupAnswers {
            strategy: InitialStrategy::PushOnly,
            mode: SyncMode::Off,
        };
        let proceed = plan_or_answers("acme", "widgets", 3, 12, 2, Some(answers.clone()))
            .expect("answers given means proceed");
        assert_eq!(proceed, answers);
    }

    /// `SyncMode::Auto` cannot reach `SetupAnswers` through anything this
    /// module exposes — `crate::cli::SetupMode` has no `Auto` variant to
    /// convert from, so the type system is the guard rather than a runtime
    /// check. This test exists so that claim stays checked: if `SetupAnswers`
    /// ever grows a way to hold `Auto`, this is where a test should catch it.
    #[test]
    fn a_setup_answer_of_auto_mode_is_unrepresentable_by_construction() {
        let modes = [SyncMode::Manual, SyncMode::Off];
        assert!(
            !modes.contains(&SyncMode::Auto),
            "the only two SetupAnswers::mode values this file ever constructs from \
             crate::cli::SetupMode are Manual and Off"
        );
    }

    fn story(id: &str, title: &str) -> crate::domain::StorySnapshot {
        crate::domain::StorySnapshot {
            id: id.to_string(),
            title: title.to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            state: "todo".to_string(),
            superstate: crate::domain::SuperState::Open,
            assignee: None,
            awaiting: None,
            comments: Vec::new(),
            referenced_by_commits: Vec::new(),
            relationships: Vec::new(),
            priority: crate::domain::Priority::None,
            priority_assessed: false,
            labels: Vec::new(),
            story_type: None,
            description: None,
            closed_at: None,
            deleted: false,
            deleted_reason: None,
            hidden_at: None,
            draft: false,
            attachments: Vec::new(),
            next_attachment_id: 1,
        }
    }

    fn issue(number: u64, title: &str) -> GithubIssue {
        GithubIssue {
            number,
            title: title.to_string(),
            body: None,
            state: "open".to_string(),
            state_reason: None,
            labels: Vec::new(),
            assignees: Vec::new(),
            milestone: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            closed_at: None,
            pull_request: None,
            comments: 0,
        }
    }

    /// An exact match after normalizing links; the old substring rule would
    /// have linked this to a *different*, wrong issue too.
    #[test]
    fn an_exact_title_after_normalizing_links() {
        let stories = vec![story("SH-1", "  Fix   the Parser  ")];
        let wrong = issue(2, "Fix the Parser crash on empty input");
        let right = issue(1, "fix the parser");
        let issues = vec![&wrong, &right];

        let (mappings, ambiguous) = handle_match_by_title(&stories, &issues, "now");

        assert_eq!(mappings.len(), 1, "{mappings:?}");
        assert_eq!(mappings[0].story_id, "SH-1");
        assert_eq!(
            mappings[0].issue_number, 1,
            "must link the exact match, not the substring one"
        );
        assert!(ambiguous.is_empty(), "{ambiguous:?}");
    }

    /// A story whose normalized title matches no issue's is not an error —
    /// it is the ordinary case of an unrelated story, and is silent.
    #[test]
    fn a_title_present_on_only_one_side_is_skipped_without_a_note() {
        let stories = vec![story("SH-1", "Nothing like it upstream")];
        let unrelated = issue(1, "A completely different issue");
        let issues = vec![&unrelated];

        let (mappings, ambiguous) = handle_match_by_title(&stories, &issues, "now");

        assert!(mappings.is_empty());
        assert!(ambiguous.is_empty());
    }

    /// Two stories sharing a normalized title against one matching issue is
    /// not evidence of anything — neither is linked, and it is named.
    #[test]
    fn a_title_shared_by_two_local_stories_is_ambiguous_and_unlinked() {
        let stories = vec![story("SH-1", "Fix the bug"), story("SH-2", "fix THE bug")];
        let matching = issue(1, "Fix the bug");
        let issues = vec![&matching];

        let (mappings, ambiguous) = handle_match_by_title(&stories, &issues, "now");

        assert!(mappings.is_empty(), "{mappings:?}");
        assert_eq!(ambiguous.len(), 1);
        assert!(ambiguous[0].contains("fix the bug"), "{}", ambiguous[0]);
        assert!(ambiguous[0].contains("2 local stories"), "{}", ambiguous[0]);
    }

    /// The same collision, on the issue side: one story title matching two
    /// open issues is equally unlinkable.
    #[test]
    fn a_title_shared_by_two_issues_is_ambiguous_and_unlinked() {
        let stories = vec![story("SH-1", "Fix the bug")];
        let a = issue(1, "Fix the bug");
        let b = issue(2, "fix the bug");
        let issues = vec![&a, &b];

        let (mappings, ambiguous) = handle_match_by_title(&stories, &issues, "now");

        assert!(mappings.is_empty(), "{mappings:?}");
        assert_eq!(ambiguous.len(), 1);
        assert!(ambiguous[0].contains("2 open issues"), "{}", ambiguous[0]);
    }

    /// **Order-invariant.** The mapping this function returns is a set; the
    /// order `open_stories()` and `list_issues()` happen to return must not
    /// change which pairs link. The previous implementation walked stories in
    /// sequence and excluded issues an earlier story had already claimed, so
    /// permuting the input changed the answer.
    #[test]
    fn the_mapping_does_not_depend_on_input_order() {
        let stories = vec![
            story("SH-1", "Alpha"),
            story("SH-2", "Bravo"),
            story("SH-3", "Charlie"),
            story("SH-4", "Charlie"), // ambiguous pair with issue 3, on purpose
        ];
        let i1 = issue(1, "Alpha");
        let i2 = issue(2, "Bravo");
        let i3 = issue(3, "Charlie");
        let issues = vec![&i1, &i2, &i3];

        let (forward, forward_notes) = handle_match_by_title(&stories, &issues, "now");

        let mut reversed_stories = stories.clone();
        reversed_stories.reverse();
        let mut reversed_issues = issues.clone();
        reversed_issues.reverse();
        let (reversed, reversed_notes) =
            handle_match_by_title(&reversed_stories, &reversed_issues, "now");

        let normalize = |mappings: &[StoryIssueMapping]| {
            let mut pairs: Vec<(String, u64)> = mappings
                .iter()
                .map(|m| (m.story_id.clone(), m.issue_number))
                .collect();
            pairs.sort();
            pairs
        };
        assert_eq!(normalize(&forward), normalize(&reversed));
        assert_eq!(forward_notes.len(), reversed_notes.len());
        assert_eq!(
            normalize(&forward),
            vec![("SH-1".to_string(), 1), ("SH-2".to_string(), 2)],
            "SH-3/SH-4 share a title with issue 3 and must stay unlinked regardless of order"
        );
    }
}
