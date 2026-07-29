//! # QUARANTINED — legacy-web only, deleted at W5
//!
//! This module is part of the pre-rearchitecture storage path. Story data lives
//! in a single global store now ([`crate::store`]), and no `story` command
//! reaches this code. It survives for exactly one reason: the web dashboard
//! (`src/web.rs`) still reads `.storyhook/` directories directly, and the wave
//! that promotes the daemon is what moves it onto the store and deletes this.
//!
//! **Do not add callers.** `tests/invoker_seam.rs::the_legacy_path_is_reachable_
//! only_from_the_web_dashboard` fails if any module other than `web.rs` reaches
//! it, so an accidental dependency is a failing test rather than a surprise a
//! wave later.
//!
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::cli::{
    CliOptions, EpicAction, GraphMode, HELP_TEXT, HistoryAction, HooksAction, Invocation,
    MemberInput, PhaseAction, PluginAction, StateAction, TypeAction, WebAction,
};
use crate::domain::{
    DependencyGraph, FieldEdit, ImportStory, Member, Priority, StateChanges, StateDef, StateUsage,
    StoryEvent, StorySnapshot, SuperState, compute_integrity_issues, compute_progress,
    derive_family_relationships, extract_story_ids, has_children, is_ready, parse_duration,
    relation_edges, would_create_parent_cycle,
};
use crate::error::AppError;
use crate::lock;
use crate::output::{
    BlockedChainView, GraphOverview, GraphView, PhaseView, ProjectSnapshotView, ReportData,
    Response, StaleInfo, StoryView, SummaryView, render_html_report,
};
use crate::storage;

