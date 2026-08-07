pub mod body_block;
pub mod client;
pub mod conflict;
pub mod diff;
pub mod field_map;
pub mod initial;
pub mod storage;
pub mod sync_state;
pub mod types;

use crate::domain::{Member, Priority, StateDef, StoryEvent, StorySnapshot, normalize_labels};
use crate::error::AppError;
use crate::output::Response;

use self::client::GithubClient;
use self::conflict::{Resolution, ResolvedConflict, resolve_conflicts_batch};
use self::diff::{
    ConflictField, FieldConflict, FieldUpdates, MergeResult, base_after_sync, three_way_merge,
};
use self::field_map::{
    RemoteSnapshot, format_comment_for_github, github_comment_to_story, is_sync_generated_comment,
    issue_to_remote_snapshot, story_to_create_request, updates_to_issue_request,
};
use self::initial::{InitialSetupOutcome, SetupAnswers, require_github_token, run_initial_setup};
use self::storage::SyncStorage;
use self::sync_state::{
    GithubSyncConfig, StoryIssueMapping, SyncMode, find_mapping, find_mapping_by_issue,
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
    /// Ambiguous match-by-title pairs from initial setup, found and left
    /// unlinked. Carried here rather than printed, because this run may be
    /// happening in the daemon, where `eprintln!` reaches a log nobody reads
    /// (SH-153's D3).
    setup_notes: Vec<String>,
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
            setup_notes: Vec::new(),
        }
    }

    /// What this run answers with.
    ///
    /// **A conflict is a failure, not a footnote to a success.** It used to be
    /// the latter: the report named the conflicting stories inside a message
    /// headed "GitHub sync complete." and the command exited 0, so a script
    /// could not tell a sync that applied everything from one that applied
    /// everything except the edits somebody actually disagreed about.
    /// [`AppError::SyncConflict`] carries the same text out through
    /// `render_error`, which exits 8, answers HTTP 409, and — unlike a
    /// `Response` — cannot be erased by `--quiet`.
    fn outcome(&self) -> Result<Response, AppError> {
        if self.conflicts.is_empty() {
            return Ok(Response::Message(self.to_message()));
        }
        Err(AppError::SyncConflict(self.to_message()))
    }

    fn to_message(&self) -> String {
        let mut lines = Vec::new();

        let total_actions = self.pushed.len()
            + self.pulled.len()
            + self.created_issues.len()
            + self.created_stories.len();

        if !self.conflicts.is_empty() {
            lines.push(format!(
                "GitHub sync ran, and left {} undecided.",
                count(
                    self.conflicts.len(),
                    "conflicting story",
                    "conflicting stories"
                )
            ));
        } else if total_actions == 0 && self.errors.is_empty() {
            lines.push("GitHub sync complete. Everything is up to date.".to_string());
        } else {
            lines.push("GitHub sync complete.".to_string());
        }

        if !self.setup_notes.is_empty() {
            lines.push(
                "Initial setup: some titles matched more than one candidate and were \
                         left unlinked:"
                    .to_string(),
            );
            for note in &self.setup_notes {
                lines.push(format!("  {note}"));
            }
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
            lines.push("Unresolved conflicts — both sides changed these:".to_string());
            for (id, fields) in &self.conflicts {
                for conflict in fields {
                    lines.push(format!("  {id}, {}:", conflict.field));
                    lines.push(format!("    base:   \"{}\"", conflict.base_value));
                    lines.push(format!("    local:  \"{}\"", conflict.local_value));
                    lines.push(format!("    remote: \"{}\"", conflict.remote_value));
                }
            }
            // The values are the part a caller cannot reconstruct, and the part
            // that used to be dropped: they were printed to a stdout the daemon
            // sends to /dev/null, while only the field *names* travelled back.
            let example = self
                .conflicts
                .first()
                .map(|(id, _)| id.as_str())
                .unwrap_or("<id>");
            lines.push(String::new());
            lines.push("Nothing was chosen for you. Decide each story with one of:".to_string());
            lines.push(format!(
                "  story github-sync {example} --resolve local    (keep your values, push them to GitHub)"
            ));
            lines.push(format!(
                "  story github-sync {example} --resolve remote   (take GitHub's values)"
            ));
            lines.push(
                "Or set the field to the value you want on both sides and re-run: a value the \
                 two sides agree on stops being a conflict."
                    .to_string(),
            );
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

/// `1 conflicting story` / `3 conflicting stories`.
fn count(n: usize, singular: &str, plural: &str) -> String {
    format!("{n} {}", if n == 1 { singular } else { plural })
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

/// What to tell a user whose project is configured for a mode this build does
/// not implement.
///
/// Only `auto` qualifies, and only because it once did. It fired from the tail
/// of the pre-rearchitecture `app::run`, was never given an equivalent on the
/// invoker, and was deleted with the rest of the legacy write path — so a
/// project migrated from before then can still carry it. The variant stays
/// deserializable deliberately: refusing to parse the document would make the
/// project unreadable over a setting that is merely inert, which trades a
/// cosmetic problem for a real one.
///
/// What must not happen is silence. A setting that is accepted and ignored is
/// the defect class this rearchitecture spent a wave removing.
fn unimplemented_mode_notice(mode: &SyncMode) -> Option<String> {
    match mode {
        SyncMode::Auto => Some(
            "note: this project is configured with sync mode `auto`, which storyhook no \
             longer implements — nothing syncs on its own. It is being treated as `manual`. \
             Re-run `story github-sync` to choose a mode this build honours."
                .to_string(),
        ),
        SyncMode::Manual | SyncMode::Off => None,
    }
}

// ---------------------------------------------------------------------------
// run_sync -- main entry point
// ---------------------------------------------------------------------------

/// Runs GitHub sync against whatever storage `sync` names. If `story_id` is
/// `Some`, syncs only that story; if `dry_run`, previews without writing.
///
/// `resolve` is the answer to every conflict this run meets, and `None` means
/// nobody has given one. There is deliberately no way to say "guess": the work
/// runs in a daemon that cannot ask, so an unanswered conflict comes back in a
/// refusal rather than being decided here (SH-152).
///
/// `strategy` and `mode` answer the initial-setup questions on an unconfigured
/// project, in advance — SH-153's D2. `None, None` on an unconfigured project
/// returns [`Response::SetupRequired`] rather than asking, because this may be
/// running in a daemon with no terminal to ask at; either value alone, or both
/// on a project that is already configured, is a refusal.
#[allow(clippy::too_many_arguments)]
pub fn run_sync_with(
    sync: &dyn SyncStorage,
    token: Option<&crate::domain::secret::GithubToken>,
    story_id: Option<&str>,
    dry_run: bool,
    resolve: Option<Resolution>,
    strategy: Option<initial::InitialStrategy>,
    mode: Option<SyncMode>,
) -> Result<Response, AppError> {
    // **A blanket resolution over a whole sync is this defect wearing a flag.**
    // `--resolve local` across every conflicting story would discard remote
    // edits the caller has never read, in one keystroke. Naming the story is
    // what makes it a decision about a disagreement somebody has actually seen.
    //
    // Asked here rather than in the parser, and before anything is loaded or
    // fetched, because this is the one gate every door passes: the CLI, the
    // dashboard, the TUI, and a hand-built `InvokeRequest`.
    if resolve.is_some() && story_id.is_none() {
        return Err(AppError::Usage(
            "--resolve applies to one story, so name it: `story github-sync <id> --resolve \
             local|remote`. Resolving a whole sync would decide conflicts you have not seen."
                .to_string(),
        ));
    }

    // `--strategy` and `--mode` answer one question together, for the same
    // reason `--resolve` is checked here rather than in the parser: this is
    // the one gate every door passes.
    if strategy.is_some() != mode.is_some() {
        return Err(AppError::Usage(
            "--strategy and --mode answer the same question during initial setup and must be \
             given together, or not at all: `story github-sync --strategy <strategy> --mode \
             <mode>`"
                .to_string(),
        ));
    }

    let mut setup_notes = Vec::new();

    // 1. Load sync config
    let mut config = match sync.load_config()? {
        Some(cfg) => {
            if strategy.is_some() || mode.is_some() {
                return Err(AppError::Usage(
                    "--strategy and --mode only apply to a project's first github-sync, and \
                     this project is already configured. Remove the flags and re-run."
                        .to_string(),
                ));
            }
            if cfg.sync.mode == SyncMode::Off {
                return Err(AppError::Usage(
                    "GitHub sync is disabled for this project (sync mode `off`). Re-run \
                     `story github-sync` and choose `manual`."
                        .to_string(),
                ));
            }
            if let Some(notice) = unimplemented_mode_notice(&cfg.sync.mode) {
                eprintln!("{notice}");
            }
            cfg
        }
        None => {
            let answers = strategy
                .zip(mode)
                .map(|(strategy, mode)| SetupAnswers { strategy, mode });
            match run_initial_setup(sync, token, answers)? {
                InitialSetupOutcome::Plan(plan) => return Ok(Response::SetupRequired(plan)),
                InitialSetupOutcome::Configured { config, notes } => {
                    setup_notes = notes;
                    config
                }
            }
        }
    };

    // If initial setup returned Off mode, bail
    if config.sync.mode == SyncMode::Off {
        return Ok(Response::Message(
            "GitHub sync is configured as off. No sync performed.".to_string(),
        ));
    }

    // 2. Get token and create client
    let token = require_github_token(token)?;
    let client = GithubClient::new(
        token.expose().to_string(),
        config.github.owner.clone(),
        config.github.repo.clone(),
    );

    // 3. Load local context
    let states = sync.states()?;
    let members = sync.members()?;
    let prefix = sync.prefix()?;
    let open_stories = sync.open_stories()?;

    let mut report = SyncReport::new();
    report.setup_notes = setup_notes;

    if dry_run {
        eprintln!("Dry-run mode: no changes will be written.\n");
    }

    // 4. Single-story sync
    if let Some(sid) = story_id {
        let story = open_stories.iter().find(|s| s.id == sid).ok_or_else(|| {
            AppError::NotFound(format!("story `{sid}` not found in open stories"))
        })?;

        match sync_single_story(
            sync,
            &client,
            &mut config,
            story,
            &states,
            &members,
            &prefix,
            dry_run,
            resolve,
        ) {
            Ok(result) => record_result(&mut report, &story.id, result),
            Err(e) => report.errors.push((story.id.clone(), e.to_string())),
        }

        if !dry_run {
            config.sync.last_sync_at = Some(sync.now());
            sync.save_config(&config)?;
        }

        return report.outcome();
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
                    sync,
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
                sync,
                &client,
                &mut config,
                story,
                &states,
                &members,
                &prefix,
                dry_run,
                resolve,
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
                sync,
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
                sync,
                &client,
                &mut config,
                story,
                &states,
                &members,
                &prefix,
                dry_run,
                resolve,
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
                sync,
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
        let now = sync.now();
        config.sync.last_sync_at = Some(now.clone());
        config.sync.last_full_sync_at = Some(now);
        sync.save_config(&config)?;
    }

    report.outcome()
}

// ---------------------------------------------------------------------------
// sync_single_story
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn sync_single_story(
    sync: &dyn SyncStorage,
    client: &GithubClient,
    config: &mut GithubSyncConfig,
    story: &StorySnapshot,
    states: &[StateDef],
    members: &[Member],
    prefix: &str,
    dry_run: bool,
    resolve: Option<Resolution>,
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
    let base = sync.load_base(&story.id)?.unwrap_or_else(|| story.clone());

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
        match resolve {
            // Nobody has said which side wins, and this process has no way to
            // ask: it is a daemon with no terminal, and the menu that used to
            // stand here answered itself. The conflicts travel back untouched.
            None => unresolved_conflicts.extend(merge_result.conflicts.iter().cloned()),
            Some(keep) => {
                resolved_conflicts = resolve_conflicts_batch(&merge_result.conflicts, keep);
            }
        }
    }

    // Apply local updates (pull from remote)
    let mut did_pull = false;
    if has_local_updates || has_new_remote_comments {
        sync.backup(&story.id)?;
        apply_local_updates(sync, &story.id, &merge_result.local_updates)?;

        // Apply resolved conflicts that chose KeepRemote
        for rc in &resolved_conflicts {
            if matches!(rc.resolution, Resolution::KeepRemote) {
                apply_conflict_locally(sync, &story.id, rc, &merge_result.conflicts)?;
            }
        }

        // Import new remote comments
        for comment in &merge_result.new_remote_comments {
            let event = StoryEvent::StoryCommentAdded {
                at: sync.now(),
                text: format!("[github] {}", comment.text),
            };
            sync.write_events(&story.id, &[event])?;
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
                    sync.backup(&story.id)?;
                    apply_conflict_locally(sync, &story.id, rc, &merge_result.conflicts)?;
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
            }
        }
    }

    // Save base snapshot for next sync — holding back whatever is still in
    // dispute, so that an unresolved conflict cannot resolve itself as
    // "remote wins" on the next run. See `diff::base_after_sync`.
    let updated_story = sync.story(&story.id)?;
    sync.save_base(
        &story.id,
        &base_after_sync(&base, &updated_story, &unresolved_conflicts),
    )?;

    // Update mapping's last_synced_at
    update_mapping_timestamp(sync, config, &story.id);

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
    sync: &dyn SyncStorage,
    client: &GithubClient,
    config: &mut GithubSyncConfig,
    issue: &types::GithubIssue,
    remote_snap: &RemoteSnapshot,
    states: &[StateDef],
    _members: &[Member],
    _prefix: &str,
) -> Result<String, AppError> {
    // Create the story via storage
    let snapshot = sync.create_story(&issue.title)?;
    let story_id = snapshot.id.clone();

    // Append events for additional fields
    let mut events = Vec::new();
    let now = sync.now();

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
            labels: normalize_labels(&remote_snap.labels),
        });
    }

    if let Some(ref description) = remote_snap.body_text {
        events.push(StoryEvent::StoryDescriptionSet {
            at: now.clone(),
            description: description.clone(),
        });
    }

    if !events.is_empty() {
        sync.write_events(&story_id, &events)?;
    }

    // Import comments from the issue
    let comments = client.list_comments(issue.number, None)?;
    for comment in &comments {
        if !comment.body.starts_with("[storyhook]") {
            let sc = github_comment_to_story(comment);
            sync.write_events(
                &story_id,
                &[StoryEvent::StoryCommentAdded {
                    at: sc.at,
                    text: sc.text,
                }],
            )?;
        }
    }

    // Update the issue body with the storyhook block so future syncs can find the mapping
    let story_for_block = sync.story(&story_id)?;
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
    let final_snapshot = sync.story(&story_id)?;
    sync.save_base(&story_id, &final_snapshot)?;

    // Add or update mapping
    let mapping_idx = config
        .mappings
        .iter()
        .position(|m| m.issue_number == issue.number);

    let new_mapping = StoryIssueMapping {
        story_id: story_id.clone(),
        issue_number: issue.number,
        last_synced_at: sync.now(),
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
        sync.write_events(
            &story_id,
            &[StoryEvent::StoryStateChanged {
                at: sync.now(),
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
    sync: &dyn SyncStorage,
    client: &GithubClient,
    config: &mut GithubSyncConfig,
    story: &StorySnapshot,
    _states: &[StateDef],
    members: &[Member],
    _prefix: &str,
) -> Result<u64, AppError> {
    let create_req = story_to_create_request(story, members, story.description.as_deref());
    let created_issue = client.create_issue(&create_req)?;

    // Save base snapshot
    sync.save_base(&story.id, story)?;

    // Add mapping
    config.mappings.push(StoryIssueMapping {
        story_id: story.id.clone(),
        issue_number: created_issue.number,
        last_synced_at: sync.now(),
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
    sync: &dyn SyncStorage,
    story_id: &str,
    updates: &FieldUpdates,
) -> Result<(), AppError> {
    let mut events = Vec::new();
    let now = sync.now();

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
        // `updates.labels` is a merge of the remote set (already comma-free,
        // via `render_remote_label`) and the locally-stored one — normalized
        // again here because the local side can still carry a label written
        // before SH-164's guard existed.
        events.push(StoryEvent::StoryLabelsSet {
            at: now.clone(),
            labels: normalize_labels(labels),
        });
    }

    if let Some(ref description_opt) = updates.description {
        // No dedicated "clear description" event exists; an empty string is
        // the legitimate representation of a cleared description, matching
        // how the CLI/web SetFields path already treats it.
        events.push(StoryEvent::StoryDescriptionSet {
            at: now.clone(),
            description: description_opt.clone().unwrap_or_default(),
        });
    }

    if !events.is_empty() {
        sync.write_events(story_id, &events)?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Apply resolved conflict locally (KeepRemote)
// ---------------------------------------------------------------------------

fn apply_conflict_locally(
    sync: &dyn SyncStorage,
    story_id: &str,
    resolved: &ResolvedConflict,
    conflicts: &[FieldConflict],
) -> Result<(), AppError> {
    let conflict = match conflicts.iter().find(|c| c.field == resolved.field) {
        Some(c) => c,
        None => return Ok(()),
    };

    let now = sync.now();
    let event = match conflict.field {
        ConflictField::Title => StoryEvent::StoryTitleSet {
            at: now,
            title: conflict.remote_value.clone(),
        },
        ConflictField::State => StoryEvent::StoryStateChanged {
            at: now,
            state: conflict.remote_value.clone(),
        },
        ConflictField::Assignee => {
            if conflict.remote_value == "<none>" {
                return Ok(());
            }
            StoryEvent::StoryAssigned {
                at: now,
                member_id: conflict.remote_value.clone(),
            }
        }
        ConflictField::Priority => {
            let priority = Priority::parse(&conflict.remote_value).unwrap_or(Priority::None);
            StoryEvent::StoryPrioritySet { at: now, priority }
        }
        ConflictField::Awaiting => {
            if conflict.remote_value == "<none>" {
                StoryEvent::StoryAwaitingCleared { at: now }
            } else {
                StoryEvent::StoryAwaitingSet {
                    at: now,
                    awaiting: conflict.remote_value.clone(),
                }
            }
        }
        ConflictField::Labels => {
            let labels = normalize_labels(conflict.remote_value.split(", "));
            StoryEvent::StoryLabelsSet { at: now, labels }
        }
        ConflictField::Description => {
            let description = if conflict.remote_value == "<none>" {
                String::new()
            } else {
                conflict.remote_value.clone()
            };
            StoryEvent::StoryDescriptionSet {
                at: now,
                description,
            }
        }
    };

    sync.write_events(story_id, &[event])
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
    match conflict.field {
        ConflictField::Title => updates.title = Some(conflict.local_value.clone()),
        ConflictField::State => updates.state = Some(conflict.local_value.clone()),
        ConflictField::Assignee => {
            if conflict.local_value == "<none>" {
                updates.assignee = Some(None);
            } else {
                updates.assignee = Some(Some(conflict.local_value.clone()));
            }
        }
        ConflictField::Priority => {
            let priority = Priority::parse(&conflict.local_value).unwrap_or(Priority::None);
            updates.priority = Some(priority);
        }
        ConflictField::Awaiting => {
            if conflict.local_value == "<none>" {
                updates.awaiting = Some(None);
            } else {
                updates.awaiting = Some(Some(conflict.local_value.clone()));
            }
        }
        ConflictField::Labels => {
            let labels: Vec<String> = conflict
                .local_value
                .split(", ")
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();
            updates.labels = Some(labels);
        }
        ConflictField::Description => {
            if conflict.local_value == "<none>" {
                updates.description = Some(None);
            } else {
                updates.description = Some(Some(conflict.local_value.clone()));
            }
        }
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
        description: remote.body_text.clone(),
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

fn update_mapping_timestamp(sync: &dyn SyncStorage, config: &mut GithubSyncConfig, story_id: &str) {
    let now = sync.now();
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
    if let Some(ref description) = updates.description {
        match description {
            Some(d) => eprintln!("{indent}description -> \"{d}\""),
            None => eprintln!("{indent}description -> (cleared)"),
        }
    }
}

#[cfg(test)]
mod mode_notice_tests {
    use super::*;

    /// A project carrying `auto` from before the rearchitecture must be told,
    /// not quietly demoted. The message has to say three things: what is
    /// configured, that nothing acts on it, and what to do.
    #[test]
    fn auto_is_reported_as_unimplemented_and_treated_as_manual() {
        let notice = unimplemented_mode_notice(&SyncMode::Auto).expect("auto needs a notice");
        assert!(notice.contains("auto"), "{notice}");
        assert!(notice.contains("no longer implements"), "{notice}");
        assert!(notice.contains("manual"), "{notice}");
        assert!(notice.contains("story github-sync"), "{notice}");
    }

    /// And the modes that work say nothing, because a notice on every run is a
    /// notice nobody reads.
    #[test]
    fn the_modes_that_work_are_silent() {
        assert!(unimplemented_mode_notice(&SyncMode::Manual).is_none());
        assert!(unimplemented_mode_notice(&SyncMode::Off).is_none());
    }
}

/// What a run answers with when it met a conflict nobody had decided — SH-152.
#[cfg(test)]
mod outcome_tests {
    use super::*;

    fn conflict(field: ConflictField, local: &str, remote: &str) -> FieldConflict {
        FieldConflict {
            field,
            base_value: "Base".to_string(),
            local_value: local.to_string(),
            remote_value: remote.to_string(),
        }
    }

    fn report_with_a_conflict() -> SyncReport {
        let mut report = SyncReport::new();
        report.pulled.push("SH-9".to_string());
        report.conflicts.push((
            "SH-1".to_string(),
            vec![conflict(ConflictField::Title, "Mine", "Theirs")],
        ));
        report
    }

    /// A sync that could not decide something is not a successful sync. Exit 8
    /// rather than 0 is the whole of it: a script that treated the old exit 0
    /// as "everything applied" was wrong and had no way to find out.
    #[test]
    fn an_undecided_conflict_is_an_error_not_a_message() {
        let error = report_with_a_conflict()
            .outcome()
            .expect_err("a conflict must not answer with a success");
        assert!(matches!(error, AppError::SyncConflict(_)), "{error}");
        assert_eq!(error.exit_code(), 8);
    }

    /// The three values are the part a caller cannot reconstruct, and the part
    /// that used to be printed to a stdout the daemon sends to `/dev/null`.
    #[test]
    fn the_refusal_carries_both_values_and_the_way_out() {
        let AppError::SyncConflict(detail) = report_with_a_conflict()
            .outcome()
            .expect_err("a conflict refuses")
        else {
            panic!("a conflict is a SyncConflict");
        };

        assert!(detail.contains("SH-1"), "{detail}");
        assert!(detail.contains("title"), "{detail}");
        assert!(detail.contains("Mine"), "{detail}");
        assert!(detail.contains("Theirs"), "{detail}");
        assert!(detail.contains("Base"), "{detail}");
        // What did land is still reported: a refusal that hid the rest of the
        // run would make the user re-derive it.
        assert!(detail.contains("SH-9"), "{detail}");
        assert!(
            detail.contains("story github-sync SH-1 --resolve local"),
            "the way out has to name the story, since --resolve refuses without one:\n{detail}"
        );
        assert!(
            detail.contains("story github-sync SH-1 --resolve remote"),
            "{detail}"
        );
        assert!(
            !detail.contains("sync complete"),
            "a run that left a conflict undecided did not complete:\n{detail}"
        );
    }

    /// Every conflicting story is named, not just the first — the example
    /// command uses one id, but the list is the whole set.
    #[test]
    fn every_conflicting_story_is_named() {
        let mut report = report_with_a_conflict();
        report.conflicts.push((
            "SH-2".to_string(),
            vec![conflict(ConflictField::State, "todo", "done")],
        ));

        let AppError::SyncConflict(detail) = report.outcome().expect_err("refuses") else {
            panic!("a conflict is a SyncConflict");
        };
        assert!(detail.contains("SH-1"), "{detail}");
        assert!(detail.contains("SH-2"), "{detail}");
        assert!(detail.contains("2 conflicting stories"), "{detail}");
    }

    /// And a run with nothing in dispute is untouched by any of this.
    #[test]
    fn a_run_with_no_conflicts_still_answers_with_a_message() {
        let mut report = SyncReport::new();
        report.pushed.push("SH-1".to_string());

        let Ok(Response::Message(message)) = report.outcome() else {
            panic!("a clean run answers with a message");
        };
        assert!(message.contains("GitHub sync complete."), "{message}");
    }
}
