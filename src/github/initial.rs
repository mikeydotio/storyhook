use std::collections::BTreeMap;
use std::path::Path;

use dialoguer::Select;

use crate::error::AppError;
use crate::storage;

use super::client::GithubClient;
use super::sync_state::{
    GithubSyncConfig, SyncMode, SyncSettings, StoryIssueMapping, detect_github_remote,
    save_sync_config,
};
use super::types::GithubIssue;

/// Read the GitHub token from environment variable.
pub fn get_github_token() -> Result<String, AppError> {
    std::env::var("STORYHOOK_GITHUB_TOKEN").map_err(|_| {
        AppError::GithubAuth(
            "STORYHOOK_GITHUB_TOKEN environment variable is not set.\n\
             Create a GitHub Personal Access Token at https://github.com/settings/tokens\n\
             and export it: export STORYHOOK_GITHUB_TOKEN=ghp_..."
                .to_string(),
        )
    })
}

/// Run the initial sync setup wizard.
/// Called when `story github-sync` is run for the first time (no github-sync.toml exists).
/// Returns the initial config with mappings established.
pub fn run_initial_setup(root: &Path) -> Result<GithubSyncConfig, AppError> {
    // 1. Detect remote
    let github_repo = detect_github_remote(root)?.ok_or_else(|| {
        AppError::Validation(
            "No GitHub remote found. Ensure `git remote origin` points to a GitHub repository."
                .to_string(),
        )
    })?;

    // 2. Validate token
    let token = get_github_token()?;
    let client = GithubClient::new(
        token,
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
    let local_stories = storage::load_all_open_snapshots(root)?;
    eprintln!("Fetching issues from GitHub...");
    let github_issues: Vec<GithubIssue> = client.list_issues(None, "open")?;
    // Filter out pull requests (list_issues already does this, but be defensive)
    let github_issues: Vec<&GithubIssue> =
        github_issues.iter().filter(|i| !i.is_pull_request()).collect();

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

    let now = storage::now();

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
    let mode_options = &[
        "Manual (run `story github-sync` explicitly)",
        "Auto (sync on every story change)",
        "Off (disable sync)",
    ];

    let mode_selection = Select::new()
        .with_prompt("Sync mode")
        .items(mode_options)
        .default(0)
        .interact()
        .map_err(|e| AppError::Usage(format!("selection cancelled: {e}")))?;

    let mode = match mode_selection {
        0 => SyncMode::Manual,
        1 => SyncMode::Auto,
        2 => SyncMode::Off,
        _ => SyncMode::Manual,
    };

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

    save_sync_config(root, &config)?;
    eprintln!();
    eprintln!("Sync config saved to .storyhook/github-sync.toml");

    Ok(config)
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
                if mappings.iter().any(|m: &StoryIssueMapping| m.issue_number == issue.number) {
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
