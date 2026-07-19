pub mod auto;
pub mod body_block;
pub mod client;
pub mod conflict;
pub mod diff;
pub mod field_map;
pub mod initial;
pub mod sync_state;
pub mod types;

use std::path::Path;

use crate::domain::{Member, Priority, StateDef, StoryEvent, StorySnapshot};
use crate::error::AppError;
use crate::output::Response;
use crate::storage;

use self::client::GithubClient;
use self::conflict::{Resolution, ResolvedConflict, resolve_conflicts_interactive};
use self::diff::{FieldConflict, FieldUpdates, MergeResult, three_way_merge};
use self::field_map::{
    RemoteSnapshot, format_comment_for_github, github_comment_to_story, is_sync_generated_comment,
    issue_to_remote_snapshot, story_to_create_request, updates_to_issue_request,
};
use self::initial::{get_github_token, run_initial_setup};
use self::sync_state::{
    GithubSyncConfig, StoryIssueMapping, SyncMode, create_backup, find_mapping,
    find_mapping_by_issue, load_base_snapshot, load_sync_config, save_base_snapshot,
    save_sync_config,
};

// ---------------------------------------------------------------------------
// Sync report
// ---------------------------------------------------------------------------

struct SyncReport {
    pushed: Vec<String>,
    pulled: Vec<String>,
    created_issues: Vec<(String, u64)>,
    created_stories: Vec<(u64, String)>,
    conflicts: Vec<(String, Vec<FieldConflict>)>,
    errors: Vec<(String, String)>,
    skipped: usize,
}

impl SyncReport {
    fn new() -> Self {
        Self {
            pushed: Vec::new(),
            pulled: Vec::new(),
            created_issues: Vec::new(),
            created_stories: Vec::new(),
            conflicts: Vec::new(),
            errors: Vec::new(),
            skipped: 0,
        }
    }

    fn to_message(&self) -> String {
        let mut lines = Vec::new();

        let total_actions = self.pushed.len()
            + self.pulled.len()
            + self.created_issues.len()
            + self.created_stories.len();

        if total_actions == 0 && self.conflicts.is_empty() && self.errors.is_empty() {
            lines.push("GitHub sync complete. Everything is up to date.".to_string());
        } else {
            lines.push("GitHub sync complete.".to_string());
        }

        if !self.pushed.is_empty() {
            lines.push(format!("Pushed {} stories to GitHub:", self.pushed.len()));
            for id in &self.pushed {
                lines.push(format!("  {id}"));
            }
        }

        if !self.pulled.is_empty() {
            lines.push(format!("Pulled {} stories from GitHub:", self.pulled.len()));
            for id in &self.pulled {
                lines.push(format!("  {id}"));
            }
        }

        if !self.created_issues.is_empty() {
            lines.push(format!(
                "Created {} GitHub issues:",
                self.created_issues.len()
            ));
            for (story_id, issue_number) in &self.created_issues {
                lines.push(format!("  {story_id} -> #{issue_number}"));
            }
        }

        if !self.created_stories.is_empty() {
            lines.push(format!(
                "Created {} local stories from GitHub issues:",
                self.created_stories.len()
            ));
            for (issue_number, story_id) in &self.created_stories {
                lines.push(format!("  #{issue_number} -> {story_id}"));
            }
        }

        if !self.conflicts.is_empty() {
            lines.push(format!(
                "Unresolved conflicts on {} stories:",
                self.conflicts.len()
            ));
            for (id, fields) in &self.conflicts {
                let field_names: Vec<&str> = fields.iter().map(|f| f.field.as_str()).collect();
                lines.push(format!("  {id}: {}", field_names.join(", ")));
            }
        }

        if !self.errors.is_empty() {
            lines.push(format!("Errors ({}):", self.errors.len()));
            for (id, msg) in &self.errors {
                lines.push(format!("  {id}: {msg}"));
            }
        }

        if self.skipped > 0 {
            lines.push(format!("Skipped {} stories/issues.", self.skipped));
        }

        lines.join("\n")
    }
}