pub fn run(root: &Path, options: CliOptions) -> Result<Response, AppError> {
    let no_hooks = options.no_hooks;

    #[cfg(feature = "github-sync")]
    let invocation_clone = options.invocation.clone();

    let response = match options.invocation {
        Invocation::Help => Ok(Response::Message(HELP_TEXT.to_string())),
        Invocation::Init {
            prefix,
            no_agents_md,
        } => {
            storage::init_project(root, prefix.as_deref())?;
            let mut msg = "initialized story project\n\n\
                 The .storyhook/ directory contains your project data.\n\
                 Remember to commit it to git — it should travel with the repository."
                .to_string();

            // Generate AGENTS.md by default unless opted out
            if !no_agents_md {
                let agents_md_path = root.join("AGENTS.md");
                if !agents_md_path.exists() {
                    let content = generate_agents_md(root);
                    std::fs::write(&agents_md_path, content)?;
                    msg.push_str("\n\nGenerated AGENTS.md for AI agent discoverability.");
                }
            }

            Ok(Response::Message(msg))
        }
        Invocation::New {
            title,
            state,
            story_type,
            description,
            priority,
            labels,
            assignee,
        } => lock::with_project_lock(root, || {
            // Validate every enrichment field before creating the story, so
            // invalid input never leaves a partially-created story behind.
            if let Some(ref st) = story_type {
                let type_map = storage::load_type_map(root)?;
                if !type_map.contains_key(st.as_str()) {
                    return Err(AppError::Validation(format!(
                        "unknown type `{st}`. Available types: {}",
                        type_map.keys().cloned().collect::<Vec<_>>().join(", ")
                    )));
                }
            }
            let priority_level = priority
                .as_deref()
                .map(|p| {
                    Priority::parse(p)
                        .ok_or_else(|| AppError::Validation(format!("invalid priority `{p}`")))
                })
                .transpose()?;
            let assignee_member = assignee
                .as_deref()
                .map(|a| storage::find_member(root, a))
                .transpose()?;

            let now = storage::now();
            let mut extra: Vec<StoryEvent> = Vec::new();
            if let Some(level) = priority_level {
                extra.push(StoryEvent::StoryPrioritySet {
                    at: now.clone(),
                    priority: level,
                });
            }
            if let Some(labels) = labels {
                let mut sorted: Vec<String> = labels;
                sorted.sort();
                sorted.dedup();
                if !sorted.is_empty() {
                    extra.push(StoryEvent::StoryLabelsSet {
                        at: now.clone(),
                        labels: sorted,
                    });
                }
            }
            if let Some(member) = assignee_member {
                extra.push(StoryEvent::StoryAssigned {
                    at: now.clone(),
                    member_id: member.id,
                });
            }
            if let Some(ref description) = description
                && !description.trim().is_empty()
            {
                extra.push(StoryEvent::StoryDescriptionSet {
                    at: now.clone(),
                    description: description.clone(),
                });
            }
            if let Some(ref st) = story_type {
                extra.push(StoryEvent::StoryTypeSet {
                    at: now.clone(),
                    story_type: st.clone(),
                });
            }

            let story = storage::create_story_with_events(root, &title, state.as_deref(), &extra)?;
            if !no_hooks && let Some(ref config) = crate::event_hooks::load_hooks_config(root) {
                let payload = serde_json::json!({
                    "event_type": "create",
                    "story_id": &story.id,
                    "timestamp": &story.created_at,
                    "story_title": &story.title,
                    "initial_state": &story.state
                });
                crate::event_hooks::fire_hook(
                    root,
                    config,
                    crate::event_hooks::HookEventType::Create,
                    &payload.to_string(),
                );
            }
            story_view_response(root, story)
        }),
        Invocation::MemberAdd { input } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            let member = build_member(root, input)?;
            storage::store_member(root, &member)?;
            Ok(Response::Message(format!("added member {}", member.id)))
        }),
        Invocation::State { action } => run_state_action(root, action),
        Invocation::List {
            state,
            assignee,
            flagged,
            priority,
            label,
            created_after,
            updated_after,
            blocked,
            ready,
            stale,
            phase,
            story_type,
        } => {
            storage::ensure_project(root)?;
            let mut views = build_story_views(root, false)?;
            let story_map: BTreeMap<String, StorySnapshot> = views
                .iter()
                .map(|v| (v.story.id.clone(), v.story.clone()))
                .collect();
            if let Some(state) = state {
                views.retain(|view| view.story.state == state);
            }
            if let Some(assignee) = assignee {
                views.retain(|view| view.story.assignee.as_deref() == Some(assignee.as_str()));
            }
            if flagged {
                views.retain(|view| !view.flagged_reasons.is_empty());
            }
            if let Some(priority_csv) = priority {
                let priorities: Vec<Priority> = priority_csv
                    .split(',')
                    .filter_map(|p| Priority::parse(p.trim()))
                    .collect();
                if !priorities.is_empty() {
                    views.retain(|view| priorities.contains(&view.story.priority));
                }
            }
            if let Some(label_csv) = label {
                let filter_labels: Vec<String> = label_csv
                    .split(',')
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect();
                if !filter_labels.is_empty() {
                    views
                        .retain(|view| filter_labels.iter().any(|l| view.story.labels.contains(l)));
                }
            }
            if let Some(ref threshold) = created_after {
                views.retain(|view| view.story.created_at.as_str() >= threshold.as_str());
            }
            if let Some(ref threshold) = updated_after {
                views.retain(|view| view.story.updated_at.as_str() >= threshold.as_str());
            }
            if blocked {
                views.retain(|view| {
                    view.story.superstate == SuperState::Open && !is_ready(&view.story, &story_map)
                });
            }
            if ready {
                views.retain(|view| is_ready(&view.story, &story_map));
            }
            if let Some(ref phase_num) = phase {
                let phase_label = format!("phase:{phase_num}");
                views.retain(|view| view.story.labels.contains(&phase_label));
            }
            if let Some(ref st) = story_type {
                if st == "none" {
                    views.retain(|view| view.story.story_type.is_none());
                } else {
                    views.retain(|view| view.story.story_type.as_deref() == Some(st.as_str()));
                }
            }
            if let Some(ref stale_str) = stale {
                let duration = parse_duration(stale_str).ok_or_else(|| {
                    AppError::Validation(format!(
                        "invalid duration `{stale_str}` (use e.g. 2h, 1d, 1w)"
                    ))
                })?;
                let threshold = chrono::Utc::now() - duration;
                let threshold_str = threshold.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
                views.retain(|view| {
                    view.story.superstate == SuperState::Open
                        && view.story.updated_at.as_str() <= threshold_str.as_str()
                });
            }
            if stale.is_some() {
                for view in &mut views {
                    let events = storage::load_open_story_events(root, &view.story.id)?;
                    let activity_type = crate::domain::last_activity_type(&events);
                    if let Ok(updated) =
                        chrono::DateTime::parse_from_rfc3339(&view.story.updated_at)
                    {
                        let days =
                            (chrono::Utc::now() - updated.with_timezone(&chrono::Utc)).num_days();
                        view.stale_info = Some(StaleInfo {
                            last_activity_at: view.story.updated_at.clone(),
                            last_activity_type: activity_type.to_string(),
                            days_stale: days.max(0) as u64,
                        });
                    }
                }
            }
            sort_story_views(&mut views);
            Ok(Response::Stories(views, None))
        }
        Invocation::Summary => {
            storage::ensure_project(root)?;
            let views = build_story_views(root, false)?;
            let story_map: BTreeMap<String, StorySnapshot> = views
                .iter()
                .map(|v| (v.story.id.clone(), v.story.clone()))
                .collect();

            let total_open = views
                .iter()
                .filter(|v| v.story.superstate == SuperState::Open)
                .count();
            let total_closed = views.len() - total_open;

            let mut state_counts: BTreeMap<String, usize> = BTreeMap::new();
            let mut priority_counts: BTreeMap<String, usize> = BTreeMap::new();
            let mut type_counts: BTreeMap<String, usize> = BTreeMap::new();
            let mut blocked_count = 0;
            let mut flagged_count = 0;

            for view in &views {
                *state_counts.entry(view.story.state.clone()).or_default() += 1;
                if view.story.priority != Priority::None {
                    *priority_counts
                        .entry(view.story.priority.as_str().to_string())
                        .or_default() += 1;
                }
                let type_label = view
                    .story
                    .story_type
                    .as_deref()
                    .unwrap_or("Default")
                    .to_string();
                *type_counts.entry(type_label).or_default() += 1;
                if !view.flagged_reasons.is_empty() {
                    flagged_count += 1;
                }
                if view.story.superstate == SuperState::Open && !is_ready(&view.story, &story_map) {
                    blocked_count += 1;
                }
            }

            let mut ready: Vec<StoryView> = views
                .into_iter()
                .filter(|v| is_ready(&v.story, &story_map))
                .collect();
            ready.sort_by(|a, b| {
                a.story
                    .priority
                    .cmp(&b.story.priority)
                    .then_with(|| a.story.created_at.cmp(&b.story.created_at))
            });
            let ready_count = ready.len();
            ready.truncate(5);

            let by_state: Vec<(String, usize)> = state_counts.into_iter().collect();
            let by_priority: Vec<(String, usize)> = priority_counts.into_iter().collect();
            let by_type: Vec<(String, usize)> = type_counts.into_iter().collect();

            Ok(Response::Summary(Box::new(SummaryView {
                total_open,
                total_closed,
                by_state,
                by_priority,
                by_type,
                blocked_count,
                flagged_count,
                ready_count,
                ready_stories: ready,
            })))
        }
        Invocation::Report { html } => {
            if !html {
                let data = build_report_data(root)?;
                let mut ready: Vec<StoryView> = data
                    .stories
                    .into_iter()
                    .filter(|v| data.ready_ids.contains(&v.story.id))
                    .collect();
                ready.sort_by(|a, b| {
                    a.story
                        .priority
                        .cmp(&b.story.priority)
                        .then_with(|| a.story.created_at.cmp(&b.story.created_at))
                });
                let ready_count = ready.len();
                ready.truncate(5);

                let mut summary = data.summary;
                summary.ready_count = ready_count;
                summary.ready_stories = ready;

                Ok(Response::Summary(Box::new(summary)))
            } else {
                let data = build_report_data(root)?;
                let ready_set: BTreeSet<&str> = data.ready_ids.iter().map(|s| s.as_str()).collect();
                let blocked_set: BTreeSet<&str> =
                    data.blocked_ids.iter().map(|s| s.as_str()).collect();

                let html_output = render_html_report(
                    &data.summary,
                    &data.stories,
                    &|id| ready_set.contains(id),
                    &|id| blocked_set.contains(id),
                );

                Ok(Response::Message(html_output))
            }
        }
        Invocation::Search { query } => {
            storage::ensure_project(root)?;
            let query_lower = query.to_lowercase();
            let all = storage::load_all_snapshots(root)?;
            let mut results: Vec<StoryView> = Vec::new();
            for story in all {
                let matches = story.title.to_lowercase().contains(&query_lower)
                    || story
                        .comments
                        .iter()
                        .any(|c| c.text.to_lowercase().contains(&query_lower))
                    || story
                        .labels
                        .iter()
                        .any(|l| l.to_lowercase().contains(&query_lower));
                if matches {
                    results.push(StoryView {
                        story,
                        derived_relationships: Vec::new(),
                        warnings: Vec::new(),
                        flagged_reasons: Vec::new(),
                        stale_info: None,
                        progress: None,
                    });
                }
            }
            sort_story_views(&mut results);
            Ok(Response::Stories(results, None))
        }
        Invocation::Doctor { fix } => {
            storage::ensure_project(root)?;
            if fix {
                lock::with_project_lock(root, || doctor_fix(root))
            } else {
                doctor_report(root)
            }
        }
        Invocation::Show { id } => {
            storage::ensure_project(root)?;
            let story = storage::load_story_snapshot(root, &id)?;
            story_view_response(root, story)
        }
        Invocation::Comment { id, text } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            ensure_open_story(root, &id)?;
            let text_clone = text.clone();
            storage::write_story_events(
                root,
                &id,
                &[StoryEvent::StoryCommentAdded {
                    at: storage::now(),
                    text,
                }],
            )?;
            if !no_hooks && let Some(ref config) = crate::event_hooks::load_hooks_config(root) {
                let snapshot = storage::load_open_story_snapshot(root, &id)?;
                let payload = serde_json::json!({
                    "event_type": "comment",
                    "story_id": &id,
                    "timestamp": crate::storage::now(),
                    "story_title": &snapshot.title,
                    "comment_text": &text_clone
                });
                crate::event_hooks::fire_hook(
                    root,
                    config,
                    crate::event_hooks::HookEventType::Comment,
                    &payload.to_string(),
                );
            }
            story_view_by_id(root, &id)
        }),
        Invocation::Assign { id, member } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            ensure_open_story(root, &id)?;
            let member = storage::find_member(root, &member)?;
            storage::write_story_events(
                root,
                &id,
                &[StoryEvent::StoryAssigned {
                    at: storage::now(),
                    member_id: member.id,
                }],
            )?;
            story_view_by_id(root, &id)
        }),
        Invocation::SetAwaiting { id, awaiting } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            ensure_open_story(root, &id)?;
            let awaiting = awaiting.trim().to_string();
            if awaiting.is_empty() {
                return Err(AppError::Validation(
                    "awaiting reason must not be empty".to_string(),
                ));
            }
            storage::write_story_events(
                root,
                &id,
                &[StoryEvent::StoryAwaitingSet {
                    at: storage::now(),
                    awaiting,
                }],
            )?;
            story_view_by_id(root, &id)
        }),
        Invocation::ClearAwaiting { id } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            ensure_open_story(root, &id)?;
            let story = storage::load_open_story_snapshot(root, &id)?;
            if story.awaiting.is_none() {
                return story_view_by_id(root, &id);
            }
            storage::write_story_events(
                root,
                &id,
                &[StoryEvent::StoryAwaitingCleared { at: storage::now() }],
            )?;
            story_view_by_id(root, &id)
        }),
        Invocation::SetState {
            id,
            state,
            comment,
            if_state,
        } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            // Ground-truth read (open or archived) must happen before the
            // open/closed check below: a concurrent writer may have closed
            // (and archived) the story between the caller's read and this
            // call. If --if-state was supplied, a mismatch against whatever
            // the story's *current* state actually is — open or closed — is
            // a lost race, and must be reported as a conflict rather than
            // surfacing as `ensure_open_story`'s generic "closed and cannot
            // be modified" validation error.
            //
            // `story delete` is the one closure path that leaves the `state`
            // slug itself unchanged — it only forces `deleted`/`superstate`
            // to CLOSED (see fold_story in domain.rs) — so a stale
            // --if-state whose value still equals the pre-deletion slug
            // would otherwise pass this comparison undetected. `deleted`
            // must be checked as part of ground truth alongside the slug,
            // not inferred from it.
            if let Some(expected) = &if_state {
                let current = storage::load_story_snapshot(root, &id)?;
                if current.deleted || &current.state != expected {
                    let actual = if current.deleted {
                        "deleted".to_string()
                    } else {
                        current.state.clone()
                    };
                    return Err(AppError::StateConflict(expected.clone(), actual));
                }
            }
            ensure_open_story(root, &id)?;
            let story = storage::load_open_story_snapshot(root, &id)?;
            let states = storage::load_state_map(root)?;
            let state_def = states
                .get(&state)
                .ok_or_else(|| AppError::Validation(format!("state `{state}` is not defined")))?;
            let now = storage::now();
            let comment_event = comment.map(|text| StoryEvent::StoryCommentAdded {
                at: now.clone(),
                text,
            });
            let events = storage::state_transition_events(
                state_def,
                story.awaiting.is_some(),
                &now,
                comment_event.into_iter().collect(),
            );
            storage::write_story_events(root, &id, &events)?;

            if !no_hooks && let Some(ref config) = crate::event_hooks::load_hooks_config(root) {
                let payload = serde_json::json!({
                    "event_type": "state_change",
                    "story_id": &id,
                    "timestamp": &now,
                    "story_title": &story.title,
                    "from_state": &story.state,
                    "to_state": &state
                });
                crate::event_hooks::fire_hook(
                    root,
                    config,
                    crate::event_hooks::HookEventType::StateChange,
                    &payload.to_string(),
                );
                if state_def.super_state == SuperState::Closed {
                    let close_payload = serde_json::json!({
                        "event_type": "close",
                        "story_id": &id,
                        "timestamp": &now,
                        "story_title": &story.title,
                        "final_state": &state
                    });
                    crate::event_hooks::fire_hook(
                        root,
                        config,
                        crate::event_hooks::HookEventType::Close,
                        &close_payload.to_string(),
                    );
                }
            }

            if state_def.super_state == SuperState::Closed {
                let story = storage::archive_story(root, &id)?;
                story_view_response(root, story)
            } else {
                story_view_by_id(root, &id)
            }
        }),
        Invocation::SetLabels { id, add, remove } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            ensure_open_story(root, &id)?;
            let story = storage::load_open_story_snapshot(root, &id)?;
            let mut labels: BTreeSet<String> = story.labels.into_iter().collect();
            for label in &add {
                labels.insert(label.clone());
            }
            for label in &remove {
                labels.remove(label);
            }
            let labels: Vec<String> = labels.into_iter().collect();
            storage::write_story_events(
                root,
                &id,
                &[StoryEvent::StoryLabelsSet {
                    at: storage::now(),
                    labels,
                }],
            )?;
            if !no_hooks && let Some(ref config) = crate::event_hooks::load_hooks_config(root) {
                let snapshot = storage::load_open_story_snapshot(root, &id)?;
                let payload = serde_json::json!({
                    "event_type": "label_change",
                    "story_id": &id,
                    "timestamp": crate::storage::now(),
                    "story_title": &snapshot.title,
                    "labels": &snapshot.labels
                });
                crate::event_hooks::fire_hook(
                    root,
                    config,
                    crate::event_hooks::HookEventType::LabelChange,
                    &payload.to_string(),
                );
            }
            story_view_by_id(root, &id)
        }),
        Invocation::SetPriority { id, priority } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            ensure_open_story(root, &id)?;
            let priority = Priority::parse(&priority).ok_or_else(|| {
                AppError::Validation(
                    "priority must be one of: critical, high, medium, low, none".to_string(),
                )
            })?;
            let priority_str = priority.as_str().to_string();
            storage::write_story_events(
                root,
                &id,
                &[StoryEvent::StoryPrioritySet {
                    at: storage::now(),
                    priority,
                }],
            )?;
            if !no_hooks && let Some(ref config) = crate::event_hooks::load_hooks_config(root) {
                let snapshot = storage::load_open_story_snapshot(root, &id)?;
                let payload = serde_json::json!({
                    "event_type": "priority_change",
                    "story_id": &id,
                    "timestamp": crate::storage::now(),
                    "story_title": &snapshot.title,
                    "priority": &priority_str
                });
                crate::event_hooks::fire_hook(
                    root,
                    config,
                    crate::event_hooks::HookEventType::PriorityChange,
                    &payload.to_string(),
                );
            }
            story_view_by_id(root, &id)
        }),
        Invocation::Next { count, phase } => {
            storage::ensure_project(root)?;
            let views = build_story_views(root, false)?;
            let story_map: BTreeMap<String, StorySnapshot> = views
                .iter()
                .map(|v| (v.story.id.clone(), v.story.clone()))
                .collect();
            let mut ready: Vec<StoryView> = views
                .into_iter()
                .filter(|v| is_ready(&v.story, &story_map) && !has_children(&v.story))
                .collect();
            if let Some(ref phase_num) = phase {
                let phase_label = format!("phase:{phase_num}");
                ready.retain(|v| v.story.labels.contains(&phase_label));
            }
            ready.sort_by(|a, b| {
                a.story
                    .priority
                    .cmp(&b.story.priority)
                    .then_with(|| a.story.created_at.cmp(&b.story.created_at))
            });
            ready.truncate(count);
            if ready.is_empty() {
                Ok(Response::Message("no ready stories".to_string()))
            } else if count == 1 {
                Ok(Response::Story(Box::new(ready.remove(0))))
            } else {
                Ok(Response::Stories(ready, None))
            }
        }
        Invocation::Import { file } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            let input = match file {
                Some(ref path) => std::fs::read_to_string(path)
                    .map_err(|e| AppError::Storage(format!("failed to read {path}: {e}")))?,
                None => {
                    use std::io::Read;
                    let mut buf = String::new();
                    std::io::stdin()
                        .read_to_string(&mut buf)
                        .map_err(|e| AppError::Storage(format!("failed to read stdin: {e}")))?;
                    buf
                }
            };
            let stories: Vec<ImportStory> = serde_json::from_str(&input)?;
            if stories.is_empty() {
                return Ok(Response::Message("no stories to import".to_string()));
            }
            // Validate all story_type values before creating any stories
            let type_map = storage::load_type_map(root)?;
            let invalid_types: std::collections::BTreeSet<&str> = stories
                .iter()
                .filter_map(|s| s.story_type.as_deref())
                .filter(|st| !type_map.contains_key(*st))
                .collect();
            if !invalid_types.is_empty() {
                return Err(AppError::Validation(format!(
                    "unknown types: {}. Available types: {}",
                    invalid_types.into_iter().collect::<Vec<_>>().join(", "),
                    type_map.keys().cloned().collect::<Vec<_>>().join(", ")
                )));
            }
            let mut created_ids: Vec<String> = Vec::new();
            for import_story in &stories {
                let story = storage::create_story(
                    root,
                    &import_story.title,
                    import_story.state.as_deref(),
                )?;
                let id = story.id.clone();
                let now = storage::now();
                let mut events = Vec::new();
                if let Some(ref priority_str) = import_story.priority
                    && let Some(priority) = Priority::parse(priority_str)
                {
                    events.push(StoryEvent::StoryPrioritySet {
                        at: now.clone(),
                        priority,
                    });
                }
                if let Some(ref labels) = import_story.labels
                    && !labels.is_empty()
                {
                    let mut sorted: Vec<String> = labels.clone();
                    sorted.sort();
                    sorted.dedup();
                    events.push(StoryEvent::StoryLabelsSet {
                        at: now.clone(),
                        labels: sorted,
                    });
                }
                if let Some(ref assignee) = import_story.assignee {
                    let member = storage::find_member(root, assignee)?;
                    events.push(StoryEvent::StoryAssigned {
                        at: now.clone(),
                        member_id: member.id,
                    });
                }
                if let Some(ref description) = import_story.description
                    && !description.trim().is_empty()
                {
                    events.push(StoryEvent::StoryDescriptionSet {
                        at: now.clone(),
                        description: description.clone(),
                    });
                }
                if let Some(ref st) = import_story.story_type {
                    events.push(StoryEvent::StoryTypeSet {
                        at: now.clone(),
                        story_type: st.clone(),
                    });
                }
                if !events.is_empty() {
                    storage::write_story_events(root, &id, &events)?;
                }
                created_ids.push(id);
            }
            // Second pass: resolve relationships
            for (index, import_story) in stories.iter().enumerate() {
                if let Some(ref rels) = import_story.relationships {
                    let a_id = &created_ids[index];
                    for rel in rels {
                        let b_id = if let Some(ref_idx) = rel.ref_index {
                            created_ids.get(ref_idx).cloned().ok_or_else(|| {
                                AppError::Validation(format!(
                                    "ref_index {ref_idx} out of bounds for import batch"
                                ))
                            })?
                        } else if let Some(ref other) = rel.other_id {
                            other.clone()
                        } else {
                            return Err(AppError::Validation(
                                "relationship must have ref_index or other_id".to_string(),
                            ));
                        };
                        if a_id == &b_id {
                            continue;
                        }
                        let edges = relation_edges(&rel.relation).ok_or_else(|| {
                            AppError::Validation(format!(
                                "unsupported relationship `{}`",
                                rel.relation
                            ))
                        })?;
                        let now = storage::now();
                        for (a_rel, b_rel) in edges {
                            storage::write_story_events(
                                root,
                                a_id,
                                &[StoryEvent::StoryRelationshipAdded {
                                    at: now.clone(),
                                    other_id: b_id.clone(),
                                    relation: a_rel.to_string(),
                                }],
                            )?;
                            if storage::open_story_exists(root, &b_id) {
                                storage::write_story_events(
                                    root,
                                    &b_id,
                                    &[StoryEvent::StoryRelationshipAdded {
                                        at: now.clone(),
                                        other_id: a_id.clone(),
                                        relation: b_rel.to_string(),
                                    }],
                                )?;
                            }
                        }
                    }
                }
            }
            let mut views = Vec::new();
            for id in &created_ids {
                let story = storage::load_open_story_snapshot(root, id)?;
                views.push(StoryView {
                    story,
                    derived_relationships: Vec::new(),
                    warnings: Vec::new(),
                    flagged_reasons: Vec::new(),
                    stale_info: None,
                    progress: None,
                });
            }
            Ok(Response::Stories(views, None))
        }),
        Invocation::Decompose {
            file,
            stdin,
            dry_run,
        } => {
            let content = if stdin {
                use std::io::Read as _;
                let mut buf = String::new();
                std::io::stdin()
                    .read_to_string(&mut buf)
                    .map_err(|e| AppError::Storage(format!("failed to read stdin: {e}")))?;
                buf
            } else if let Some(ref path) = file {
                std::fs::read_to_string(path)
                    .map_err(|e| AppError::Storage(format!("failed to read {path}: {e}")))?
            } else {
                return Err(AppError::Usage(
                    "usage: story decompose <file> [--dry-run] | story decompose --stdin [--dry-run]"
                        .to_string(),
                ));
            };

            let import_stories = crate::decompose::decompose(file.as_deref(), &content)?;

            if dry_run {
                let json = serde_json::to_string_pretty(&import_stories)?;
                return Ok(Response::Message(json));
            }

            if import_stories.is_empty() {
                return Ok(Response::Message("no stories to import".to_string()));
            }

            lock::with_project_lock(root, || {
                storage::ensure_project(root)?;
                let result = import_stories_batch(root, &import_stories)?;

                // Build a relationship summary
                let story_count = result.views.len();
                let rel_count = result.relationship_lines.len();
                let mut summary = format!(
                    "Created {} {} with {} {}",
                    story_count,
                    if story_count == 1 { "story" } else { "stories" },
                    rel_count,
                    if rel_count == 1 {
                        "relationship"
                    } else {
                        "relationships"
                    },
                );
                if !result.relationship_lines.is_empty() {
                    summary.push(':');
                    for line in &result.relationship_lines {
                        summary.push_str(&format!("\n  {}", line));
                    }
                }

                Ok(Response::Stories(result.views, Some(summary)))
            })
        }
        Invocation::Reopen { id, force } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            if storage::open_story_exists(root, &id) {
                return Err(AppError::Validation(format!(
                    "story `{id}` is already open"
                )));
            }
            if !storage::is_archived(root, &id)? {
                return Err(AppError::NotFound(format!("story `{id}` not found")));
            }
            // Reopening a soft-deleted story is an undelete, not an ordinary
            // reopen — guard it so a stray `reopen` doesn't silently restore
            // something someone meant to remove. Ordinarily-closed stories
            // (deleted == false) skip this entirely.
            if !force {
                let archived = storage::load_archived_story(root, &id)?;
                if archived.deleted && !confirm_undelete(&id, archived.deleted_reason.as_deref())? {
                    return Ok(Response::Message(format!(
                        "reopen aborted: `{id}` was not undeleted"
                    )));
                }
            }
            storage::unarchive_story(root, &id)?;
            let default_state = storage::default_open_state(root)?;
            let now = storage::now();
            let state_slug = default_state.slug.clone();
            storage::write_story_events(
                root,
                &id,
                &[StoryEvent::StoryStateChanged {
                    at: now,
                    state: default_state.slug,
                }],
            )?;
            if !no_hooks && let Some(ref config) = crate::event_hooks::load_hooks_config(root) {
                let snapshot = storage::load_open_story_snapshot(root, &id)?;
                let payload = serde_json::json!({
                    "event_type": "state_change",
                    "story_id": &id,
                    "timestamp": crate::storage::now(),
                    "story_title": &snapshot.title,
                    "from_state": "closed",
                    "to_state": &state_slug
                });
                crate::event_hooks::fire_hook(
                    root,
                    config,
                    crate::event_hooks::HookEventType::StateChange,
                    &payload.to_string(),
                );
            }
            story_view_by_id(root, &id)
        }),
        Invocation::Delete { id, reason } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            storage::delete_story(root, &id, &reason)?;
            Ok(Response::Message(format!("deleted {id}: {reason}")))
        }),
        Invocation::BulkUpdate { updates } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            let states = storage::load_state_map(root)?;
            let mut results: Vec<String> = Vec::new();

            for (id, state_slug) in &updates {
                match states.get(state_slug) {
                    None => {
                        results.push(format!("{id}: error — state `{state_slug}` is not defined"));
                    }
                    Some(state_def) => {
                        if !storage::open_story_exists(root, id) {
                            results.push(format!("{id}: error — story not found or not open"));
                            continue;
                        }

                        match storage::move_story_to_state(root, id, state_def) {
                            Ok(true) => results.push(format!("{id}: {state_slug} (archived)")),
                            Ok(false) => results.push(format!("{id}: {state_slug}")),
                            Err(e) => results.push(format!("{id}: error — {e}")),
                        }
                    }
                }
            }

            Ok(Response::Message(results.join("\n")))
        }),
        Invocation::Export => {
            storage::ensure_project(root)?;
            let export = storage::export_project(root)?;
            let json = serde_json::to_string_pretty(&export)?;
            // `RawJson`, not `Message`: the export document *is* the result, so
            // the `--json` envelope has nothing to add and wrapping it as an
            // escaped string makes `story import-project` reject it. See
            // `tests/story_export.rs::export_json_flag_emits_the_document_itself`.
            Ok(Response::RawJson(json))
        }
        Invocation::ImportProject { file } => lock::with_project_lock(root, || {
            let input = std::fs::read_to_string(&file)
                .map_err(|e| AppError::Storage(format!("failed to read {file}: {e}")))?;
            let export: storage::ProjectExport = serde_json::from_str(&input)?;
            storage::import_project(root, &export)?;
            Ok(Response::Message(format!(
                "imported project with {} stories",
                export.stories.len()
            )))
        }),
        // The one arm on this leg that reaches the store, and deliberately so.
        // `story migrate` moves a legacy tree *into* the store, so it has to
        // work from a binary whose default backend is still the legacy one —
        // otherwise nothing could be migrated until after the flip, and the
        // repo cutover is what proves the flip is safe. It takes no project
        // lock: it never writes to `root`.
        Invocation::Migrate { .. } => {
            use crate::store::Store as _;
            let store = crate::store::SqliteStore::open(crate::paths::store_path()?)?;
            store.migrate()?;
            crate::invoke::dispatch_unscoped(&store, root, &storage::now(), options.invocation)
        }
        Invocation::Context { format } => {
            storage::ensure_project(root)?;
            let views = build_story_views(root, false)?;
            let story_map: BTreeMap<String, StorySnapshot> = views
                .iter()
                .map(|v| (v.story.id.clone(), v.story.clone()))
                .collect();

            let total_open = views
                .iter()
                .filter(|v| v.story.superstate == SuperState::Open)
                .count();
            let total_closed = views.len() - total_open;

            let mut state_counts: BTreeMap<String, usize> = BTreeMap::new();
            let mut type_counts: BTreeMap<String, usize> = BTreeMap::new();
            for view in &views {
                *state_counts.entry(view.story.state.clone()).or_default() += 1;
                let type_label = view
                    .story
                    .story_type
                    .as_deref()
                    .unwrap_or("Default")
                    .to_string();
                *type_counts.entry(type_label).or_default() += 1;
            }

            let blocked: Vec<&StoryView> = views
                .iter()
                .filter(|v| {
                    v.story.superstate == SuperState::Open && !is_ready(&v.story, &story_map)
                })
                .collect();

            let mut ready: Vec<&StoryView> = views
                .iter()
                .filter(|v| is_ready(&v.story, &story_map))
                .collect();
            ready.sort_by(|a, b| {
                a.story
                    .priority
                    .cmp(&b.story.priority)
                    .then_with(|| a.story.created_at.cmp(&b.story.created_at))
            });
            let ready_count = ready.len();
            ready.truncate(5);

            let use_json = format.as_deref() == Some("json");
            if use_json {
                let context = serde_json::json!({
                    "total_stories": views.len(),
                    "open": total_open,
                    "closed": total_closed,
                    "by_state": state_counts,
                    "by_type": type_counts,
                    "blocked_count": blocked.len(),
                    "ready_count": ready_count,
                    "ready_stories": ready.iter().map(|v| {
                        serde_json::json!({
                            "id": v.story.id,
                            "title": v.story.title,
                            "state": v.story.state,
                            "priority": v.story.priority.as_str(),
                        })
                    }).collect::<Vec<_>>(),
                });
                Ok(Response::Message(
                    serde_json::to_string_pretty(&context).unwrap(),
                ))
            } else {
                let mut body = String::new();
                body.push_str(&format!(
                    "# Project Status\n\nStories: {} ({} open, {} closed)\n\n",
                    views.len(),
                    total_open,
                    total_closed
                ));

                body.push_str("## State Distribution\n\n");
                for (state, count) in &state_counts {
                    body.push_str(&format!("- {state}: {count}\n"));
                }

                body.push_str("\n## Type Distribution\n\n");
                for (type_name, count) in &type_counts {
                    body.push_str(&format!("- {type_name}: {count}\n"));
                }

                if !ready.is_empty() {
                    body.push_str(&format!("\n## Ready to Work ({} total)\n\n", ready_count));
                    for view in &ready {
                        let priority = if view.story.priority != Priority::None {
                            format!(" ({})", view.story.priority.as_str())
                        } else {
                            String::new()
                        };
                        body.push_str(&format!(
                            "- {} {}{}\n",
                            view.story.id, view.story.title, priority
                        ));
                    }
                }

                if !blocked.is_empty() {
                    body.push_str(&format!("\n## Blocked ({})\n\n", blocked.len()));
                    for view in &blocked {
                        let reason = if let Some(ref awaiting) = view.story.awaiting {
                            format!(" — awaiting: {awaiting}")
                        } else {
                            String::new()
                        };
                        body.push_str(&format!(
                            "- {} {}{}\n",
                            view.story.id, view.story.title, reason
                        ));
                    }
                }

                // Phase Progress section
                let mut phase_map: BTreeMap<String, Vec<&StoryView>> = BTreeMap::new();
                for view in &views {
                    for label in &view.story.labels {
                        if let Some(num) = label.strip_prefix("phase:") {
                            phase_map.entry(num.to_string()).or_default().push(view);
                        }
                    }
                }

                if !phase_map.is_empty() {
                    body.push_str("\n## Phase Progress\n\n");
                    for (phase_num, phase_stories) in &phase_map {
                        let total = phase_stories.len();
                        let done_count = phase_stories
                            .iter()
                            .filter(|v| v.story.superstate == SuperState::Closed)
                            .count();
                        let default_open = storage::default_open_state(root)
                            .map(|s| s.slug)
                            .unwrap_or_else(|_| "todo".to_string());
                        let in_prog = phase_stories
                            .iter()
                            .filter(|v| {
                                v.story.superstate == SuperState::Open
                                    && v.story.state != default_open
                            })
                            .count();
                        let blocked_count = phase_stories
                            .iter()
                            .filter(|v| {
                                v.story.superstate == SuperState::Open
                                    && !is_ready(&v.story, &story_map)
                            })
                            .count();

                        // Look for a phase title from a grouping story
                        let title_suffix = phase_stories
                            .iter()
                            .find_map(|v| {
                                let prefix = format!("Phase {}:", phase_num);
                                if v.story.title.starts_with(&prefix) {
                                    let rest = v.story.title[prefix.len()..].trim();
                                    if !rest.is_empty() {
                                        return Some(format!(": {rest}"));
                                    }
                                }
                                None
                            })
                            .unwrap_or_default();

                        let mut status_parts = Vec::new();
                        status_parts.push(format!("{done_count}/{total} done"));
                        if in_prog > 0 {
                            status_parts.push(format!("{in_prog} in-progress"));
                        }
                        if blocked_count > 0 {
                            status_parts.push(format!("{blocked_count} blocked"));
                        }

                        body.push_str(&format!(
                            "- Phase {}{} -- {}\n",
                            phase_num,
                            title_suffix,
                            status_parts.join(", ")
                        ));
                    }
                }

                Ok(Response::Message(body))
            }
        }
        Invocation::Handoff { since } => {
            storage::ensure_project(root)?;
            let threshold = if let Some(ref duration_str) = since {
                let duration = parse_duration(duration_str).ok_or_else(|| {
                    AppError::Validation(format!(
                        "invalid duration `{duration_str}` (use e.g. 2h, 1d, 1w)"
                    ))
                })?;
                let cutoff = chrono::Utc::now() - duration;
                cutoff.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            } else {
                // Default: 24 hours
                let cutoff = chrono::Utc::now() - chrono::Duration::try_hours(24).unwrap();
                cutoff.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            };

            let all = storage::load_all_snapshots(root)?;
            let mut created = Vec::new();
            let mut updated = Vec::new();
            let mut closed = Vec::new();

            for story in &all {
                let in_window = story.updated_at.as_str() >= threshold.as_str();
                if !in_window {
                    continue;
                }
                if story.superstate == SuperState::Closed {
                    closed.push(story);
                } else if story.created_at.as_str() >= threshold.as_str() {
                    created.push(story);
                } else {
                    updated.push(story);
                }
            }

            let mut body = String::from("# Session Handoff\n\n");

            if !created.is_empty() {
                body.push_str(&format!("## Created ({})\n\n", created.len()));
                for story in &created {
                    body.push_str(&format!(
                        "- {} {} [{}]\n",
                        story.id, story.title, story.state
                    ));
                }
                body.push('\n');
            }

            if !updated.is_empty() {
                body.push_str(&format!("## Updated ({})\n\n", updated.len()));
                for story in &updated {
                    body.push_str(&format!(
                        "- {} {} [{}]\n",
                        story.id, story.title, story.state
                    ));
                }
                body.push('\n');
            }

            if !closed.is_empty() {
                body.push_str(&format!("## Closed ({})\n\n", closed.len()));
                for story in &closed {
                    body.push_str(&format!(
                        "- {} {} [{}]\n",
                        story.id, story.title, story.state
                    ));
                }
                body.push('\n');
            }

            if created.is_empty() && updated.is_empty() && closed.is_empty() {
                body.push_str("No changes in the specified period.\n");
            }

            Ok(Response::Message(body))
        }
        Invocation::Phase { action } => handle_phase(root, action, no_hooks),
        Invocation::Graph { mode } => {
            storage::ensure_project(root)?;
            let views = build_story_views(root, false)?;
            let story_map: BTreeMap<String, StorySnapshot> = views
                .iter()
                .map(|v| (v.story.id.clone(), v.story.clone()))
                .collect();
            let dep_graph = DependencyGraph::from_open_stories(&story_map);

            let graph_view = match mode {
                GraphMode::Overview => {
                    let groups = dep_graph.parallel_groups();
                    let path = dep_graph.critical_path();
                    let open_stories: Vec<&StorySnapshot> = story_map
                        .values()
                        .filter(|s| s.superstate == SuperState::Open)
                        .collect();
                    let total_open = open_stories.len();
                    // Count edges
                    let mut total_edges = 0;
                    for story in &open_stories {
                        for rel in &story.relationships {
                            if matches!(rel.relation.as_str(), "blocks" | "blocked-by")
                                && story_map
                                    .get(&rel.other_id)
                                    .is_some_and(|s| s.superstate == SuperState::Open)
                            {
                                total_edges += 1;
                            }
                        }
                    }
                    // Roots: open stories with no predecessors in dep graph
                    let roots: Vec<String> = open_stories
                        .iter()
                        .filter(|s| {
                            !s.relationships.iter().any(|r| {
                                r.relation == "blocked-by"
                                    && story_map
                                        .get(&r.other_id)
                                        .is_some_and(|o| o.superstate == SuperState::Open)
                            })
                        })
                        .map(|s| s.id.clone())
                        .collect();
                    let leaves: Vec<String> = open_stories
                        .iter()
                        .filter(|s| {
                            !s.relationships.iter().any(|r| {
                                r.relation == "blocks"
                                    && story_map
                                        .get(&r.other_id)
                                        .is_some_and(|o| o.superstate == SuperState::Open)
                            })
                        })
                        .map(|s| s.id.clone())
                        .collect();

                    GraphView {
                        critical_path: if path.len() > 1 { Some(path) } else { None },
                        blocked_chain: None,
                        parallel_groups: if groups.len() > 1 {
                            Some(
                                groups
                                    .into_iter()
                                    .map(|g| g.into_iter().collect())
                                    .collect(),
                            )
                        } else {
                            None
                        },
                        overview: Some(GraphOverview {
                            total_open,
                            total_edges,
                            roots,
                            leaves,
                        }),
                    }
                }
                GraphMode::CriticalPath => {
                    let path = dep_graph.critical_path();
                    GraphView {
                        critical_path: Some(path),
                        blocked_chain: None,
                        parallel_groups: None,
                        overview: None,
                    }
                }
                GraphMode::BlockedBy(id) => {
                    if !story_map.contains_key(&id) {
                        return Err(AppError::NotFound(format!("story `{id}` not found")));
                    }
                    let blocked = dep_graph.blocked_chain(&id);
                    GraphView {
                        critical_path: None,
                        blocked_chain: Some(BlockedChainView {
                            source: id,
                            blocked: blocked.into_iter().collect(),
                        }),
                        parallel_groups: None,
                        overview: None,
                    }
                }
                GraphMode::ParallelGroups => {
                    let groups = dep_graph.parallel_groups();
                    GraphView {
                        critical_path: None,
                        blocked_chain: None,
                        parallel_groups: Some(
                            groups
                                .into_iter()
                                .map(|g| g.into_iter().collect())
                                .collect(),
                        ),
                        overview: None,
                    }
                }
            };

            Ok(Response::Graph(Box::new(graph_view)))
        }
        Invocation::Scaffold { kind } => {
            let template = match kind.as_str() {
                "agents-md" => generate_agents_md(root),
                "claude-md" => generate_claude_md(root),
                "cursor-rules" => generate_cursor_rules(),
                _ => {
                    return Err(AppError::Usage(
                        "usage: story scaffold agents-md|claude-md|cursor-rules".to_string(),
                    ));
                }
            };
            Ok(Response::Message(template))
        }
        Invocation::Hooks { action } => match action {
            HooksAction::Install => {
                let msg = crate::hooks::install_hooks(root)?;
                Ok(Response::Message(msg))
            }
            HooksAction::Uninstall => {
                let msg = crate::hooks::uninstall_hooks(root)?;
                Ok(Response::Message(msg))
            }
            HooksAction::List => {
                let msg = crate::event_hooks::list_hooks(root);
                Ok(Response::Message(msg))
            }
            HooksAction::Test { event_type } => {
                storage::ensure_project(root)?;
                let msg = crate::event_hooks::test_hook(root, &event_type)?;
                Ok(Response::Message(msg))
            }
        },
        Invocation::Relate {
            a,
            relation,
            b,
            remove,
        } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            ensure_open_story(root, &a)?;
            ensure_open_story(root, &b)?;
            if a == b {
                return Err(AppError::Validation(
                    "stories cannot relate to themselves".to_string(),
                ));
            }
            let a_story = storage::load_open_story_snapshot(root, &a)?;
            let b_story = storage::load_open_story_snapshot(root, &b)?;
            let stories = load_story_map(root)?;

            if !remove {
                validate_parent_constraints(&stories, &a, &b, &relation, &a_story, &b_story)?;
            }

            let edges = relation_edges(&relation).ok_or_else(|| {
                AppError::Validation(format!("unsupported relationship `{relation}`"))
            })?;

            let now = storage::now();
            let mut a_events = Vec::new();
            let mut b_events = Vec::new();
            let mut changed = false;

            for (a_relation, b_relation) in edges {
                let a_has = has_relation(&a_story, a_relation, &b);
                let b_has = has_relation(&b_story, b_relation, &a);

                if remove {
                    if a_has {
                        a_events.push(StoryEvent::StoryRelationshipRemoved {
                            at: now.clone(),
                            other_id: b.clone(),
                            relation: a_relation.to_string(),
                        });
                        changed = true;
                    }
                    if b_has {
                        b_events.push(StoryEvent::StoryRelationshipRemoved {
                            at: now.clone(),
                            other_id: a.clone(),
                            relation: b_relation.to_string(),
                        });
                        changed = true;
                    }
                } else {
                    if !a_has {
                        a_events.push(StoryEvent::StoryRelationshipAdded {
                            at: now.clone(),
                            other_id: b.clone(),
                            relation: a_relation.to_string(),
                        });
                        changed = true;
                    }
                    if !b_has {
                        b_events.push(StoryEvent::StoryRelationshipAdded {
                            at: now.clone(),
                            other_id: a.clone(),
                            relation: b_relation.to_string(),
                        });
                        changed = true;
                    }
                }
            }

            if !changed {
                let action = if remove { "removed" } else { "added" };
                return Ok(Response::Message(format!(
                    "no changes; relationship already {action}"
                )));
            }

            if !a_events.is_empty() {
                storage::write_story_events(root, &a, &a_events)?;
            }
            if !b_events.is_empty() {
                storage::write_story_events(root, &b, &b_events)?;
            }

            if !no_hooks && let Some(ref config) = crate::event_hooks::load_hooks_config(root) {
                let snapshot_a = storage::load_open_story_snapshot(root, &a)?;
                let payload = serde_json::json!({
                    "event_type": "relationship_change",
                    "story_id": &a,
                    "timestamp": crate::storage::now(),
                    "story_title": &snapshot_a.title,
                    "action": if remove { "removed" } else { "added" },
                    "relation": &relation,
                    "other_id": &b
                });
                crate::event_hooks::fire_hook(
                    root,
                    config,
                    crate::event_hooks::HookEventType::RelationshipChange,
                    &payload.to_string(),
                );
            }

            story_view_by_id(root, &a)
        }),
        Invocation::GithubSync { id, dry_run } => {
            #[cfg(feature = "github-sync")]
            {
                Ok(lock::with_project_lock(root, || {
                    storage::ensure_project(root)?;
                    crate::github::run_sync(root, id.as_deref(), dry_run)
                })?)
            }
            #[cfg(not(feature = "github-sync"))]
            {
                let _ = (id, dry_run);
                return Err(AppError::Usage(
                    "github-sync requires the `github-sync` feature. \
                     Rebuild with: cargo install storyhook --features github-sync"
                        .to_string(),
                ));
            }
        }
        Invocation::CommitSync { since } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;

            // 1. Check we're in a git repo
            let git_check = std::process::Command::new("git")
                .args(["rev-parse", "--git-dir"])
                .current_dir(root)
                .output();
            match git_check {
                Ok(output) if output.status.success() => {}
                _ => {
                    return Err(AppError::Validation("not a git repository".to_string()));
                }
            }

            // 2. Load prefix and auto-transition config
            let prefix = storage::load_project_prefix(root)?;
            let active_state = storage::find_active_state(root)?;
            let default_open = storage::default_open_state(root)?;
            let auto_transition_enabled = storage::is_auto_transition_enabled(root)?;
            let mut transitions: Vec<(String, String, String, String)> = Vec::new(); // (story_id, from, to, short_hash)

            // 3. Compute --since value
            let since_str = since.as_deref().unwrap_or("7d");
            let duration = parse_duration(since_str).ok_or_else(|| {
                AppError::Validation(format!(
                    "invalid duration `{since_str}` (use e.g. 2h, 1d, 1w)"
                ))
            })?;
            let since_date =
                (chrono::Utc::now() - duration).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

            // 4. Run git log
            let output = std::process::Command::new("git")
                .args(["log", "--format=%H %s", &format!("--since={since_date}")])
                .current_dir(root)
                .output()
                .map_err(|e| AppError::Storage(format!("failed to run git: {e}")))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(AppError::Storage(format!("git log failed: {stderr}")));
            }

            let log_output = String::from_utf8_lossy(&output.stdout);

            // 5. Parse output, extract story IDs, add comments
            let mut commits_scanned: usize = 0;
            let mut comments_added: usize = 0;
            let mut stories_touched: BTreeSet<String> = BTreeSet::new();

            for line in log_output.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                commits_scanned += 1;

                let (hash, message) = match line.split_once(' ') {
                    Some((h, m)) => (h, m),
                    None => continue,
                };

                let short_hash = &hash[..7.min(hash.len())];
                let story_ids = extract_story_ids(&prefix, message);

                for story_id in story_ids {
                    // Check if the story is open
                    if !storage::open_story_exists(root, &story_id) {
                        continue;
                    }

                    // Check idempotency: look for existing [git] comment with this short hash
                    let events = storage::load_open_story_events(root, &story_id)?;
                    let comment_prefix = format!("[git] {short_hash}:");
                    let already_exists = events.iter().any(|event| {
                        matches!(event, StoryEvent::StoryCommentAdded { text, .. } if text.starts_with(&comment_prefix))
                    });

                    if already_exists {
                        continue;
                    }

                    // Add comment
                    let comment_text = format!("[git] {short_hash}: {message}");
                    storage::write_story_events(
                        root,
                        &story_id,
                        &[StoryEvent::StoryCommentAdded {
                            at: storage::now(),
                            text: comment_text,
                        }],
                    )?;
                    comments_added += 1;
                    stories_touched.insert(story_id.clone());

                    // Auto-transition from initial state to active state
                    if auto_transition_enabled && let Some(ref active) = active_state {
                        let snapshot = storage::load_open_story_snapshot(root, &story_id)?;
                        if snapshot.state == default_open.slug
                            && !transitions.iter().any(|(tid, _, _, _)| tid == &story_id)
                        {
                            storage::write_story_events(
                                root,
                                &story_id,
                                &[StoryEvent::StoryStateChanged {
                                    at: storage::now(),
                                    state: active.slug.clone(),
                                }],
                            )?;
                            transitions.push((
                                story_id.clone(),
                                default_open.slug.clone(),
                                active.slug.clone(),
                                short_hash.to_string(),
                            ));
                        }
                    }
                }
            }

            let mut msg = format!(
                "scanned {} commits, added {} comments to {} stories",
                commits_scanned,
                comments_added,
                stories_touched.len()
            );
            for (id, from, to, hash) in &transitions {
                msg.push_str(&format!(
                    "\n{id}: {from} \u{2192} {to} (referenced in {hash})"
                ));
            }
            Ok(Response::Message(msg))
        }),
        Invocation::HelpTopic { topic } => match crate::help_topics::get_help_topic(&topic) {
            Some(text) => Ok(Response::Message(text.to_string())),
            None => {
                let topics = crate::help_topics::list_topics();
                Err(AppError::Usage(format!(
                    "unknown help topic `{topic}`. Available: {}",
                    topics.join(", ")
                )))
            }
        },
        Invocation::HelpCompact => Ok(Response::Message(
            crate::help_topics::compact_reference().to_string(),
        )),
        Invocation::HelpAll => Ok(Response::Message(crate::help_topics::all_topics_text())),
        Invocation::SetFields {
            id,
            title,
            state,
            priority,
            assignee,
            labels,
            blocked,
            unblocked,
            json: json_patch,
            story_type,
            description,
        } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            ensure_open_story(root, &id)?;
            let story = storage::load_open_story_snapshot(root, &id)?;
            let now = storage::now();
            let mut events: Vec<StoryEvent> = Vec::new();
            let mut changes: Vec<String> = Vec::new();

            // Shared by the `--assignee` flag and the `--json "assignee"` key so both
            // surfaces reject an unknown member the same way (see issue #39).
            let resolve_assignee = |lookup: &str| -> Result<String, AppError> {
                let members = storage::load_members(root)?;
                members
                    .iter()
                    .find(|m| m.id == lookup || m.github.as_deref() == Some(lookup))
                    .map(|m| m.id.clone())
                    .ok_or_else(|| AppError::Validation(format!("member `{lookup}` not found")))
            };

            if let Some(ref t) = title {
                events.push(StoryEvent::StoryTitleSet {
                    at: now.clone(),
                    title: t.clone(),
                });
                changes.push(format!("title -> {t}"));
            }
            if let Some(ref s) = state {
                let states = storage::load_state_map(root)?;
                let state_def = states
                    .get(s)
                    .ok_or_else(|| AppError::Validation(format!("state `{s}` is not defined")))?;
                events.extend(storage::state_transition_events(
                    state_def,
                    story.awaiting.is_some(),
                    &now,
                    Vec::new(),
                ));
                changes.push(format!("state -> {s}"));
            }
            if let Some(ref p) = priority {
                let level = Priority::parse(p)
                    .ok_or_else(|| AppError::Validation(format!("invalid priority `{p}`")))?;
                events.push(StoryEvent::StoryPrioritySet {
                    at: now.clone(),
                    priority: level,
                });
                changes.push(format!("priority -> {p}"));
            }
            if let Some(ref a) = assignee {
                let member_id = resolve_assignee(a)?;
                events.push(StoryEvent::StoryAssigned {
                    at: now.clone(),
                    member_id,
                });
                changes.push(format!("assignee -> {a}"));
            }
            if let Some(ref l) = labels {
                let add: Vec<String> = l
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                let mut current_labels: BTreeSet<String> = story.labels.iter().cloned().collect();
                for label in &add {
                    current_labels.insert(label.clone());
                }
                events.push(StoryEvent::StoryLabelsSet {
                    at: now.clone(),
                    labels: current_labels.into_iter().collect(),
                });
                changes.push(format!("labels += {l}"));
            }
            if let Some(ref reason) = blocked {
                events.push(StoryEvent::StoryAwaitingSet {
                    at: now.clone(),
                    awaiting: reason.clone(),
                });
                changes.push(format!("blocked: {reason}"));
            }
            if unblocked {
                events.push(StoryEvent::StoryAwaitingCleared { at: now.clone() });
                changes.push("unblocked".to_string());
            }
            if let Some(ref st) = story_type {
                let type_map = storage::load_type_map(root)?;
                if !type_map.contains_key(st.as_str()) {
                    return Err(AppError::Validation(format!(
                        "unknown type `{st}`. Available types: {}",
                        type_map.keys().cloned().collect::<Vec<_>>().join(", ")
                    )));
                }
                events.push(StoryEvent::StoryTypeSet {
                    at: now.clone(),
                    story_type: st.clone(),
                });
                changes.push(format!("type -> {st}"));
            }
            if let Some(ref d) = description {
                events.push(StoryEvent::StoryDescriptionSet {
                    at: now.clone(),
                    description: d.clone(),
                });
                changes.push("description updated".to_string());
            }
            if let Some(ref j) = json_patch {
                let patch: serde_json::Value = serde_json::from_str(j)
                    .map_err(|e| AppError::Validation(format!("invalid JSON: {e}")))?;
                let obj = patch
                    .as_object()
                    .ok_or_else(|| AppError::Validation("JSON must be an object".to_string()))?;
                for (key, value) in obj {
                    match key.as_str() {
                        "title" => {
                            let v = value.as_str().ok_or_else(|| {
                                AppError::Validation("title must be a string".to_string())
                            })?;
                            if !v.is_empty() {
                                events.push(StoryEvent::StoryTitleSet {
                                    at: now.clone(),
                                    title: v.to_string(),
                                });
                                changes.push(format!("title -> {v}"));
                            }
                        }
                        "state" => {
                            let v = value.as_str().ok_or_else(|| {
                                AppError::Validation("state must be a string".to_string())
                            })?;
                            let states = storage::load_state_map(root)?;
                            let state_def = states.get(v).ok_or_else(|| {
                                AppError::Validation(format!("state `{v}` is not defined"))
                            })?;
                            events.extend(storage::state_transition_events(
                                state_def,
                                story.awaiting.is_some(),
                                &now,
                                Vec::new(),
                            ));
                            changes.push(format!("state -> {v}"));
                        }
                        "priority" => {
                            let v = value.as_str().ok_or_else(|| {
                                AppError::Validation("priority must be a string".to_string())
                            })?;
                            let level = Priority::parse(v).ok_or_else(|| {
                                AppError::Validation(format!("invalid priority `{v}`"))
                            })?;
                            events.push(StoryEvent::StoryPrioritySet {
                                at: now.clone(),
                                priority: level,
                            });
                            changes.push(format!("priority -> {v}"));
                        }
                        "assignee" => {
                            if value.is_null() {
                                changes.push("assignee cleared".to_string());
                            } else if let Some(v) = value.as_str() {
                                if v.is_empty() {
                                    changes.push("assignee cleared".to_string());
                                } else {
                                    let member_id = resolve_assignee(v)?;
                                    events.push(StoryEvent::StoryAssigned {
                                        at: now.clone(),
                                        member_id,
                                    });
                                    changes.push(format!("assignee -> {v}"));
                                }
                            } else {
                                return Err(AppError::Validation(
                                    "assignee must be a string or null".to_string(),
                                ));
                            }
                        }
                        "labels" => {
                            let arr = value.as_array().ok_or_else(|| {
                                AppError::Validation(
                                    "labels must be an array of strings".to_string(),
                                )
                            })?;
                            let new_labels: Vec<String> = arr
                                .iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect();
                            events.push(StoryEvent::StoryLabelsSet {
                                at: now.clone(),
                                labels: new_labels.clone(),
                            });
                            changes.push(format!("labels -> [{}]", new_labels.join(", ")));
                        }
                        "blocked" => {
                            if value.is_null() {
                                events.push(StoryEvent::StoryAwaitingCleared { at: now.clone() });
                                changes.push("unblocked".to_string());
                            } else if let Some(v) = value.as_str() {
                                if v.is_empty() {
                                    events
                                        .push(StoryEvent::StoryAwaitingCleared { at: now.clone() });
                                    changes.push("unblocked".to_string());
                                } else {
                                    events.push(StoryEvent::StoryAwaitingSet {
                                        at: now.clone(),
                                        awaiting: v.to_string(),
                                    });
                                    changes.push(format!("blocked: {v}"));
                                }
                            } else {
                                return Err(AppError::Validation(
                                    "blocked must be a string or null".to_string(),
                                ));
                            }
                        }
                        "story_type" => {
                            let v = value.as_str().ok_or_else(|| {
                                AppError::Validation("story_type must be a string".to_string())
                            })?;
                            let type_map = storage::load_type_map(root)?;
                            if !type_map.contains_key(v) {
                                return Err(AppError::Validation(format!(
                                    "unknown type `{v}`. Available types: {}",
                                    type_map.keys().cloned().collect::<Vec<_>>().join(", ")
                                )));
                            }
                            events.push(StoryEvent::StoryTypeSet {
                                at: now.clone(),
                                story_type: v.to_string(),
                            });
                            changes.push(format!("type -> {v}"));
                        }
                        "description" => {
                            let v = value.as_str().ok_or_else(|| {
                                AppError::Validation("description must be a string".to_string())
                            })?;
                            events.push(StoryEvent::StoryDescriptionSet {
                                at: now.clone(),
                                description: v.to_string(),
                            });
                            changes.push("description updated".to_string());
                        }
                        other => {
                            return Err(AppError::Validation(format!(
                                "unknown field `{other}` in JSON. Valid fields: title, state, priority, assignee, labels, blocked, story_type, description"
                            )));
                        }
                    }
                }
            }

            if events.is_empty() {
                return Err(AppError::Usage("no fields to update".to_string()));
            }

            storage::write_story_events(root, &id, &events)?;

            // Archive if state changed to CLOSED
            let needs_archive = state
                .as_ref()
                .map(|s| {
                    storage::load_state_map(root)
                        .ok()
                        .and_then(|m| m.get(s).map(|d| d.super_state == SuperState::Closed))
                        .unwrap_or(false)
                })
                .unwrap_or(false)
                || json_patch
                    .as_ref()
                    .map(|j| {
                        serde_json::from_str::<serde_json::Value>(j)
                            .ok()
                            .and_then(|v| v.get("state")?.as_str().map(|s| s.to_string()))
                            .map(|s| {
                                storage::load_state_map(root)
                                    .ok()
                                    .and_then(|m| {
                                        m.get(&s).map(|d| d.super_state == SuperState::Closed)
                                    })
                                    .unwrap_or(false)
                            })
                            .unwrap_or(false)
                    })
                    .unwrap_or(false);

            if needs_archive {
                storage::archive_story(root, &id)?;
            }

            Ok(Response::Message(format!(
                "updated {id}: {}",
                changes.join(", ")
            )))
        }),
        Invocation::Plugin { action } => match action {
            PluginAction::Install { target } => {
                let msg = crate::plugin::install(&target, root)?;
                Ok(Response::Message(msg))
            }
            PluginAction::Uninstall { target } => {
                let msg = crate::plugin::uninstall(&target, root)?;
                Ok(Response::Message(msg))
            }
        },
        Invocation::Type { action } => match action {
            TypeAction::List => {
                storage::ensure_project(root)?;
                let types = storage::load_types(root)?;
                let lines: Vec<String> = types
                    .iter()
                    .map(|t| {
                        if let Some(ref desc) = t.description {
                            format!("{} — {desc}", t.slug)
                        } else {
                            t.slug.clone()
                        }
                    })
                    .collect();
                Ok(Response::Message(lines.join("\n")))
            }
            TypeAction::Add { slug, description } => lock::with_project_lock(root, || {
                storage::ensure_project(root)?;
                let type_def = storage::add_type(root, &slug, description.as_deref())?;
                Ok(Response::Message(format!("added type {}", type_def.slug)))
            }),
            TypeAction::Remove { slug } => lock::with_project_lock(root, || {
                storage::ensure_project(root)?;
                storage::remove_type(root, &slug)?;
                Ok(Response::Message(format!("removed type {slug}")))
            }),
        },
        Invocation::Epic { action } => match action {
            EpicAction::Create { title } => lock::with_project_lock(root, || {
                storage::ensure_project(root)?;
                // Validate that "epic" type exists
                let type_map = storage::load_type_map(root)?;
                if !type_map.contains_key("epic") {
                    return Err(AppError::Validation(
                        "type `epic` is not defined. Add it with: story type add epic".to_string(),
                    ));
                }
                // Create story + set type to epic in a single lock
                let story = storage::create_story(root, &title, None)?;
                storage::write_story_events(
                    root,
                    &story.id,
                    &[StoryEvent::StoryTypeSet {
                        at: storage::now(),
                        story_type: "epic".to_string(),
                    }],
                )?;
                story_view_response(root, story)
            }),
            EpicAction::Add { epic_id, story_id } => lock::with_project_lock(root, || {
                storage::ensure_project(root)?;
                ensure_open_story(root, &epic_id)?;
                ensure_open_story(root, &story_id)?;
                if epic_id == story_id {
                    return Err(AppError::Validation(
                        "stories cannot relate to themselves".to_string(),
                    ));
                }
                let a_story = storage::load_open_story_snapshot(root, &epic_id)?;
                let b_story = storage::load_open_story_snapshot(root, &story_id)?;
                let stories = load_story_map(root)?;

                validate_parent_constraints(
                    &stories,
                    &epic_id,
                    &story_id,
                    "parent-of",
                    &a_story,
                    &b_story,
                )?;

                let edges = relation_edges("parent-of").ok_or_else(|| {
                    AppError::Validation("unsupported relationship `parent-of`".to_string())
                })?;

                let now = storage::now();
                let mut a_events = Vec::new();
                let mut b_events = Vec::new();

                for (a_relation, b_relation) in edges {
                    if !has_relation(&a_story, a_relation, &story_id) {
                        a_events.push(StoryEvent::StoryRelationshipAdded {
                            at: now.clone(),
                            other_id: story_id.clone(),
                            relation: a_relation.to_string(),
                        });
                    }
                    if !has_relation(&b_story, b_relation, &epic_id) {
                        b_events.push(StoryEvent::StoryRelationshipAdded {
                            at: now.clone(),
                            other_id: epic_id.clone(),
                            relation: b_relation.to_string(),
                        });
                    }
                }

                if !a_events.is_empty() {
                    storage::write_story_events(root, &epic_id, &a_events)?;
                }
                if !b_events.is_empty() {
                    storage::write_story_events(root, &story_id, &b_events)?;
                }

                story_view_by_id(root, &epic_id)
            }),
            EpicAction::List => {
                storage::ensure_project(root)?;
                let mut views = build_story_views(root, false)?;
                views.retain(|view| view.story.story_type.as_deref() == Some("epic"));
                sort_story_views(&mut views);
                Ok(Response::Stories(views, None))
            }
            EpicAction::Show { id } => {
                storage::ensure_project(root)?;
                story_view_by_id(root, &id)
            }
        },
        Invocation::Web { action } => match action {
            WebAction::Start { port } => {
                let msg = crate::web::handle_start(port)?;
                Ok(Response::Message(msg))
            }
            WebAction::Stop => {
                let msg = crate::web::handle_stop()?;
                Ok(Response::Message(msg))
            }
            WebAction::Status => {
                let msg = crate::web::handle_status()?;
                Ok(Response::Message(msg))
            }
            // A relative `path` resolves against this process's actual
            // working directory (same as any other relative CLI path
            // argument) via `Path::canonicalize` inside `Registry::register`
            // — `root` itself is exactly that directory, so no manual join
            // against it is needed here.
            WebAction::Register { path, name } => {
                let msg = crate::web::handle_register(&path, name.as_deref())?;
                Ok(Response::Message(msg))
            }
            // Registry-only: works from anywhere, not just inside a project.
            WebAction::Deregister { target } => {
                let msg = crate::web::handle_deregister(&target)?;
                Ok(Response::Message(msg))
            }
            WebAction::List => {
                let msg = crate::web::handle_list()?;
                Ok(Response::Message(msg))
            }
            // Registry-only: works from anywhere, not just inside a project.
            WebAction::Open => {
                let msg = crate::web::handle_open()?;
                Ok(Response::Message(msg))
            }
            WebAction::Address => {
                let msg = crate::web::handle_address()?;
                Ok(Response::Message(msg))
            }
            WebAction::Serve { .. } => {
                // Handled in main.rs before app::run
                unreachable!("web --serve is dispatched in main.rs")
            }
        },
        Invocation::SessionStart => {
            return session_start(root);
        }
        Invocation::Version => Ok(Response::Message(format!(
            "story {}",
            env!("CARGO_PKG_VERSION")
        ))),
        // The two seam-only invocations, served over legacy storage so that
        // `LegacyInvoker` answers them before the flip. Neither is reachable
        // from the command line; both exist because a client that holds a
        // model — the TUI today, the dashboard's resync later — needs a bulk
        // read and an undo primitive that no CLI verb provides.
        Invocation::ProjectSnapshot => {
            storage::ensure_project(root)?;
            Ok(Response::ProjectSnapshot(Box::new(ProjectSnapshotView {
                prefix: storage::load_project_prefix(root)?,
                states: storage::load_states(root)?,
                members: storage::load_members(root)?,
                stories: storage::load_open_snapshots_tolerant(root)?,
            })))
        }
        Invocation::History { action } => match action {
            HistoryAction::Read { id } => {
                storage::ensure_project(root)?;
                Ok(Response::StoryHistory(
                    storage::load_open_story_events(root, &id).unwrap_or_default(),
                ))
            }
            HistoryAction::Restore { id, events } => lock::with_project_lock(root, || {
                storage::ensure_project(root)?;
                // An empty history means "this story should not exist":
                // undoing a creation. The legacy representation of a story
                // that does not exist is a missing file.
                if events.is_empty() {
                    let path = storage::ProjectPaths::new(root).open_story_file(&id);
                    if path.exists() {
                        std::fs::remove_file(&path)?;
                    }
                } else {
                    storage::rewrite_story_events(root, &id, &events)?;
                }
                Ok(Response::Message(format!("restored {id}")))
            }),
        },
        Invocation::Update { check, force } => {
            #[cfg(feature = "github-sync")]
            {
                Ok(Response::Message(crate::update::run(check, force)?))
            }
            #[cfg(not(feature = "github-sync"))]
            {
                let _ = (check, force);
                return Err(AppError::Usage(
                    "self-update requires the `github-sync` feature. \
                     Reinstall via the official installer \
                     (curl -fsSL https://raw.githubusercontent.com/mikeydotio/storyhook/main/install.sh | sh) \
                     or rebuild with: cargo install storyhook --features github-sync"
                        .to_string(),
                ));
            }
        }
    };

    // After successful command, maybe auto-sync to GitHub
    #[cfg(feature = "github-sync")]
    if let Ok(ref resp) = response
        && !no_hooks
    {
        let created_id = extract_created_story_id(resp);
        crate::github::auto::maybe_auto_sync(root, &invocation_clone, created_id.as_deref());
    }

    response
}

