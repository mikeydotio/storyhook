use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::cli::{CliOptions, HELP_TEXT, Invocation, MemberInput};
use crate::domain::{
    Member, StoryEvent, StorySnapshot, SuperState, compute_integrity_issues, relation_edges,
};
use crate::error::AppError;
use crate::lock;
use crate::output::{Response, StoryView};
use crate::storage;

pub fn run(root: &Path, options: CliOptions) -> Result<Response, AppError> {
    match options.invocation {
        Invocation::Help => Ok(Response::Message(HELP_TEXT.to_string())),
        Invocation::Init => {
            storage::init_project(root)?;
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
        } => {
            storage::ensure_project(root)?;
            let mut views = all_story_views(root)?;
            if let Some(state) = state {
                views.retain(|view| view.story.state == state);
            }
            if let Some(assignee) = assignee {
                views.retain(|view| view.story.assignee.as_deref() == Some(assignee.as_str()));
            }
            if flagged {
                views.retain(|view| !view.flagged_reasons.is_empty());
            }
            sort_story_views(&mut views);
            Ok(Response::Stories(views))
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
        Invocation::SetState { id, state, comment } => lock::with_project_lock(root, || {
            storage::ensure_project(root)?;
            ensure_open_story(root, &id)?;
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

            validate_parent_constraints(&a, &b, &relation, &a_story, &b_story)?;

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

fn all_story_views(root: &Path) -> Result<Vec<StoryView>, AppError> {
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
            continue;
        }
        stories.insert(story.id.clone(), story);
    }

    let mut issues = compute_integrity_issues(&stories);
    for duplicate in duplicates {
        issues
            .entry(duplicate)
            .or_default()
            .push("story exists in both open and archive storage".to_string());
    }

    let mut views = Vec::new();
    for story in stories.into_values() {
        let mut flagged_reasons = issues.remove(&story.id).unwrap_or_default();
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
    let views = all_story_views(root)?;
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

    Ok(())
}

fn doctor_report(root: &Path) -> Result<Response, AppError> {
    let views = all_story_views(root)?;
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