// ---------------------------------------------------------------------------
// Single-story sync result
// ---------------------------------------------------------------------------

enum SyncStoryResult {
    Pushed,
    Pulled,
    PushedAndPulled,
    UpToDate,
    Conflicts(Vec<FieldConflict>),
}

// ---------------------------------------------------------------------------
// run_sync -- main entry point
// ---------------------------------------------------------------------------

/// Run GitHub sync. If `story_id` is Some, sync only that story.
/// If `dry_run` is true, preview changes without applying them.
pub fn run_sync(root: &Path, story_id: Option<&str>, dry_run: bool) -> Result<Response, AppError> {
    // 1. Load sync config
    let mut config = match load_sync_config(root)? {
        Some(cfg) => {
            if cfg.sync.mode == SyncMode::Off {
                return Err(AppError::Usage(
                    "GitHub sync is disabled. Set sync_mode to 'manual' or 'auto' in \
                     .storyhook/github-sync.toml"
                        .to_string(),
                ));
            }
            cfg
        }
        None => run_initial_setup(root)?,
    };

    // If initial setup returned Off mode, bail
    if config.sync.mode == SyncMode::Off {
        return Ok(Response::Message(
            "GitHub sync is configured as off. No sync performed.".to_string(),
        ));
    }

    // 2. Get token and create client
    let token = get_github_token()?;
    let client = GithubClient::new(
        token,
        config.github.owner.clone(),
        config.github.repo.clone(),
    );

    // 3. Load local context
    let states = storage::load_states(root)?;
    let members = storage::load_members(root)?;
    let prefix = storage::load_project_prefix(root)?;
    let open_stories = storage::load_all_open_snapshots(root)?;

    let mut report = SyncReport::new();

    if dry_run {
        eprintln!("Dry-run mode: no changes will be written.\n");
    }

    // 4. Single-story sync
    if let Some(sid) = story_id {
        let story = open_stories.iter().find(|s| s.id == sid).ok_or_else(|| {
            AppError::NotFound(format!("story `{sid}` not found in open stories"))
        })?;

        match sync_single_story(
            root,
            &client,
            &mut config,
            story,
            &states,
            &members,
            &prefix,
            dry_run,
        ) {
            Ok(result) => record_result(&mut report, &story.id, result),
            Err(e) => report.errors.push((story.id.clone(), e.to_string())),
        }

        if !dry_run {
            config.sync.last_sync_at = Some(storage::now());
            save_sync_config(root, &config)?;
        }

        return Ok(Response::Message(report.to_message()));
    }

    // 5. Full sync

    // Track which stories were already synced during the pull phase
    // so we don't double-process them during the push phase.
    let mut synced_story_ids: Vec<String> = Vec::new();

    // ---- PULL phase: fetch issues changed since last sync ----
    let since = config.sync.last_sync_at.clone();
    eprintln!("Fetching issues from GitHub...");
    let remote_issues = client.list_issues(since.as_deref(), "all")?;
    eprintln!("Fetched {} issues.", remote_issues.len());

    for issue in &remote_issues {
        if issue.is_pull_request() {
            continue;
        }

        if let Some(mapping) = find_mapping_by_issue(&config, issue.number) {
            let story_id_owned = mapping.story_id.clone();

            // Skip placeholder mappings (story_id is empty -- from initial import)
            if story_id_owned.is_empty() {
                // This is a placeholder from initial setup "Import all" -- treat as unmapped
                let remote_snap = issue_to_remote_snapshot(issue, &states, &members, &prefix);

                if dry_run {
                    eprintln!(
                        "Would create local story from issue #{}: \"{}\"",
                        issue.number, issue.title
                    );
                    report
                        .created_stories
                        .push((issue.number, "(dry-run)".to_string()));
                    continue;
                }

                match create_story_from_issue(
                    root,
                    &client,
                    &mut config,
                    issue,
                    &remote_snap,
                    &states,
                    &members,
                    &prefix,
                ) {
                    Ok(new_id) => {
                        synced_story_ids.push(new_id.clone());
                        report.created_stories.push((issue.number, new_id));
                    }
                    Err(e) => {
                        report
                            .errors
                            .push((format!("#{}", issue.number), e.to_string()));
                    }
                }
                continue;
            }

            // Check if the mapped story still exists locally
            let story = match open_stories.iter().find(|s| s.id == story_id_owned) {
                Some(s) => s,
                None => {
                    report.skipped += 1;
                    continue;
                }
            };

            match sync_single_story(
                root,
                &client,
                &mut config,
                story,
                &states,
                &members,
                &prefix,
                dry_run,
            ) {
                Ok(result) => {
                    synced_story_ids.push(story.id.clone());
                    record_result(&mut report, &story.id, result);
                }
                Err(e) => {
                    synced_story_ids.push(story.id.clone());
                    report.errors.push((story.id.clone(), e.to_string()));
                }
            }
        } else {
            // Unmapped issue: create local story from GitHub issue
            let remote_snap = issue_to_remote_snapshot(issue, &states, &members, &prefix);

            // If the issue body has a storyhook block referencing a story that already
            // exists locally, skip it to avoid duplicates.
            if let Some(ref sid) = remote_snap.story_id
                && open_stories.iter().any(|s| s.id == *sid)
            {
                report.skipped += 1;
                continue;
            }

            if dry_run {
                eprintln!(
                    "Would create local story from issue #{}: \"{}\"",
                    issue.number, issue.title
                );
                report
                    .created_stories
                    .push((issue.number, "(dry-run)".to_string()));
                continue;
            }

            match create_story_from_issue(
                root,
                &client,
                &mut config,
                issue,
                &remote_snap,
                &states,
                &members,
                &prefix,
            ) {
                Ok(new_id) => {
                    synced_story_ids.push(new_id.clone());
                    report.created_stories.push((issue.number, new_id));
                }
                Err(e) => {
                    report
                        .errors
                        .push((format!("#{}", issue.number), e.to_string()));
                }
            }
        }
    }

    // ---- PUSH phase: push local stories not yet on GitHub ----
    for story in &open_stories {
        // Skip stories already synced in the pull phase
        if synced_story_ids.contains(&story.id) {
            continue;
        }

        if find_mapping(&config, &story.id).is_some() {
            // Mapped but not in pull results -- check for local-only changes to push
            match sync_single_story(
                root,
                &client,
                &mut config,
                story,
                &states,
                &members,
                &prefix,
                dry_run,
            ) {
                Ok(result) => record_result(&mut report, &story.id, result),
                Err(e) => report.errors.push((story.id.clone(), e.to_string())),
            }
        } else {
            // Unmapped local story: create GitHub issue
            if dry_run {
                eprintln!(
                    "Would create GitHub issue for story {}: \"{}\"",
                    story.id, story.title
                );
                report.created_issues.push((story.id.clone(), 0));
                continue;
            }

            match create_issue_from_story(
                root,
                &client,
                &mut config,
                story,
                &states,
                &members,
                &prefix,
            ) {
                Ok(issue_number) => {
                    report.created_issues.push((story.id.clone(), issue_number));
                }
                Err(e) => {
                    report.errors.push((story.id.clone(), e.to_string()));
                }
            }
        }
    }

    // 6. Update sync state
    if !dry_run {
        let now = storage::now();
        config.sync.last_sync_at = Some(now.clone());
        config.sync.last_full_sync_at = Some(now);
        save_sync_config(root, &config)?;
    }

    Ok(Response::Message(report.to_message()))
}

