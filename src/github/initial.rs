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
        1 => handle_match_by_title(&local_stories, &github_issues, &now)?,
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

/// Match local stories to GitHub issues by title (case-insensitive contains).
fn handle_match_by_title(
    stories: &[crate::domain::StorySnapshot],
    issues: &[&GithubIssue],
    now: &str,
) -> Result<Vec<StoryIssueMapping>, AppError> {
    let mut mappings = Vec::new();
    let mut skip_all = false;

    for story in stories {
        if skip_all {
            break;
        }

        let story_title_lower = story.title.to_lowercase();

        // Find issues whose titles share a substring match (case-insensitive)
        let candidates: Vec<&&GithubIssue> = issues
            .iter()
            .filter(|issue| {
                // Skip issues already mapped
                if mappings
                    .iter()
                    .any(|m: &StoryIssueMapping| m.issue_number == issue.number)
                {
                    return false;
                }
                let issue_title_lower = issue.title.to_lowercase();
                issue_title_lower.contains(&story_title_lower)
                    || story_title_lower.contains(&issue_title_lower)
            })
            .collect();

        if candidates.is_empty() {
            continue;
        }

        for candidate in &candidates {
            if skip_all {
                break;
            }

            eprintln!();
            eprintln!(
                "  {} \"{}\" <-> #{} \"{}\"",
                story.id, story.title, candidate.number, candidate.title
            );

            let options = &["Yes", "No", "Skip all"];
            let selection = Select::new()
                .with_prompt("Link these?")
                .items(options)
                .default(0)
                .interact()
                .map_err(|e| AppError::Usage(format!("selection cancelled: {e}")))?;

            match selection {
                0 => {
                    mappings.push(StoryIssueMapping {
                        story_id: story.id.clone(),
                        issue_number: candidate.number,
                        last_synced_at: now.to_string(),
                        last_local_event_index: None,
                    });
                    break; // Move to next story after a match
                }
                2 => {
                    skip_all = true;
                }
                _ => {} // No — try next candidate
            }
        }
    }

    Ok(mappings)
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
}