/// Extract the story ID from a successful `New` command response.
#[cfg(feature = "github-sync")]
fn extract_created_story_id(response: &Response) -> Option<String> {
    if let Response::Story(view) = response {
        Some(view.story.id.clone())
    } else {
        None
    }
}

/// Determines whether the plugin is disabled based on TOML config content.
///
/// Supports two config formats:
/// - Bare key: `enabled = false` or `enabled = "false"`
/// - Nested table: `[plugin]\nenabled = false`
///
/// Returns `true` if the plugin is explicitly disabled.
/// Returns `false` (fail-open) if the content is malformed or enabled is absent/true.
fn plugin_config_disabled(content: &str) -> bool {
    #[derive(serde::Deserialize)]
    struct PluginTable {
        enabled: Option<toml::Value>,
    }

    #[derive(serde::Deserialize)]
    struct PluginConfig {
        enabled: Option<toml::Value>,
        plugin: Option<PluginTable>,
    }

    fn is_disabled(val: &toml::Value) -> bool {
        match val {
            toml::Value::Boolean(b) => !b,
            toml::Value::String(s) => s.eq_ignore_ascii_case("false"),
            _ => false,
        }
    }

    let config: PluginConfig = match toml::from_str(content) {
        Ok(c) => c,
        Err(_) => return false, // malformed → fail open (treat as enabled)
    };

    // Check nested [plugin].enabled first, then top-level enabled
    if let Some(ref plugin) = config.plugin
        && let Some(ref val) = plugin.enabled
    {
        return is_disabled(val);
    }
    if let Some(ref val) = config.enabled {
        return is_disabled(val);
    }

    false // no enabled key found → treat as enabled
}