// ---------------------------------------------------------------------------
// sync_single_story
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn sync_single_story(
    root: &Path,
    client: &GithubClient,
    config: &mut GithubSyncConfig,
    story: &StorySnapshot,
    states: &[StateDef],
    members: &[Member],
    prefix: &str,
    dry_run: bool,
) -> Result<SyncStoryResult, AppError> {
    let mapping = match find_mapping(config, &story.id) {
        Some(m) => m.clone(),
        None => {
            // No mapping -- nothing to sync for an existing story in this helper.
            return Ok(SyncStoryResult::UpToDate);
        }
    };

    // Fetch the current remote issue
    let issue = client.get_issue(mapping.issue_number)?;
    let remote_snap = issue_to_remote_snapshot(&issue, states, members, prefix);

    // Build a StorySnapshot from the remote data for diffing
    let remote_as_story = remote_snapshot_to_story_snapshot(&remote_snap, &issue, &story.id);

    // Load base snapshot (falls back to current local state for first sync)
    let base = load_base_snapshot(root, &story.id)?.unwrap_or_else(|| story.clone());

    // Fetch remote comments
    let remote_comments = client.list_comments(mapping.issue_number, None)?;
    let remote_story_comments: Vec<crate::domain::StoryComment> = remote_comments
        .iter()
        .map(github_comment_to_story)
        .collect();

    // Attach remote comments for merge
    let mut remote_for_merge = remote_as_story;
    remote_for_merge.comments = remote_story_comments;

    // Three-way merge
    let merge_result = three_way_merge(&base, story, &remote_for_merge);

    // Check if there's anything to do
    let has_local_updates = !merge_result.local_updates.is_empty();
    let has_remote_updates = !merge_result.remote_updates.is_empty();
    let has_conflicts = !merge_result.conflicts.is_empty();
    let has_new_local_comments = !merge_result.new_local_comments.is_empty();
    let has_new_remote_comments = !merge_result.new_remote_comments.is_empty();

    if !has_local_updates
        && !has_remote_updates
        && !has_conflicts
        && !has_new_local_comments
        && !has_new_remote_comments
    {
        return Ok(SyncStoryResult::UpToDate);
    }

    if dry_run {
        print_dry_run_preview(&story.id, &merge_result);
        if has_conflicts {
            return Ok(SyncStoryResult::Conflicts(merge_result.conflicts));
        }
        if has_local_updates || has_new_remote_comments {
            if has_remote_updates || has_new_local_comments {
                return Ok(SyncStoryResult::PushedAndPulled);
            }
            return Ok(SyncStoryResult::Pulled);
        }
        return Ok(SyncStoryResult::Pushed);
    }

    // Handle conflicts
    let mut resolved_conflicts: Vec<ResolvedConflict> = Vec::new();
    let mut unresolved_conflicts: Vec<FieldConflict> = Vec::new();

    if has_conflicts {
        let resolutions = resolve_conflicts_interactive(&story.id, &merge_result.conflicts);
        for (conflict, resolved) in merge_result.conflicts.iter().zip(resolutions.iter()) {
            match resolved.resolution {
                Resolution::Skip => unresolved_conflicts.push(conflict.clone()),
                _ => resolved_conflicts.push(resolved.clone()),
            }
        }
    }

    // Apply local updates (pull from remote)
    let mut did_pull = false;
    if has_local_updates || has_new_remote_comments {
        create_backup(root, &story.id)?;
        apply_local_updates(root, &story.id, &merge_result.local_updates)?;

        // Apply resolved conflicts that chose KeepRemote
        for rc in &resolved_conflicts {
            if matches!(rc.resolution, Resolution::KeepRemote) {
                apply_conflict_locally(root, &story.id, rc, &merge_result.conflicts)?;
            }
        }

        // Import new remote comments
        for comment in &merge_result.new_remote_comments {
            let event = StoryEvent::StoryCommentAdded {
                at: storage::now(),
                text: format!("[github] {}", comment.text),
            };
            storage::write_story_events(root, &story.id, &[event])?;
        }

        did_pull = true;
    }

    // Apply remote updates (push to GitHub)
    let mut did_push = false;
    if has_remote_updates || has_new_local_comments {
        // Build the update request for the issue
        if has_remote_updates {
            let update_req =
                updates_to_issue_request(&merge_result.remote_updates, story, members, states);
            client.update_issue(mapping.issue_number, &update_req)?;
        }

        // Apply resolved conflicts that chose KeepLocal (push to remote)
        for rc in &resolved_conflicts {
            if matches!(rc.resolution, Resolution::KeepLocal) {
                apply_conflict_remotely(
                    client,
                    &mapping,
                    rc,
                    &merge_result.conflicts,
                    story,
                    members,
                    states,
                )?;
            }
        }

        // Post new local comments to GitHub
        for comment in &merge_result.new_local_comments {
            if !is_sync_generated_comment(&comment.text) {
                let formatted = format_comment_for_github(comment);
                client.create_comment(mapping.issue_number, &formatted)?;
            }
        }

        did_push = true;
    }

    // Also handle conflict resolutions when there are no other updates
    if !did_pull && !did_push && !resolved_conflicts.is_empty() {
        for rc in &resolved_conflicts {
            match rc.resolution {
                Resolution::KeepRemote => {
                    create_backup(root, &story.id)?;
                    apply_conflict_locally(root, &story.id, rc, &merge_result.conflicts)?;
                    did_pull = true;
                }
                Resolution::KeepLocal => {
                    apply_conflict_remotely(
                        client,
                        &mapping,
                        rc,
                        &merge_result.conflicts,
                        story,
                        members,
                        states,
                    )?;
                    did_push = true;
                }
                Resolution::Skip => {}
            }
        }
    }

    // Save base snapshot for next sync
    let updated_story = storage::load_open_story_snapshot(root, &story.id)?;
    save_base_snapshot(root, &story.id, &updated_story)?;

    // Update mapping's last_synced_at
    update_mapping_timestamp(config, &story.id);

    if !unresolved_conflicts.is_empty() {
        return Ok(SyncStoryResult::Conflicts(unresolved_conflicts));
    }

    match (did_push, did_pull) {
        (true, true) => Ok(SyncStoryResult::PushedAndPulled),
        (true, false) => Ok(SyncStoryResult::Pushed),
        (false, true) => Ok(SyncStoryResult::Pulled),
        (false, false) => Ok(SyncStoryResult::UpToDate),
    }
}

