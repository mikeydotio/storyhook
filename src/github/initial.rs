use std::collections::BTreeMap;

use dialoguer::Select;

use crate::domain::secret::{self, GithubToken};
use crate::error::AppError;

use super::client::GithubClient;
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

/// Run the initial sync setup wizard.
/// Called when `story github-sync` is run for the first time (no github-sync.toml exists).
/// Returns the initial config with mappings established.
pub fn run_initial_setup(
    sync: &dyn SyncStorage,
    token: Option<&GithubToken>,
) -> Result<GithubSyncConfig, AppError> {
    // 1. Detect remote
    let github_repo = detect_github_remote(sync.root())?.ok_or_else(|| {
        AppError::Validation(
            "No GitHub remote found. Ensure `git remote origin` points to a GitHub repository."
                .to_string(),
        )
    })?;

    // 2. Validate token
    let token = require_github_token(token)?;
    let client = GithubClient::new(
        token.expose().to_string(),
        github_repo.owner.clone(),
        github_repo.repo.clone(),
    );
    eprintln!(
        "Validating token for {}/{}...",
        github_repo.owner, github_repo.repo
    );
    client.validate_token()?;
    eprintln!("Token validated.");

    // 3. Scan both sides
    let local_stories = sync.open_stories()?;
    eprintln!("Fetching issues from GitHub...");
    let github_issues: Vec<GithubIssue> = client.list_issues(None, "open")?;
    // Filter out pull requests (list_issues already does this, but be defensive)
    let github_issues: Vec<&GithubIssue> = github_issues
        .iter()
        .filter(|i| !i.is_pull_request())
        .collect();

    eprintln!();
    eprintln!(
        "Found {} local stories and {} open GitHub issues.",
        local_stories.len(),
        github_issues.len()
    );
    eprintln!();

    // 4. Present initial sync strategy choices
    let strategy_options = &[
        "Import all open issues from GitHub",
        "Match stories to issues by title",
        "Push local stories to GitHub only",
        "Start fresh (sync only future changes)",
    ];

    let strategy = Select::new()
        .with_prompt("How would you like to handle the initial sync?")
        .items(strategy_options)
        .default(0)
        .interact()
        .map_err(|e| AppError::Usage(format!("selection cancelled: {e}")))?;

    let now = sync.now();

    // 5. Handle each choice
    let mappings = match strategy {
        0 => handle_import_all(&github_issues, &now),
        1 => {
            let (mappings, ambiguous) = handle_match_by_title(&local_stories, &github_issues, &now);
            for note in &ambiguous {
                eprintln!("  {note}");
            }
            mappings
        }
        2 => Vec::new(), // Push only: empty mappings, orchestrator creates issues later
        3 => Vec::new(), // Start fresh: empty mappings
        _ => Vec::new(),
    };

    let last_sync_at = if strategy == 3 {
        Some(now.clone())
    } else {
        None
    };

    // 6. Ask sync mode
    let mode_selection = Select::new()
        .with_prompt("Sync mode")
        .items(MODE_OPTIONS)
        .default(0)
        .interact()
        .map_err(|e| AppError::Usage(format!("selection cancelled: {e}")))?;

    let mode = mode_for_selection(mode_selection);

    // 7. Save config and return
    let config = GithubSyncConfig {
        github: github_repo,
        sync: SyncSettings {
            mode,
            last_sync_at,
            last_full_sync_at: None,
        },
        etags: BTreeMap::new(),
        mappings,
    };

    sync.save_config(&config)?;
    eprintln!();
    eprintln!("Sync config saved.");

    Ok(config)
}

/// The sync modes this build can actually honour.
///
/// **`auto` is deliberately absent.** It used to be here, and picking it
/// configured a project for a behaviour that no longer exists: auto-sync fired
/// from the tail of the pre-rearchitecture `app::run`, was never given an
/// equivalent on the invoker, and was deleted with the rest of the legacy write
/// path. Offering a choice that silently does nothing is worse than not
/// offering it — the same rule that made `story web register --name` silently
/// dropping its argument a defect rather than a quirk.
///
/// Reinstating it is a feature rather than a repair: an honest auto-sync makes a
/// network call to GitHub on the tail of every story-modifying command, in the
/// daemon as well as locally, and needs a failure policy, a timeout and a
/// re-entrancy story before any of that is safe. SH-68 carries that design.
const MODE_OPTIONS: &[&str] = &[
    "Manual (run `story github-sync` explicitly)",
    "Off (disable sync)",
];

/// The mode a menu selection means.
///
/// Out-of-range selections fall back to manual, which is `SyncMode`'s own
/// default and the least surprising answer to a question that cannot be
/// answered.
fn mode_for_selection(selection: usize) -> SyncMode {
    match selection {
        1 => SyncMode::Off,
        _ => SyncMode::Manual,
    }
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

    /// The menu must not offer a mode this build does not honour. Picking it
    /// configured a project for nothing at all, silently — which is the same
    /// shape as a flag that is accepted and dropped.
    #[test]
    fn the_setup_menu_offers_no_mode_that_does_nothing() {
        assert_eq!(MODE_OPTIONS.len(), 2);
        for label in MODE_OPTIONS {
            assert!(
                !label.to_lowercase().contains("auto"),
                "the menu must not offer auto while nothing implements it: {label}"
            );
        }
        for selection in 0..MODE_OPTIONS.len() {
            assert_ne!(
                mode_for_selection(selection),
                SyncMode::Auto,
                "no selection may produce a mode nothing acts on"
            );
        }
    }

    /// Every label must map to the mode it describes, and the mapping is by
    /// index — so a reordered menu that silently swapped `manual` and `off`
    /// would turn a user's "off" into live syncing.
    #[test]
    fn every_menu_position_means_what_it_says() {
        assert!(MODE_OPTIONS[0].starts_with("Manual"));
        assert_eq!(mode_for_selection(0), SyncMode::Manual);
        assert!(MODE_OPTIONS[1].starts_with("Off"));
        assert_eq!(mode_for_selection(1), SyncMode::Off);
        assert_eq!(
            mode_for_selection(99),
            SyncMode::Manual,
            "an impossible selection falls back to SyncMode's own default"
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
            relationships: Vec::new(),
            priority: crate::domain::Priority::None,
            labels: Vec::new(),
            story_type: None,
            description: None,
            closed_at: None,
            deleted: false,
            deleted_reason: None,
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