/// Wrap a session-start context string in the Claude Code SessionStart hook
/// envelope. `additionalContext` is injected silently into Claude's context and
/// is *not* rendered as a user-visible block (unlike `systemMessage`), so the
/// CLI reference and project state prime the model without spamming the user.
fn session_context_json(msg: String) -> Response {
    let json = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": msg,
        }
    });
    Response::RawJson(json.to_string())
}

/// Handle `story session-start`. Outputs raw JSON suitable for shell hooks.
/// Returns `{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"..."}}`
/// when a project exists and the plugin is enabled, or `{}` otherwise.
fn session_start(root: &Path) -> Result<Response, AppError> {
    let storyhook_dir = root.join(".storyhook");

    // No project → {}
    if !storyhook_dir.exists() {
        return Ok(Response::RawJson("{}".to_string()));
    }

    // Check plugin config — disabled → {}
    let config_path = storyhook_dir.join("plugin-config.toml");
    if config_path.exists()
        && let Ok(content) = std::fs::read_to_string(&config_path)
        && plugin_config_disabled(&content)
    {
        return Ok(Response::RawJson("{}".to_string()));
    }

    // Build the system message
    let mut msg = String::new();

    // 1. Compact CLI reference
    msg.push_str(crate::help_topics::compact_reference());

    // 2. Project state
    msg.push_str("PROJECT STATE\n");

    let open_stories = match storage::load_all_open_snapshots(root) {
        Ok(stories) => stories,
        Err(_) => {
            // If we can't load stories, still output CLI reference
            msg.push_str("  Unable to load project state.\n");
            return Ok(session_context_json(msg));
        }
    };

    let story_map: BTreeMap<String, StorySnapshot> = open_stories
        .iter()
        .map(|s| (s.id.clone(), s.clone()))
        .collect();

    let open_count = open_stories.len();
    let ready_stories: Vec<&StorySnapshot> = open_stories
        .iter()
        .filter(|s| is_ready(s, &story_map) && !has_children(s))
        .collect();
    let ready_count = ready_stories.len();

    msg.push_str(&format!(
        "  {open_count} open stories, {ready_count} ready\n"
    ));

    // Find the highest-priority ready story for "Next" info
    if !ready_stories.is_empty() {
        let mut sorted_ready: Vec<&StorySnapshot> = ready_stories;
        sorted_ready.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.created_at.cmp(&b.created_at))
        });
        let next = sorted_ready[0];
        let pri = if next.priority != Priority::None {
            format!(" ({})", next.priority.as_str())
        } else {
            String::new()
        };
        msg.push_str(&format!("  Next: {} — {}{}\n", next.id, next.title, pri));
    }

    // Truncate to under 4000 characters if needed
    if msg.len() > 3900 {
        // Find the largest char boundary at or below 3900 so we never split a
        // multi-byte UTF-8 character (avoids `floor_char_boundary`, MSRV 1.91).
        let mut end = 3900;
        while end > 0 && !msg.is_char_boundary(end) {
            end -= 1;
        }
        msg.truncate(end);
        msg.push_str("\n...(truncated)\n");
    }

    Ok(session_context_json(msg))
}