// ---------------------------------------------------------------------------
// Create story from GitHub issue
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn create_story_from_issue(
    root: &Path,
    client: &GithubClient,
    config: &mut GithubSyncConfig,
    issue: &types::GithubIssue,
    remote_snap: &RemoteSnapshot,
    states: &[StateDef],
    _members: &[Member],
    _prefix: &str,
) -> Result<String, AppError> {
    // Create the story via storage
    let snapshot = storage::create_story(root, &issue.title, None)?;
    let story_id = snapshot.id.clone();

    // Append events for additional fields
    let mut events = Vec::new();
    let now = storage::now();

    if remote_snap.state != snapshot.state {
        events.push(StoryEvent::StoryStateChanged {
            at: now.clone(),
            state: remote_snap.state.clone(),
        });
    }

    if let Some(ref assignee) = remote_snap.assignee {
        events.push(StoryEvent::StoryAssigned {
            at: now.clone(),
            member_id: assignee.clone(),
        });
    }

    if remote_snap.priority != Priority::None {
        events.push(StoryEvent::StoryPrioritySet {
            at: now.clone(),
            priority: remote_snap.priority.clone(),
        });
    }

    if let Some(ref awaiting) = remote_snap.awaiting {
        events.push(StoryEvent::StoryAwaitingSet {
            at: now.clone(),
            awaiting: awaiting.clone(),
        });
    }

    if !remote_snap.labels.is_empty() {
        events.push(StoryEvent::StoryLabelsSet {
            at: now.clone(),
            labels: remote_snap.labels.clone(),
        });
    }

    if !events.is_empty() {
        storage::write_story_events(root, &story_id, &events)?;
    }

    // Import comments from the issue
    let comments = client.list_comments(issue.number, None)?;
    for comment in &comments {
        if !comment.body.starts_with("[storyhook]") {
            let sc = github_comment_to_story(comment);
            storage::write_story_events(
                root,
                &story_id,
                &[StoryEvent::StoryCommentAdded {
                    at: sc.at,
                    text: sc.text,
                }],
            )?;
        }
    }

    // Update the issue body with the storyhook block so future syncs can find the mapping
    let story_for_block = storage::load_open_story_snapshot(root, &story_id)?;
    let block = field_map::story_to_block(&story_for_block);
    let body_text = issue.body.as_deref().unwrap_or("");
    let clean_body = match body_block::extract_block(body_text) {
        Some((text, _)) => text,
        None => body_text.to_string(),
    };
    let new_body = body_block::render_block(&clean_body, &block);
    let update_req = types::UpdateIssueRequest {
        body: Some(new_body),
        ..Default::default()
    };
    client.update_issue(issue.number, &update_req)?;

    // Save base snapshot
    let final_snapshot = storage::load_open_story_snapshot(root, &story_id)?;
    save_base_snapshot(root, &story_id, &final_snapshot)?;

    // Add or update mapping
    let mapping_idx = config
        .mappings
        .iter()
        .position(|m| m.issue_number == issue.number);

    let new_mapping = StoryIssueMapping {
        story_id: story_id.clone(),
        issue_number: issue.number,
        last_synced_at: storage::now(),
        last_local_event_index: None,
    };

    if let Some(idx) = mapping_idx {
        config.mappings[idx] = new_mapping;
    } else {
        config.mappings.push(new_mapping);
    }

    // If the issue is closed, transition the story to a closed state
    if remote_snap.superstate == crate::domain::SuperState::Closed
        && let Some(closed_state) = states
            .iter()
            .find(|s| s.super_state == crate::domain::SuperState::Closed)
    {
        storage::write_story_events(
            root,
            &story_id,
            &[StoryEvent::StoryStateChanged {
                at: storage::now(),
                state: closed_state.slug.clone(),
            }],
        )?;
    }

    Ok(story_id)
}

