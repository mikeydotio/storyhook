use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::cli::{CliOptions, GraphMode, HELP_TEXT, Invocation, MemberInput};
use crate::domain::{
    DependencyGraph, ImportStory, Member, Priority, StoryEvent, StorySnapshot, SuperState,
    compute_integrity_issues, derive_family_relationships, is_ready, parse_duration,
    relation_edges, would_create_parent_cycle,
};
use crate::error::AppError;
use crate::lock;
use crate::output::{BlockedChainView, GraphOverview, GraphView, Response, StoryView, SummaryView};
use crate::storage;

pub fn run(root: &Path, options: CliOptions) -> Result<Response, AppError> {
    match options.invocation {
        Invocation::Help => Ok(Response::Message(HELP_TEXT.to_string())),
        Invocation::Init { prefix } => {
            storage::init_project(root, prefix.as_deref())?;
            Ok(Response::Message("initialized story project".to_string()))
        }
        Invocation::New { title } => lock::with_project_lock(root, || {
            let story = storage::create_story(root, &title)?;
            story_view_response(root, story)
        }),
        Invocation::MemberAdd { input } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            let member = build_member(root, input)?;
            storage::store_member(root, &member)?;
            Ok(Response::Message(format!("added member {}", member.id)))
        }),
        Invocation::StateAdd { slug, superstate } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            let superstate = SuperState::parse(&superstate).ok_or_else(|| {
                AppError::Validation("superstate must be OPEN or CLOSED".to_string())
            })?;
            let state = storage::add_state(root, &slug, superstate)?;
            Ok(Response::Message(format!(
                "added state {} ({})",
                state.slug,
                state.super_state.as_str()
            )))
        }),
        Invocation::StateRemove { slug } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            storage::remove_state(root, &slug)?;
            Ok(Response::Message(format!("removed state {slug}")))
        }),
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
            sort_story_views(&mut views);
            Ok(Response::Stories(views))
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
            let mut blocked_count = 0;
            let mut flagged_count = 0;

            for view in &views {
                *state_counts.entry(view.story.state.clone()).or_default() += 1;
                if view.story.priority != Priority::None {
                    *priority_counts
                        .entry(view.story.priority.as_str().to_string())
                        .or_default() += 1;
                }
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

            Ok(Response::Summary(Box::new(SummaryView {
                total_open,
                total_closed,
                by_state,
                by_priority,
                blocked_count,
                flagged_count,
                ready_count,
                ready_stories: ready,
            })))
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
                    });
                }
            }
            sort_story_views(&mut results);
            Ok(Response::Stories(results))
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
            storage::write_story_events(
                root,
                &id,
                &[StoryEvent::StoryCommentAdded {
                    at: storage::now(),
                    text,
                }],
            )?;
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
        Invocation::SetState { id, state, comment } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            ensure_open_story(root, &id)?;
            let story = storage::load_open_story_snapshot(root, &id)?;
            let states = storage::load_state_map(root)?;
            let state_def = states
                .get(&state)
                .ok_or_else(|| AppError::Validation(format!("state `{state}` is not defined")))?;
            let now = storage::now();
            let mut events = vec![StoryEvent::StoryStateChanged {
                at: now.clone(),
                state: state.clone(),
            }];
            if let Some(comment) = comment {
                events.push(StoryEvent::StoryCommentAdded {
                    at: now.clone(),
                    text: comment,
                });
            }
            if state_def.super_state == SuperState::Closed {
                if story.awaiting.is_some() {
                    events.push(StoryEvent::StoryAwaitingCleared { at: now.clone() });
                }
                events.push(StoryEvent::StoryClosedAndArchived {
                    at: now,
                    state: state.clone(),
                });
            }
            storage::write_story_events(root, &id, &events)?;

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
            storage::write_story_events(
                root,
                &id,
                &[StoryEvent::StoryPrioritySet {
                    at: storage::now(),
                    priority,
                }],
            )?;
            story_view_by_id(root, &id)
        }),
        Invocation::Next { count } => {
            storage::ensure_project(root)?;
            let views = build_story_views(root, false)?;
            let story_map: BTreeMap<String, StorySnapshot> = views
                .iter()
                .map(|v| (v.story.id.clone(), v.story.clone()))
                .collect();
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
            ready.truncate(count);
            if ready.is_empty() {
                Ok(Response::Message("no ready stories".to_string()))
            } else if count == 1 {
                Ok(Response::Story(Box::new(ready.remove(0))))
            } else {
                Ok(Response::Stories(ready))
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
            let mut created_ids: Vec<String> = Vec::new();
            for import_story in &stories {
                let story = storage::create_story(root, &import_story.title)?;
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
                });
            }
            Ok(Response::Stories(views))
        }),
        Invocation::Reopen { id } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            if storage::open_story_exists(root, &id) {
                return Err(AppError::Validation(format!(
                    "story `{id}` is already open"
                )));
            }
            if !storage::is_archived(root, &id)? {
                return Err(AppError::NotFound(format!("story `{id}` not found")));
            }
            storage::unarchive_story(root, &id)?;
            let default_state = storage::default_open_state(root)?;
            let now = storage::now();
            storage::write_story_events(
                root,
                &id,
                &[StoryEvent::StoryStateChanged {
                    at: now,
                    state: default_state.slug,
                }],
            )?;
            story_view_by_id(root, &id)
        }),
        Invocation::Export => {
            storage::ensure_project(root)?;
            let export = storage::export_project(root)?;
            let json = serde_json::to_string_pretty(&export)?;
            Ok(Response::Message(json))
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
            for view in &views {
                *state_counts.entry(view.story.state.clone()).or_default() += 1;
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
                            if matches!(
                                rel.relation.as_str(),
                                "follows" | "starts-after" | "precedes" | "starts-before"
                            ) && story_map
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
                                (r.relation == "follows" || r.relation == "starts-after")
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
                                (r.relation == "precedes" || r.relation == "starts-before")
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

            story_view_by_id(root, &a)
        }),
    }
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
        if story
            .relationships
            .iter()
            .any(|relation| relation.relation == "conflicts-with")
        {
            flagged_reasons.push("story conflicts with another story".to_string());
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
        });
    }

    Ok(views)
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
        "parent-of" => {
            if would_create_parent_cycle(stories, a, b) {
                return Err(AppError::Validation(format!(
                    "adding `parent-of` from `{a}` to `{b}` would create a cycle"
                )));
            }
        }
        "child-of" => {
            if would_create_parent_cycle(stories, b, a) {
                return Err(AppError::Validation(format!(
                    "adding `child-of` from `{a}` to `{b}` would create a cycle"
                )));
            }
        }
        _ => {}
    }

    Ok(())
}

fn doctor_report(root: &Path) -> Result<Response, AppError> {
    let views = build_story_views(root, false)?;
    let mut issues = Vec::new();
    for view in views {
        for issue in view.flagged_reasons {
            if issue.contains("obviated") || issue.contains("conflicts") {
                continue;
            }
            issues.push(format!("{}: {}", view.story.id, issue));
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

    let result = doctor_report(root);
    match result {
        Ok(_) => {
            if touched.is_empty() {
                Ok(Response::Message("doctor found nothing to fix".to_string()))
            } else {
                Ok(Response::Message(
                    "doctor repaired supported integrity issues".to_string(),
                ))
            }
        }
        Err(error) => Err(error),
    }
}