fn build_member(root: &Path, input: MemberInput) -> Result<Member, AppError> {
    let now = storage::now();
    let member = match input {
        MemberInput::Github(handle) => Member {
            id: slugify(&handle),
            display_name: handle.clone(),
            email: None,
            github: Some(handle),
            created_at: now,
        },
        MemberInput::Identity(identity) => {
            let trimmed = identity.trim();
            if let Some((name, email)) = parse_identity(trimmed) {
                Member {
                    id: slugify(name),
                    display_name: name.to_string(),
                    email: Some(email.to_string()),
                    github: None,
                    created_at: now,
                }
            } else {
                Member {
                    id: slugify(trimmed),
                    display_name: trimmed.to_string(),
                    email: None,
                    github: None,
                    created_at: now,
                }
            }
        }
    };

    let existing = storage::load_members(root)?;
    if existing.iter().any(|candidate| candidate.id == member.id) {
        return Err(AppError::Validation(format!(
            "member `{}` already exists",
            member.id
        )));
    }

    Ok(member)
}

fn parse_identity(input: &str) -> Option<(&str, &str)> {
    let start = input.find('<')?;
    let end = input.rfind('>')?;
    if start >= end {
        return None;
    }
    let name = input[..start].trim();
    let email = input[start + 1..end].trim();
    if name.is_empty() || email.is_empty() {
        return None;
    }
    Some((name, email))
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;

    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }

    slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "member".to_string()
    } else {
        slug
    }
}