// ---------------------------------------------------------------------------
// Create GitHub issue from local story
// ---------------------------------------------------------------------------

fn create_issue_from_story(
    root: &Path,
    client: &GithubClient,
    config: &mut GithubSyncConfig,
    story: &StorySnapshot,
    _states: &[StateDef],
    members: &[Member],
    _prefix: &str,
) -> Result<u64, AppError> {
    let create_req = story_to_create_request(story, members, None);
    let created_issue = client.create_issue(&create_req)?;

    // Save base snapshot
    save_base_snapshot(root, &story.id, story)?;

    // Add mapping
    config.mappings.push(StoryIssueMapping {
        story_id: story.id.clone(),
        issue_number: created_issue.number,
        last_synced_at: storage::now(),
        last_local_event_index: None,
    });

    // Post any existing non-sync comments to GitHub
    for comment in &story.comments {
        if !is_sync_generated_comment(&comment.text) {
            let formatted = format_comment_for_github(comment);
            client.create_comment(created_issue.number, &formatted)?;
        }
    }

    Ok(created_issue.number)
}

// ---------------------------------------------------------------------------
// Apply local updates from merge
// ---------------------------------------------------------------------------

fn apply_local_updates(
    root: &Path,
    story_id: &str,
    updates: &FieldUpdates,
) -> Result<(), AppError> {
    let mut events = Vec::new();
    let now = storage::now();

    if let Some(ref title) = updates.title {
        events.push(StoryEvent::StoryTitleSet {
            at: now.clone(),
            title: title.clone(),
        });
    }

    if let Some(ref state) = updates.state {
        events.push(StoryEvent::StoryStateChanged {
            at: now.clone(),
            state: state.clone(),
        });
    }

    if let Some(Some(member_id)) = &updates.assignee {
        // Note: the domain has no "unassign" event, so clearing an assignee
        // cannot be expressed. We skip the None case.
        events.push(StoryEvent::StoryAssigned {
            at: now.clone(),
            member_id: member_id.clone(),
        });
    }

    if let Some(ref priority) = updates.priority {
        events.push(StoryEvent::StoryPrioritySet {
            at: now.clone(),
            priority: priority.clone(),
        });
    }

    if let Some(ref awaiting_opt) = updates.awaiting {
        match awaiting_opt {
            Some(awaiting) => {
                events.push(StoryEvent::StoryAwaitingSet {
                    at: now.clone(),
                    awaiting: awaiting.clone(),
                });
            }
            None => {
                events.push(StoryEvent::StoryAwaitingCleared { at: now.clone() });
            }
        }
    }

    if let Some(ref labels) = updates.labels {
        events.push(StoryEvent::StoryLabelsSet {
            at: now.clone(),
            labels: labels.clone(),
        });
    }

    if !events.is_empty() {
        storage::write_story_events(root, story_id, &events)?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Apply resolved conflict locally (KeepRemote)
// ---------------------------------------------------------------------------

fn apply_conflict_locally(
    root: &Path,
    story_id: &str,
    resolved: &ResolvedConflict,
    conflicts: &[FieldConflict],
) -> Result<(), AppError> {
    let conflict = match conflicts.iter().find(|c| c.field == resolved.field) {
        Some(c) => c,
        None => return Ok(()),
    };

    let now = storage::now();
    let event = match conflict.field.as_str() {
        "title" => StoryEvent::StoryTitleSet {
            at: now,
            title: conflict.remote_value.clone(),
        },
        "state" => StoryEvent::StoryStateChanged {
            at: now,
            state: conflict.remote_value.clone(),
        },
        "assignee" => {
            if conflict.remote_value == "<none>" {
                return Ok(());
            }
            StoryEvent::StoryAssigned {
                at: now,
                member_id: conflict.remote_value.clone(),
            }
        }
        "priority" => {
            let priority = Priority::parse(&conflict.remote_value).unwrap_or(Priority::None);
            StoryEvent::StoryPrioritySet { at: now, priority }
        }
        "awaiting" => {
            if conflict.remote_value == "<none>" {
                StoryEvent::StoryAwaitingCleared { at: now }
            } else {
                StoryEvent::StoryAwaitingSet {
                    at: now,
                    awaiting: conflict.remote_value.clone(),
                }
            }
        }
        "labels" => {
            let labels: Vec<String> = conflict
                .remote_value
                .split(", ")
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();
            StoryEvent::StoryLabelsSet { at: now, labels }
        }
        _ => return Ok(()),
    };

    storage::write_story_events(root, story_id, &[event])
}

// ---------------------------------------------------------------------------
// Apply resolved conflict remotely (KeepLocal)
// ---------------------------------------------------------------------------

fn apply_conflict_remotely(
    client: &GithubClient,
    mapping: &StoryIssueMapping,
    resolved: &ResolvedConflict,
    conflicts: &[FieldConflict],
    story: &StorySnapshot,
    members: &[Member],
    states: &[StateDef],
) -> Result<(), AppError> {
    let conflict = match conflicts.iter().find(|c| c.field == resolved.field) {
        Some(c) => c,
        None => return Ok(()),
    };

    let mut updates = FieldUpdates::default();
    match conflict.field.as_str() {
        "title" => updates.title = Some(conflict.local_value.clone()),
        "state" => updates.state = Some(conflict.local_value.clone()),
        "assignee" => {
            if conflict.local_value == "<none>" {
                updates.assignee = Some(None);
            } else {
                updates.assignee = Some(Some(conflict.local_value.clone()));
            }
        }
        "priority" => {
            let priority = Priority::parse(&conflict.local_value).unwrap_or(Priority::None);
            updates.priority = Some(priority);
        }
        "awaiting" => {
            if conflict.local_value == "<none>" {
                updates.awaiting = Some(None);
            } else {
                updates.awaiting = Some(Some(conflict.local_value.clone()));
            }
        }
        "labels" => {
            let labels: Vec<String> = conflict
                .local_value
                .split(", ")
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();
            updates.labels = Some(labels);
        }
        _ => return Ok(()),
    }

    let update_req = updates_to_issue_request(&updates, story, members, states);
    client.update_issue(mapping.issue_number, &update_req)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helper: convert RemoteSnapshot to StorySnapshot for diffing
// ---------------------------------------------------------------------------

fn remote_snapshot_to_story_snapshot(
    remote: &RemoteSnapshot,
    issue: &types::GithubIssue,
    story_id: &str,
) -> StorySnapshot {
    StorySnapshot {
        id: story_id.to_string(),
        title: remote.title.clone(),
        created_at: issue.created_at.clone(),
        updated_at: issue.updated_at.clone(),
        state: remote.state.clone(),
        superstate: remote.superstate.clone(),
        assignee: remote.assignee.clone(),
        awaiting: remote.awaiting.clone(),
        comments: Vec::new(), // Comments handled separately
        relationships: remote.non_native_relationships.clone(),
        priority: remote.priority.clone(),
        labels: remote.labels.clone(),
        story_type: None,
        closed_at: issue.closed_at.clone(),
        // GitHub issues have no notion of storyhook's soft-delete; a synced
        // remote issue is never considered "deleted" for diffing purposes.
        deleted: false,
        deleted_reason: None,
    }
}

// ---------------------------------------------------------------------------
// Helper: update mapping timestamp
// ---------------------------------------------------------------------------

fn update_mapping_timestamp(config: &mut GithubSyncConfig, story_id: &str) {
    let now = storage::now();
    if let Some(m) = config.mappings.iter_mut().find(|m| m.story_id == story_id) {
        m.last_synced_at = now;
    }
}

// ---------------------------------------------------------------------------
// Helper: record sync result into report
// ---------------------------------------------------------------------------

fn record_result(report: &mut SyncReport, story_id: &str, result: SyncStoryResult) {
    match result {
        SyncStoryResult::Pushed => report.pushed.push(story_id.to_string()),
        SyncStoryResult::Pulled => report.pulled.push(story_id.to_string()),
        SyncStoryResult::PushedAndPulled => {
            report.pushed.push(story_id.to_string());
            report.pulled.push(story_id.to_string());
        }
        SyncStoryResult::UpToDate => {}
        SyncStoryResult::Conflicts(conflicts) => {
            report.conflicts.push((story_id.to_string(), conflicts));
        }
    }
}

// ---------------------------------------------------------------------------
// Dry-run preview
// ---------------------------------------------------------------------------

fn print_dry_run_preview(story_id: &str, merge: &MergeResult) {
    if !merge.local_updates.is_empty() {
        eprintln!("{story_id}: would pull from GitHub:");
        print_field_updates("  ", &merge.local_updates);
    }
    if !merge.remote_updates.is_empty() {
        eprintln!("{story_id}: would push to GitHub:");
        print_field_updates("  ", &merge.remote_updates);
    }
    if !merge.conflicts.is_empty() {
        eprintln!("{story_id}: conflicts:");
        for c in &merge.conflicts {
            eprintln!(
                "  {}: base=\"{}\" local=\"{}\" remote=\"{}\"",
                c.field, c.base_value, c.local_value, c.remote_value
            );
        }
    }
    if !merge.new_local_comments.is_empty() {
        eprintln!(
            "{story_id}: would push {} comments to GitHub",
            merge.new_local_comments.len()
        );
    }
    if !merge.new_remote_comments.is_empty() {
        eprintln!(
            "{story_id}: would pull {} comments from GitHub",
            merge.new_remote_comments.len()
        );
    }
}

fn print_field_updates(indent: &str, updates: &FieldUpdates) {
    if let Some(ref title) = updates.title {
        eprintln!("{indent}title -> \"{title}\"");
    }
    if let Some(ref state) = updates.state {
        eprintln!("{indent}state -> \"{state}\"");
    }
    if let Some(ref assignee) = updates.assignee {
        match assignee {
            Some(id) => eprintln!("{indent}assignee -> \"{id}\""),
            None => eprintln!("{indent}assignee -> (cleared)"),
        }
    }
    if let Some(ref priority) = updates.priority {
        eprintln!("{indent}priority -> \"{}\"", priority.as_str());
    }
    if let Some(ref awaiting) = updates.awaiting {
        match awaiting {
            Some(a) => eprintln!("{indent}awaiting -> \"{a}\""),
            None => eprintln!("{indent}awaiting -> (cleared)"),
        }
    }
    if let Some(ref labels) = updates.labels {
        eprintln!("{indent}labels -> [{}]", labels.join(", "));
    }
}