fn ensure_open_story(root: &Path, id: &str) -> Result<(), AppError> {
    if storage::open_story_exists(root, id) {
        return Ok(());
    }

    if storage::is_archived(root, id)? {
        return Err(AppError::Validation(format!(
            "story `{id}` is closed and cannot be modified"
        )));
    }

    Err(AppError::NotFound(format!("story `{id}` not found")))
}

fn load_story_map(root: &Path) -> Result<BTreeMap<String, StorySnapshot>, AppError> {
    let open = storage::load_all_open_snapshots(root)?;
    let archived = storage::load_all_archived_snapshots(root)?;
    let mut stories = BTreeMap::new();

    for story in open {
        stories.insert(story.id.clone(), story);
    }

    for story in archived {
        stories.entry(story.id.clone()).or_insert(story);
    }

    Ok(stories)
}

fn build_story_views(root: &Path, include_derived: bool) -> Result<Vec<StoryView>, AppError> {
    let open = storage::load_all_open_snapshots(root)?;
    let archived = storage::load_all_archived_snapshots(root)?;
    let mut stories = BTreeMap::new();
    let mut duplicates = BTreeSet::new();

    for story in open {
        stories.insert(story.id.clone(), story);
    }

    for story in archived {
        if stories.contains_key(&story.id) {
            duplicates.insert(story.id.clone());
        } else {
            stories.insert(story.id.clone(), story);
        }
    }

    let mut issues = compute_integrity_issues(&stories);
    let derived_relationships = if include_derived {
        derive_family_relationships(&stories)
    } else {
        BTreeMap::new()
    };
    for duplicate in duplicates {
        issues
            .entry(duplicate)
            .or_default()
            .push("story exists in both open and archive storage".to_string());
    }

    let progress_map: BTreeMap<String, _> = stories
        .values()
        .filter_map(|story| compute_progress(story, &stories).map(|p| (story.id.clone(), p)))
        .collect();

    let mut views = Vec::new();
    for story in stories.into_values() {
        let story_id = story.id.clone();
        let mut flagged_reasons = issues.remove(&story_id).unwrap_or_default();
        if story
            .relationships
            .iter()
            .any(|relation| relation.relation == "obviated-by")
        {
            flagged_reasons.push("story is obviated by another story".to_string());
        }
        flagged_reasons.sort();
        flagged_reasons.dedup();

        views.push(StoryView {
            story,
            derived_relationships: derived_relationships
                .get(&story_id)
                .cloned()
                .unwrap_or_default(),
            warnings: Vec::new(),
            flagged_reasons,
            stale_info: None,
            progress: progress_map.get(&story_id).cloned(),
        });
    }

    Ok(views)
}

pub fn build_report_data(root: &Path) -> Result<ReportData, AppError> {
    storage::ensure_project(root)?;
    let views = build_story_views(root, false)?;
    let story_map: BTreeMap<String, StorySnapshot> = views
        .iter()
        .map(|v| (v.story.id.clone(), v.story.clone()))
        .collect();

    let total_open = views
        .iter()
        .filter(|v| v.story.superstate == SuperState::Open)
        .count();
    let total_closed = views.len() - total_open;

    let mut state_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut priority_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut type_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut blocked_count = 0;
    let mut flagged_count = 0;

    let mut ready_ids: Vec<String> = Vec::new();
    let mut blocked_ids: Vec<String> = Vec::new();

    for view in &views {
        *state_counts.entry(view.story.state.clone()).or_default() += 1;
        if view.story.priority != Priority::None {
            *priority_counts
                .entry(view.story.priority.as_str().to_string())
                .or_default() += 1;
        }
        let type_label = view
            .story
            .story_type
            .as_deref()
            .unwrap_or("Default")
            .to_string();
        *type_counts.entry(type_label).or_default() += 1;
        if !view.flagged_reasons.is_empty() {
            flagged_count += 1;
        }
        if view.story.superstate == SuperState::Open {
            if is_ready(&view.story, &story_map) {
                ready_ids.push(view.story.id.clone());
            } else {
                blocked_count += 1;
                blocked_ids.push(view.story.id.clone());
            }
        }
    }

    let ready_count = ready_ids.len();

    let by_state: Vec<(String, usize)> = state_counts.into_iter().collect();
    let by_priority: Vec<(String, usize)> = priority_counts.into_iter().collect();
    let by_type: Vec<(String, usize)> = type_counts.into_iter().collect();

    let summary = SummaryView {
        total_open,
        total_closed,
        by_state,
        by_priority,
        by_type,
        blocked_count,
        flagged_count,
        ready_count,
        ready_stories: Vec::new(),
    };

    Ok(ReportData {
        summary,
        stories: views,
        ready_ids,
        blocked_ids,
    })
}

/// Guards `story reopen` on a soft-deleted story (undelete). At an
/// interactive terminal, warns and asks for confirmation, returning whether
/// the user agreed to proceed. Non-interactively (pipes, scripts, tests)
/// there is no one to prompt, so it fails outright and tells the caller to
/// pass `--force` instead of hanging on a read that will never resolve.
fn confirm_undelete(id: &str, reason: Option<&str>) -> Result<bool, AppError> {
    use std::io::{IsTerminal, Write};

    let reason = reason.unwrap_or("no reason given");
    if !std::io::stdin().is_terminal() {
        return Err(AppError::Validation(format!(
            "story `{id}` was deleted (reason: {reason}); re-run with --force to undelete"
        )));
    }

    println!("story `{id}` was deleted (reason: {reason}).");
    print!("Reopen (undelete) this deleted story? [y/N] ");
    std::io::stdout()
        .flush()
        .map_err(|e| AppError::Storage(format!("failed to write prompt: {e}")))?;

    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|e| AppError::Storage(format!("failed to read confirmation: {e}")))?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn story_view_by_id(root: &Path, id: &str) -> Result<Response, AppError> {
    let story = storage::load_story_snapshot(root, id)?;
    story_view_response(root, story)
}

fn story_view_response(root: &Path, story: StorySnapshot) -> Result<Response, AppError> {
    let views = build_story_views(root, true)?;
    let view = views
        .into_iter()
        .find(|candidate| candidate.story.id == story.id)
        .ok_or_else(|| AppError::NotFound(format!("story `{}` not found", story.id)))?;
    Ok(Response::Story(Box::new(view)))
}

fn sort_story_views(stories: &mut [StoryView]) {
    stories.sort_by_key(|story| numeric_story_id(&story.story.id));
}

fn numeric_story_id(id: &str) -> u64 {
    id.split('-')
        .nth(1)
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(u64::MAX)
}

fn has_relation(story: &StorySnapshot, relation: &str, other_id: &str) -> bool {
    story
        .relationships
        .iter()
        .any(|candidate| candidate.relation == relation && candidate.other_id == other_id)
}

fn validate_parent_constraints(
    stories: &BTreeMap<String, StorySnapshot>,
    a: &str,
    b: &str,
    relation: &str,
    a_story: &StorySnapshot,
    b_story: &StorySnapshot,
) -> Result<(), AppError> {
    let check_story: Option<(&StorySnapshot, &str)> = match relation {
        "parent-of" => Some((b_story, a)),
        "child-of" => Some((a_story, b)),
        _ => None,
    };

    if let Some((story, expected_parent)) = check_story {
        let parents = story
            .relationships
            .iter()
            .filter(|candidate| candidate.relation == "child-of")
            .map(|candidate| candidate.other_id.as_str())
            .collect::<Vec<_>>();
        if parents
            .iter()
            .any(|candidate| *candidate != expected_parent)
        {
            return Err(AppError::Validation(format!(
                "story `{}` already has a different parent",
                story.id
            )));
        }
    }

    match relation {
        "parent-of" if would_create_parent_cycle(stories, a, b) => {
            return Err(AppError::Validation(format!(
                "adding `parent-of` from `{a}` to `{b}` would create a cycle"
            )));
        }
        "child-of" if would_create_parent_cycle(stories, b, a) => {
            return Err(AppError::Validation(format!(
                "adding `child-of` from `{a}` to `{b}` would create a cycle"
            )));
        }
        _ => {}
    }

    Ok(())
}

fn doctor_report(root: &Path) -> Result<Response, AppError> {
    let views = build_story_views(root, false)?;
    let type_map = storage::load_type_map(root)?;
    let mut issues = Vec::new();
    for view in views {
        for issue in view.flagged_reasons {
            if issue.contains("obviated") || issue.contains("conflicts") {
                continue;
            }
            issues.push(format!("{}: {}", view.story.id, issue));
        }
        if let Some(ref slug) = view.story.story_type
            && !type_map.contains_key(slug)
        {
            issues.push(format!("{}: unknown type `{}`", view.story.id, slug));
        }
    }

    if issues.is_empty() {
        return Ok(Response::Issues(Vec::new()));
    }

    Err(AppError::Integrity(issues.join("\n")))
}

fn doctor_fix(root: &Path) -> Result<Response, AppError> {
    let open_stories = storage::load_all_open_snapshots(root)?;
    let story_map = open_stories
        .iter()
        .cloned()
        .map(|story| (story.id.clone(), story))
        .collect::<BTreeMap<_, _>>();
    let now = storage::now();
    let mut touched = BTreeSet::new();

    for story in open_stories {
        let mut extra_events = Vec::new();
        for relation in &story.relationships {
            let Some(other_story) = story_map.get(&relation.other_id) else {
                extra_events.push(StoryEvent::StoryRelationshipRemoved {
                    at: now.clone(),
                    other_id: relation.other_id.clone(),
                    relation: relation.relation.clone(),
                });
                continue;
            };

            let Some(expected_inverse) = crate::domain::inverse_relation(&relation.relation) else {
                continue;
            };
            if !has_relation(other_story, expected_inverse, &story.id) {
                storage::write_story_events(
                    root,
                    &relation.other_id,
                    &[StoryEvent::StoryRelationshipAdded {
                        at: now.clone(),
                        other_id: story.id.clone(),
                        relation: expected_inverse.to_string(),
                    }],
                )?;
                touched.insert(relation.other_id.clone());
            }
        }

        if !extra_events.is_empty() {
            storage::write_story_events(root, &story.id, &extra_events)?;
            touched.insert(story.id.clone());
        }
    }

    // Re-fold archived snapshots from their event log so stories archived
    // before a `fold_story` behavior change (e.g. #18: deletion now forces
    // `superstate: CLOSED`) self-heal instead of keeping a stale cache.
    let archive_repair = storage::repair_archived_snapshots(root)?;
    touched.extend(archive_repair.repaired);

    let result = doctor_report(root);
    match result {
        Ok(_) => {
            let mut message = if touched.is_empty() {
                "doctor found nothing to fix".to_string()
            } else {
                "doctor repaired supported integrity issues".to_string()
            };
            if !archive_repair.issues.is_empty() {
                message.push_str(&format!(
                    "\n{} archived stor{} could not be repaired:\n{}",
                    archive_repair.issues.len(),
                    if archive_repair.issues.len() == 1 {
                        "y"
                    } else {
                        "ies"
                    },
                    archive_repair.issues.join("\n")
                ));
            }
            Ok(Response::Message(message))
        }
        Err(error) => Err(error),
    }
}

/// Runs a `story state …` subcommand.
///
/// Every mutating branch takes the project write lock, because editing the
/// state set can migrate stories (see `storage::update_state`) — the
/// configuration change and the story moves have to land together.
fn run_state_action(root: &Path, action: StateAction) -> Result<Response, AppError> {
    match action {
        StateAction::List => {
            storage::ensure_project(root)?;
            let usage = storage::state_usage(root)?;
            let lines: Vec<String> = storage::load_states(root)?
                .iter()
                .map(|state| format_state_line(state, usage.get(&state.slug).copied()))
                .collect();
            Ok(Response::Message(lines.join("\n")))
        }

        StateAction::Add {
            slug,
            superstate,
            role,
            description,
        } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            let superstate = parse_superstate(&superstate)?;
            let state = storage::add_state(root, &slug, superstate, role, description)?;
            Ok(Response::Message(format!(
                "added state {} ({})",
                state.slug,
                state.super_state.as_str()
            )))
        }),

        StateAction::Set {
            slug,
            superstate,
            role,
            description,
            clear_description,
            move_stories_to,
        } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            let changes = StateChanges {
                super_state: superstate.as_deref().map(parse_superstate).transpose()?,
                // `--role none` clears; `active` is the only real role, so
                // `none` can't collide with one.
                role: match role.as_deref() {
                    None => FieldEdit::Keep,
                    Some("none") => FieldEdit::Clear,
                    Some(value) => FieldEdit::Set(value.to_string()),
                },
                description: if clear_description {
                    FieldEdit::Clear
                } else {
                    match description {
                        Some(text) => FieldEdit::Set(text),
                        None => FieldEdit::Keep,
                    }
                },
            };
            let edit = storage::update_state(root, &slug, &changes, move_stories_to.as_deref())?;
            let mut message = format!(
                "updated state {} ({})",
                edit.state.slug,
                edit.state.super_state.as_str()
            );
            if edit.moved > 0 {
                message.push_str(&format!(
                    "; moved {} {} to {}",
                    edit.moved,
                    if edit.moved == 1 { "story" } else { "stories" },
                    move_stories_to.as_deref().unwrap_or("another state")
                ));
            }
            Ok(Response::Message(message))
        }),

        StateAction::Remove {
            slug,
            move_stories_to,
        } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            let moved = storage::remove_state(root, &slug, move_stories_to.as_deref())?;
            let mut message = format!("removed state {slug}");
            if moved > 0 {
                message.push_str(&format!(
                    "; moved {} {} to {}",
                    moved,
                    if moved == 1 { "story" } else { "stories" },
                    move_stories_to.as_deref().unwrap_or("another state")
                ));
            }
            Ok(Response::Message(message))
        }),

        StateAction::Reorder { order } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            let states = storage::reorder_states(root, &order)?;
            Ok(Response::Message(format!(
                "reordered states: {}",
                states
                    .iter()
                    .map(|state| state.slug.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )))
        }),
    }
}

fn parse_superstate(raw: &str) -> Result<SuperState, AppError> {
    SuperState::parse(raw)
        .ok_or_else(|| AppError::Validation("superstate must be OPEN or CLOSED".to_string()))
}

/// One `story state list` row: `in-progress (OPEN, active) — 2 open — desc`.
fn format_state_line(state: &StateDef, usage: Option<StateUsage>) -> String {
    let mut attributes = vec![state.super_state.as_str().to_string()];
    if let Some(ref role) = state.role {
        attributes.push(role.clone());
    }
    let mut line = format!("{} ({})", state.slug, attributes.join(", "));

    if let Some(usage) = usage {
        let mut counts = Vec::new();
        if usage.open > 0 {
            counts.push(format!("{} open", usage.open));
        }
        if usage.archived > 0 {
            counts.push(format!("{} archived", usage.archived));
        }
        if !counts.is_empty() {
            line.push_str(&format!(" — {}", counts.join(", ")));
        }
    }
    if let Some(ref description) = state.description {
        line.push_str(&format!(" — {description}"));
    }
    line
}

fn handle_phase(root: &Path, action: PhaseAction, no_hooks: bool) -> Result<Response, AppError> {
    match action {
        PhaseAction::List => {
            storage::ensure_project(root)?;
            let views = build_story_views(root, false)?;
            let story_map: BTreeMap<String, StorySnapshot> = views
                .iter()
                .map(|v| (v.story.id.clone(), v.story.clone()))
                .collect();

            let mut phase_map: BTreeMap<String, Vec<&StoryView>> = BTreeMap::new();
            for view in &views {
                for label in &view.story.labels {
                    if let Some(num) = label.strip_prefix("phase:") {
                        phase_map.entry(num.to_string()).or_default().push(view);
                    }
                }
            }

            if phase_map.is_empty() {
                return Ok(Response::PhaseList(Vec::new()));
            }

            let default_open = storage::default_open_state(root)
                .map(|s| s.slug)
                .unwrap_or_else(|_| "todo".to_string());
            let mut phase_views = Vec::new();
            for (phase_num, stories) in &phase_map {
                let total = stories.len();
                let mut done = 0;
                let mut in_progress = 0;
                let mut todo = 0;
                let mut blocked_count = 0;
                let mut title = None;
                let mut story_ids = Vec::new();

                for view in stories {
                    story_ids.push(view.story.id.clone());
                    if view.story.superstate == SuperState::Closed {
                        done += 1;
                    } else if view.story.superstate == SuperState::Open
                        && !is_ready(&view.story, &story_map)
                    {
                        blocked_count += 1;
                    } else if view.story.superstate == SuperState::Open
                        && view.story.state != default_open
                    {
                        in_progress += 1;
                    } else {
                        todo += 1;
                    }

                    // Check for a phase grouping story (title starts with "Phase N:")
                    let prefix = format!("Phase {}:", phase_num);
                    if view.story.title.starts_with(&prefix) {
                        let rest = view.story.title[prefix.len()..].trim();
                        if !rest.is_empty() {
                            title = Some(rest.to_string());
                        }
                    } else {
                        let prefix_bare = format!("Phase {}", phase_num);
                        if view.story.title == prefix_bare {
                            // No title suffix
                        }
                    }
                }

                phase_views.push(PhaseView {
                    phase: phase_num.clone(),
                    title,
                    total,
                    done,
                    in_progress,
                    todo,
                    blocked: blocked_count,
                    story_ids,
                });
            }

            Ok(Response::PhaseList(phase_views))
        }
        PhaseAction::Show { phase } => {
            storage::ensure_project(root)?;
            let mut views = build_story_views(root, false)?;
            let phase_label = format!("phase:{phase}");
            views.retain(|v| v.story.labels.contains(&phase_label));
            sort_story_views(&mut views);
            Ok(Response::Stories(views, None))
        }
        PhaseAction::Add { id, phase } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            ensure_open_story(root, &id)?;
            let story = storage::load_open_story_snapshot(root, &id)?;
            let mut labels: BTreeSet<String> = story.labels.into_iter().collect();
            // Remove any existing phase:* labels
            labels.retain(|l| !l.starts_with("phase:"));
            // Add the new phase label
            labels.insert(format!("phase:{phase}"));
            let labels: Vec<String> = labels.into_iter().collect();
            storage::write_story_events(
                root,
                &id,
                &[StoryEvent::StoryLabelsSet {
                    at: storage::now(),
                    labels,
                }],
            )?;
            if !no_hooks && let Some(ref config) = crate::event_hooks::load_hooks_config(root) {
                let snapshot = storage::load_open_story_snapshot(root, &id)?;
                let payload = serde_json::json!({
                    "event_type": "label_change",
                    "story_id": &id,
                    "timestamp": crate::storage::now(),
                    "story_title": &snapshot.title,
                    "labels": &snapshot.labels
                });
                crate::event_hooks::fire_hook(
                    root,
                    config,
                    crate::event_hooks::HookEventType::LabelChange,
                    &payload.to_string(),
                );
            }
            Ok(Response::Message(format!("assigned {id} to phase {phase}")))
        }),
        PhaseAction::Remove { id } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            ensure_open_story(root, &id)?;
            let story = storage::load_open_story_snapshot(root, &id)?;
            let mut labels: BTreeSet<String> = story.labels.into_iter().collect();
            let had_phase = labels.iter().any(|l| l.starts_with("phase:"));
            labels.retain(|l| !l.starts_with("phase:"));
            if !had_phase {
                return Ok(Response::Message(format!("{id} has no phase assignment")));
            }
            let labels: Vec<String> = labels.into_iter().collect();
            storage::write_story_events(
                root,
                &id,
                &[StoryEvent::StoryLabelsSet {
                    at: storage::now(),
                    labels,
                }],
            )?;
            Ok(Response::Message(format!(
                "removed phase assignment from {id}"
            )))
        }),
        PhaseAction::Create { phase, title } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            let story_title = if let Some(ref t) = title {
                format!("Phase {}: {}", phase, t)
            } else {
                format!("Phase {}", phase)
            };
            let story = storage::create_story(root, &story_title, None)?;
            let id = story.id.clone();
            // Add the phase label
            let labels = vec![format!("phase:{phase}")];
            storage::write_story_events(
                root,
                &id,
                &[StoryEvent::StoryLabelsSet {
                    at: storage::now(),
                    labels,
                }],
            )?;
            story_view_by_id(root, &id)
        }),
    }
}

/// `AGENTS.md`, for the legacy leg.
///
/// Delegates to [`crate::service::templates::agents_md`] rather than holding a
/// second copy of the text. The two used to be maintained in parallel and kept
/// honest by a differential row comparing them byte for byte; one definition
/// is better than a guard against two definitions drifting, and it is what
/// lets the flip change the scaffolded guidance in one place.
fn generate_agents_md(root: &std::path::Path) -> String {
    let (prefix, done_state) =
        read_project_config(root).unwrap_or_else(|| ("SH".to_string(), "done".to_string()));
    crate::service::templates::agents_md(&prefix, &done_state)
}

fn generate_claude_md(_root: &std::path::Path) -> String {
    r#"## Storyhook

This project uses **storyhook** for task tracking. Full usage instructions are in `AGENTS.md` — read that file before starting work.

Quick start: run `story load-context` at session start, `story next` to pick a task.

Run `story help <command>` for detailed usage on any command, or `story help --compact` for the full reference.
"#
    .to_string()
}

fn generate_cursor_rules() -> String {
    r#"# Cursor Rules — storyhook Integration

This project uses **storyhook** as its issue tracker. Use the storyhook CLI
to manage tasks.

## Task Management

- Run `story load-context` at the start of each session to understand project state.
- Run `story next` to find the highest-priority ready task.
- After completing work, mark the story done: `story move <id> done`.
- Use `story handoff --since 2h` to summarize work at session end.

## Commands

- `story list` — list open stories
- `story new "<title>"` — create a new story
- `story show <id>` — show story details
- `story comment <id> "text"` — add a comment
- `story move <id> <state>` — change story state
- `story prioritize <id> <level>` — set priority (critical, high, medium, low, none)
- `story assign <id> <member>` — assign a story
- `story label <id> <label>` — add a label
- `story block <id> "reason"` — mark story as blocked
- `story unblock <id>` — clear blocked status
- `story relate <a> <rel> <b>` — add a relationship
- `story set <id> --field value` — update multiple fields at once
- `story search "<query>"` — search stories
- `story summary` — project overview
- `story load-context` — full project context for LLM consumption
- `story phase list` — phase progress overview
- `story handoff --since <duration>` — recent changes summary

Run `story help <command>` for detailed usage on any command.
"#
    .to_string()
}

fn read_project_config(root: &std::path::Path) -> Option<(String, String)> {
    let prefix = storage::load_project_prefix(root).ok()?;
    let states = storage::load_states(root).ok()?;
    let done_state = states
        .iter()
        .find(|s| s.super_state == crate::domain::SuperState::Closed)
        .map(|s| s.slug.clone())
        .unwrap_or_else(|| "done".to_string());
    Some((prefix, done_state))
}

/// Result of importing a batch of stories, including relationship summary.
struct ImportBatchResult {
    views: Vec<StoryView>,
    /// Human-readable relationship summary lines (e.g. "SH-10 child-of SH-9").
    relationship_lines: Vec<String>,
}

fn import_stories_batch(
    root: &Path,
    stories: &[ImportStory],
) -> Result<ImportBatchResult, AppError> {
    if stories.is_empty() {
        return Ok(ImportBatchResult {
            views: Vec::new(),
            relationship_lines: Vec::new(),
        });
    }
    // Validate all story_type values before creating any stories
    let type_map = storage::load_type_map(root)?;
    let invalid_types: std::collections::BTreeSet<&str> = stories
        .iter()
        .filter_map(|s| s.story_type.as_deref())
        .filter(|st| !type_map.contains_key(*st))
        .collect();
    if !invalid_types.is_empty() {
        return Err(AppError::Validation(format!(
            "unknown types: {}. Available types: {}",
            invalid_types.into_iter().collect::<Vec<_>>().join(", "),
            type_map.keys().cloned().collect::<Vec<_>>().join(", ")
        )));
    }
    let mut created_ids: Vec<String> = Vec::new();
    for import_story in stories {
        let story =
            storage::create_story(root, &import_story.title, import_story.state.as_deref())?;
        let id = story.id.clone();
        let now = storage::now();
        let mut events = Vec::new();
        if let Some(ref priority_str) = import_story.priority
            && let Some(priority) = Priority::parse(priority_str)
        {
            events.push(StoryEvent::StoryPrioritySet {
                at: now.clone(),
                priority,
            });
        }
        if let Some(ref labels) = import_story.labels
            && !labels.is_empty()
        {
            let mut sorted: Vec<String> = labels.clone();
            sorted.sort();
            sorted.dedup();
            events.push(StoryEvent::StoryLabelsSet {
                at: now.clone(),
                labels: sorted,
            });
        }
        if let Some(ref assignee) = import_story.assignee {
            let member = storage::find_member(root, assignee)?;
            events.push(StoryEvent::StoryAssigned {
                at: now.clone(),
                member_id: member.id,
            });
        }
        if let Some(ref description) = import_story.description
            && !description.trim().is_empty()
        {
            events.push(StoryEvent::StoryDescriptionSet {
                at: now.clone(),
                description: description.clone(),
            });
        }
        if let Some(ref st) = import_story.story_type {
            events.push(StoryEvent::StoryTypeSet {
                at: now.clone(),
                story_type: st.clone(),
            });
        }
        if !events.is_empty() {
            storage::write_story_events(root, &id, &events)?;
        }
        created_ids.push(id);
    }
    // Second pass: resolve relationships and collect summary
    let mut relationship_lines: Vec<String> = Vec::new();
    for (index, import_story) in stories.iter().enumerate() {
        if let Some(ref rels) = import_story.relationships {
            let a_id = &created_ids[index];
            for rel in rels {
                let b_id = if let Some(ref_idx) = rel.ref_index {
                    created_ids.get(ref_idx).cloned().ok_or_else(|| {
                        AppError::Validation(format!(
                            "ref_index {ref_idx} out of bounds for import batch"
                        ))
                    })?
                } else if let Some(ref other) = rel.other_id {
                    other.clone()
                } else {
                    return Err(AppError::Validation(
                        "relationship must have ref_index or other_id".to_string(),
                    ));
                };
                if a_id == &b_id {
                    continue;
                }
                let edges = relation_edges(&rel.relation).ok_or_else(|| {
                    AppError::Validation(format!("unsupported relationship `{}`", rel.relation))
                })?;
                let now = storage::now();
                for (a_rel, b_rel) in edges {
                    storage::write_story_events(
                        root,
                        a_id,
                        &[StoryEvent::StoryRelationshipAdded {
                            at: now.clone(),
                            other_id: b_id.clone(),
                            relation: a_rel.to_string(),
                        }],
                    )?;
                    if storage::open_story_exists(root, &b_id) {
                        storage::write_story_events(
                            root,
                            &b_id,
                            &[StoryEvent::StoryRelationshipAdded {
                                at: now.clone(),
                                other_id: a_id.clone(),
                                relation: b_rel.to_string(),
                            }],
                        )?;
                    }
                }
                relationship_lines.push(format!("{} {} {}", a_id, rel.relation, b_id));
            }
        }
    }
    let mut views = Vec::new();
    for id in &created_ids {
        let story = storage::load_open_story_snapshot(root, id)?;
        views.push(StoryView {
            story,
            derived_relationships: Vec::new(),
            warnings: Vec::new(),
            flagged_reasons: Vec::new(),
            stale_info: None,
            progress: None,
        });
    }
    Ok(ImportBatchResult {
        views,
        relationship_lines,
    })
}
