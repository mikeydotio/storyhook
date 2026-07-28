//! The read surface: every question a caller can ask without changing
//! anything.
//!
//! [`QueryService`] borrows a [`ReadOps`] transaction rather than a
//! [`Store`](crate::store::Store), which makes it *statically* incapable of
//! writing — there is no write method in scope, so "this command must not
//! mutate" stops being a review comment and becomes a type error. That is the
//! whole reason it does not take a [`Ctx`](super::Ctx) the way the mutating
//! services do: a `Ctx` hands out a store, and a store can write.
//!
//! # Ordering is a compatibility contract
//!
//! Every ordering in this file reproduces the legacy one exactly, **including
//! the two known defects the golden corpus freezes**:
//!
//! * `list`, `search`, `epic list` and `phase show` sort by story *number*;
//!   `graph`, `handoff`, `context` and `summary`'s ready list sort
//!   *lexicographically* (`SH-1, SH-10, SH-11, SH-2, …`), because they iterate
//!   a map keyed by the id string.
//! * `handoff` iterates open stories then archived ones, each in id order,
//!   because the legacy path concatenated a directory listing with a
//!   `ORDER BY id ASC` query.
//!
//! Reproducing a defect is deliberate. The golden corpus pins the current
//! bytes; normalising an ordering here would move those bytes in a wave whose
//! entire claim is that it moves none. The wave that flips the default owns
//! deciding which of these becomes numeric.

use std::collections::{BTreeMap, BTreeSet};

use crate::cli::GraphMode;
use crate::domain::{
    DependencyGraph, Priority, StorySnapshot, SuperState, compute_integrity_issues,
    compute_progress, derive_family_relationships, has_children, is_ready, last_activity_type,
    parse_duration,
};
use crate::error::AppError;
use crate::output::{
    BlockedChainView, GraphOverview, GraphView, ReportData, StaleInfo, StoryView, SummaryView,
};
use crate::store::{ProjectId, ReadOps, StoryNo, StoryQuery, partition_known};

/// The `story list` filter grammar, as one value.
///
/// One field per flag the CLI accepts, in the order the legacy arm applied
/// them — the order matters only for `--stale`, which both filters and
/// *annotates*, but keeping the whole set in one struct is what lets the
/// filter chain be read against the grammar rather than against a call site.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListFilters {
    /// `--state <slug>`: exact state match.
    pub state: Option<String>,
    /// `--assignee <member>`: exact member-id match.
    pub assignee: Option<String>,
    /// `--flagged`: only stories with integrity flags.
    pub flagged: bool,
    /// `--priority a,b`: any of a comma-separated list.
    pub priority: Option<String>,
    /// `--label a,b`: carrying any of a comma-separated list.
    pub label: Option<String>,
    /// `--created-after <ts>`: string comparison against `created_at`.
    pub created_after: Option<String>,
    /// `--updated-after <ts>`: string comparison against `updated_at`.
    pub updated_after: Option<String>,
    /// `--blocked`: open and not ready.
    pub blocked: bool,
    /// `--ready`: ready to be worked.
    pub ready: bool,
    /// `--stale <duration>`: open and untouched for longer than that.
    pub stale: Option<String>,
    /// `--phase <n>`: carrying the `phase:<n>` label.
    pub phase: Option<String>,
    /// `--type <slug>`, or the literal `none` for stories with no type.
    pub story_type: Option<String>,
}

/// Answers every read-only question about a project.
pub struct QueryService<'a, R: ReadOps> {
    tx: &'a R,
    project: ProjectId,
    now: String,
}

impl<'a, R: ReadOps> QueryService<'a, R> {
    /// A query service reading `project` through `tx`, stamping relative
    /// deadlines (`--stale`, `handoff --since`) from `now`.
    ///
    /// `now` is a parameter rather than a clock read because two of the read
    /// surfaces are *functions of the current time*, and a test that cannot
    /// pin it can only assert that they did not crash.
    pub fn new(tx: &'a R, project: ProjectId, now: &str) -> Self {
        Self {
            tx,
            project,
            now: now.to_string(),
        }
    }

    /// Every story in the project, keyed by id.
    ///
    /// A `BTreeMap`, so iterating it is **lexicographic** by id — which is
    /// where `graph`, `context`, `summary` and `doctor` get their ordering.
    pub fn story_map(&self) -> Result<BTreeMap<String, StorySnapshot>, AppError> {
        story_map(self.tx, self.project)
    }

    /// Every story as a view, with the cross-story facts filled in.
    pub fn story_views(&self, include_derived: bool) -> Result<Vec<StoryView>, AppError> {
        story_views(self.tx, self.project, include_derived)
    }

    /// `story show <id>` — one story, with its derived family relationships.
    ///
    /// Archived and deleted stories are found too: the legacy path looked in
    /// the open directory *and* the archive, and a story is one row here.
    pub fn show(&self, id: &str) -> Result<StoryView, AppError> {
        match story_view(self.tx, self.project, id)? {
            crate::output::Response::Story(view) => Ok(*view),
            other => Err(AppError::Storage(format!(
                "internal: story view answered with {other:?}"
            ))),
        }
    }

    /// `story list` with every filter the CLI grammar allows.
    ///
    /// Filters are applied in the legacy order and are conjunctive. `--stale`
    /// is last because it also *annotates* the survivors with
    /// [`StaleInfo`], which costs one event read per remaining story.
    pub fn list(&self, filters: &ListFilters) -> Result<Vec<StoryView>, AppError> {
        let mut views = self.story_views(false)?;
        let stories = view_map(&views);

        if let Some(state) = &filters.state {
            views.retain(|view| &view.story.state == state);
        }
        if let Some(assignee) = &filters.assignee {
            views.retain(|view| view.story.assignee.as_deref() == Some(assignee.as_str()));
        }
        if filters.flagged {
            views.retain(|view| !view.flagged_reasons.is_empty());
        }
        if let Some(priority_csv) = &filters.priority {
            let priorities: Vec<Priority> = priority_csv
                .split(',')
                .filter_map(|raw| Priority::parse(raw.trim()))
                .collect();
            // An all-unparseable list filters nothing, rather than everything:
            // `--priority nonsense` has always listed the whole project.
            if !priorities.is_empty() {
                views.retain(|view| priorities.contains(&view.story.priority));
            }
        }
        if let Some(label_csv) = &filters.label {
            let wanted: Vec<String> = label_csv
                .split(',')
                .map(|raw| raw.trim().to_string())
                .filter(|label| !label.is_empty())
                .collect();
            if !wanted.is_empty() {
                views.retain(|view| wanted.iter().any(|label| view.story.labels.contains(label)));
            }
        }
        if let Some(threshold) = &filters.created_after {
            views.retain(|view| view.story.created_at.as_str() >= threshold.as_str());
        }
        if let Some(threshold) = &filters.updated_after {
            views.retain(|view| view.story.updated_at.as_str() >= threshold.as_str());
        }
        if filters.blocked {
            views.retain(|view| {
                view.story.superstate == SuperState::Open && !is_ready(&view.story, &stories)
            });
        }
        if filters.ready {
            views.retain(|view| is_ready(&view.story, &stories));
        }
        if let Some(phase) = &filters.phase {
            let label = format!("phase:{phase}");
            views.retain(|view| view.story.labels.contains(&label));
        }
        if let Some(story_type) = &filters.story_type {
            if story_type == "none" {
                views.retain(|view| view.story.story_type.is_none());
            } else {
                views.retain(|view| view.story.story_type.as_deref() == Some(story_type.as_str()));
            }
        }
        if let Some(stale) = &filters.stale {
            let duration = parse_duration(stale).ok_or_else(|| {
                AppError::Validation(format!("invalid duration `{stale}` (use e.g. 2h, 1d, 1w)"))
            })?;
            let threshold =
                (self.instant()? - duration).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            views.retain(|view| {
                view.story.superstate == SuperState::Open
                    && view.story.updated_at.as_str() <= threshold.as_str()
            });
            self.annotate_staleness(&mut views)?;
        }

        sort_story_views(&mut views);
        Ok(views)
    }

    /// `story search <query>` — case-insensitive substring match on the
    /// title, any comment, or any label.
    ///
    /// The views it returns are deliberately *bare*: no derived relationships,
    /// no integrity flags, no progress rollup. The legacy arm folded snapshots
    /// straight into views without the cross-story pass, and a search result
    /// that suddenly carried flags would change every `--json` consumer's
    /// document.
    pub fn search(&self, query: &str) -> Result<Vec<StoryView>, AppError> {
        let needle = query.to_lowercase();
        let mut results: Vec<StoryView> = self
            .all_stories_legacy_order()?
            .into_iter()
            .filter(|story| {
                story.title.to_lowercase().contains(&needle)
                    || story
                        .comments
                        .iter()
                        .any(|comment| comment.text.to_lowercase().contains(&needle))
                    || story
                        .labels
                        .iter()
                        .any(|label| label.to_lowercase().contains(&needle))
            })
            .map(bare_view)
            .collect();
        sort_story_views(&mut results);
        Ok(results)
    }

    /// `story next` — the ready stories a worker should pick up, best first.
    ///
    /// Leaf stories only: a parent whose children are ready is not itself
    /// work. Sorted by `priority ASC, created_at ASC` — the legacy
    /// comparator, whose second key has one-second precision and therefore
    /// ties. Ties fall back to the order this list arrived in, which is
    /// lexicographic by id.
    pub fn next(&self, count: usize, phase: Option<&str>) -> Result<Vec<StoryView>, AppError> {
        let views = self.story_views(false)?;
        let stories = view_map(&views);
        let mut ready: Vec<StoryView> = views
            .into_iter()
            .filter(|view| is_ready(&view.story, &stories) && !has_children(&view.story))
            .collect();
        if let Some(phase) = phase {
            let label = format!("phase:{phase}");
            ready.retain(|view| view.story.labels.contains(&label));
        }
        sort_by_priority_then_age(&mut ready);
        ready.truncate(count);
        Ok(ready)
    }

    /// `story summary` — the project's rollup plus its top five ready stories.
    pub fn summary(&self) -> Result<SummaryView, AppError> {
        let views = self.story_views(false)?;
        let stories = view_map(&views);
        let mut summary = rollup(&views, &stories);

        let mut ready: Vec<StoryView> = views
            .into_iter()
            .filter(|view| is_ready(&view.story, &stories))
            .collect();
        sort_by_priority_then_age(&mut ready);
        summary.ready_count = ready.len();
        ready.truncate(READY_PREVIEW);
        summary.ready_stories = ready;
        Ok(summary)
    }

    /// The whole project in the shape `story report` and the web dashboard
    /// both consume: the rollup, every view, and the ready/blocked id sets.
    pub fn report_data(&self) -> Result<ReportData, AppError> {
        let views = self.story_views(false)?;
        let stories = view_map(&views);
        let mut summary = rollup(&views, &stories);

        let mut ready_ids = Vec::new();
        let mut blocked_ids = Vec::new();
        for view in &views {
            if view.story.superstate == SuperState::Open {
                if is_ready(&view.story, &stories) {
                    ready_ids.push(view.story.id.clone());
                } else {
                    blocked_ids.push(view.story.id.clone());
                }
            }
        }
        // `rollup` leaves the ready count at zero because each surface fills it
        // from its own ready set; report's is the open ready ids it just
        // collected. (`is_ready` is false for a closed story, so this equals
        // `summary`'s count — the two are computed differently and agree.)
        summary.ready_count = ready_ids.len();

        Ok(ReportData {
            summary,
            stories: views,
            ready_ids,
            blocked_ids,
        })
    }

    /// `story report` in its non-HTML form: the summary, with the ready
    /// preview filled in from the report's own ready set.
    pub fn report_summary(&self) -> Result<SummaryView, AppError> {
        let data = self.report_data()?;
        let ready_ids: BTreeSet<String> = data.ready_ids.into_iter().collect();
        let mut ready: Vec<StoryView> = data
            .stories
            .into_iter()
            .filter(|view| ready_ids.contains(&view.story.id))
            .collect();
        sort_by_priority_then_age(&mut ready);

        let mut summary = data.summary;
        summary.ready_count = ready.len();
        ready.truncate(READY_PREVIEW);
        summary.ready_stories = ready;
        Ok(summary)
    }

    /// `story graph` in each of its four modes.
    pub fn graph(&self, mode: &GraphMode) -> Result<GraphView, AppError> {
        let stories = self.story_map()?;
        let graph = DependencyGraph::from_open_stories(&stories);

        Ok(match mode {
            GraphMode::Overview => {
                let groups = graph.parallel_groups();
                let path = graph.critical_path();
                let open: Vec<&StorySnapshot> = stories
                    .values()
                    .filter(|story| story.superstate == SuperState::Open)
                    .collect();

                let open_neighbour = |id: &str| {
                    stories
                        .get(id)
                        .is_some_and(|other| other.superstate == SuperState::Open)
                };
                let total_edges = open
                    .iter()
                    .flat_map(|story| &story.relationships)
                    .filter(|relation| {
                        matches!(relation.relation.as_str(), "blocks" | "blocked-by")
                            && open_neighbour(&relation.other_id)
                    })
                    .count();
                let ends = |wanted: &str| -> Vec<String> {
                    open.iter()
                        .filter(|story| {
                            !story.relationships.iter().any(|relation| {
                                relation.relation == wanted && open_neighbour(&relation.other_id)
                            })
                        })
                        .map(|story| story.id.clone())
                        .collect()
                };

                GraphView {
                    // A one-node path is not a path, and a single group is not
                    // parallelism: both collapse to absent rather than to a
                    // degenerate value the renderer would have to special-case.
                    critical_path: (path.len() > 1).then_some(path),
                    blocked_chain: None,
                    parallel_groups: (groups.len() > 1).then(|| collect_groups(groups)),
                    overview: Some(GraphOverview {
                        total_open: open.len(),
                        total_edges,
                        roots: ends("blocked-by"),
                        leaves: ends("blocks"),
                    }),
                }
            }
            GraphMode::CriticalPath => GraphView {
                critical_path: Some(graph.critical_path()),
                blocked_chain: None,
                parallel_groups: None,
                overview: None,
            },
            GraphMode::BlockedBy(id) => {
                if !stories.contains_key(id) {
                    return Err(AppError::NotFound(format!("story `{id}` not found")));
                }
                GraphView {
                    critical_path: None,
                    blocked_chain: Some(BlockedChainView {
                        source: id.clone(),
                        blocked: graph.blocked_chain(id).into_iter().collect(),
                    }),
                    parallel_groups: None,
                    overview: None,
                }
            }
            GraphMode::ParallelGroups => GraphView {
                critical_path: None,
                blocked_chain: None,
                parallel_groups: Some(collect_groups(graph.parallel_groups())),
                overview: None,
            },
        })
    }

    /// `story context` — the agent-facing project briefing, as Markdown or as
    /// a JSON document.
    ///
    /// The JSON form is returned as a *string* rather than a typed value, and
    /// deliberately so: the global `--json` flag then wraps it as an escaped
    /// string in the envelope's `message` field. That is a defect the export
    /// wave fixed for `story export` and left alone here, because the golden
    /// corpus freezes the wrapped form and no consumer parses it.
    pub fn context(&self, json: bool) -> Result<String, AppError> {
        let views = self.story_views(false)?;
        let stories = view_map(&views);

        let total_open = views
            .iter()
            .filter(|view| view.story.superstate == SuperState::Open)
            .count();
        let total_closed = views.len() - total_open;

        let mut state_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut type_counts: BTreeMap<String, usize> = BTreeMap::new();
        for view in &views {
            *state_counts.entry(view.story.state.clone()).or_default() += 1;
            *type_counts.entry(type_label(&view.story)).or_default() += 1;
        }

        let blocked: Vec<&StoryView> = views
            .iter()
            .filter(|view| {
                view.story.superstate == SuperState::Open && !is_ready(&view.story, &stories)
            })
            .collect();
        let mut ready: Vec<&StoryView> = views
            .iter()
            .filter(|view| is_ready(&view.story, &stories))
            .collect();
        ready.sort_by(|a, b| priority_then_age(&a.story, &b.story));
        let ready_count = ready.len();
        ready.truncate(READY_PREVIEW);

        if json {
            let document = serde_json::json!({
                "total_stories": views.len(),
                "open": total_open,
                "closed": total_closed,
                "by_state": state_counts,
                "by_type": type_counts,
                "blocked_count": blocked.len(),
                "ready_count": ready_count,
                "ready_stories": ready.iter().map(|view| serde_json::json!({
                    "id": view.story.id,
                    "title": view.story.title,
                    "state": view.story.state,
                    "priority": view.story.priority.as_str(),
                })).collect::<Vec<_>>(),
            });
            return Ok(serde_json::to_string_pretty(&document).unwrap_or_default());
        }

        let mut body = format!(
            "# Project Status\n\nStories: {} ({} open, {} closed)\n\n",
            views.len(),
            total_open,
            total_closed
        );

        body.push_str("## State Distribution\n\n");
        for (state, count) in &state_counts {
            body.push_str(&format!("- {state}: {count}\n"));
        }
        body.push_str("\n## Type Distribution\n\n");
        for (story_type, count) in &type_counts {
            body.push_str(&format!("- {story_type}: {count}\n"));
        }

        if !ready.is_empty() {
            body.push_str(&format!("\n## Ready to Work ({ready_count} total)\n\n"));
            for view in &ready {
                let priority = if view.story.priority == Priority::None {
                    String::new()
                } else {
                    format!(" ({})", view.story.priority.as_str())
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
                let reason = match &view.story.awaiting {
                    Some(awaiting) => format!(" — awaiting: {awaiting}"),
                    None => String::new(),
                };
                body.push_str(&format!(
                    "- {} {}{}\n",
                    view.story.id, view.story.title, reason
                ));
            }
        }

        body.push_str(&self.phase_progress(&views, &stories)?);
        Ok(body)
    }

    /// `story handoff` — what changed in the last window, as Markdown.
    ///
    /// Note the iteration order: open stories first, then archived ones, each
    /// in **lexicographic** id order. That is the legacy concatenation of a
    /// directory listing and an `ORDER BY id ASC`, reproduced rather than
    /// repaired.
    pub fn handoff(&self, since: Option<&str>) -> Result<String, AppError> {
        let duration = match since {
            Some(raw) => parse_duration(raw).ok_or_else(|| {
                AppError::Validation(format!("invalid duration `{raw}` (use e.g. 2h, 1d, 1w)"))
            })?,
            None => chrono::Duration::try_hours(DEFAULT_HANDOFF_HOURS).unwrap_or_default(),
        };
        let threshold =
            (self.instant()? - duration).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        let (mut created, mut updated, mut closed) = (Vec::new(), Vec::new(), Vec::new());
        for story in self.all_stories_legacy_order()? {
            if story.updated_at.as_str() < threshold.as_str() {
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
        for (heading, stories) in [
            ("Created", &created),
            ("Updated", &updated),
            ("Closed", &closed),
        ] {
            if stories.is_empty() {
                continue;
            }
            body.push_str(&format!("## {heading} ({})\n\n", stories.len()));
            for story in stories.iter() {
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
        Ok(body)
    }

    // --- internals ---------------------------------------------------------

    /// Every story, open ones first and then archived ones, each in
    /// lexicographic id order — `storage::load_all_snapshots`'s order.
    fn all_stories_legacy_order(&self) -> Result<Vec<StorySnapshot>, AppError> {
        let mut all = Vec::new();
        for archived in [false, true] {
            let mut rows = self
                .tx
                .stories(self.project, &StoryQuery::all().archived(archived))?
                .into_iter()
                .map(|row| row.snapshot)
                .collect::<Vec<_>>();
            rows.sort_by(|a, b| a.id.cmp(&b.id));
            all.append(&mut rows);
        }
        Ok(all)
    }

    /// This service's "now", as a datetime.
    fn instant(&self) -> Result<chrono::DateTime<chrono::Utc>, AppError> {
        chrono::DateTime::parse_from_rfc3339(&self.now)
            .map(|at| at.with_timezone(&chrono::Utc))
            .map_err(|error| {
                AppError::Storage(format!("unparseable timestamp `{}`: {error}", self.now))
            })
    }

    /// Fills in [`StaleInfo`] for each surviving view, which needs that
    /// story's event log for the *kind* of its last activity.
    fn annotate_staleness(&self, views: &mut [StoryView]) -> Result<(), AppError> {
        let prefix = super::project_prefix(self.tx, self.project)?;
        let now = self.instant()?;
        for view in views {
            let story_no = StoryNo::parse_id(&prefix, &view.story.id)
                .map_err(|error| AppError::Storage(format!("unparseable story id: {error}")))?;
            let stored = self.tx.events_for(self.project, story_no)?;
            let (known, _unknown) = partition_known(story_no, &stored);
            let Ok(updated) = chrono::DateTime::parse_from_rfc3339(&view.story.updated_at) else {
                continue;
            };
            let days = (now - updated.with_timezone(&chrono::Utc)).num_days();
            view.stale_info = Some(StaleInfo {
                last_activity_at: view.story.updated_at.clone(),
                last_activity_type: last_activity_type(&known).to_string(),
                days_stale: days.max(0) as u64,
            });
        }
        Ok(())
    }

    /// `story context`'s trailing phase-progress table, or an empty string
    /// when the project uses no `phase:` labels.
    fn phase_progress(
        &self,
        views: &[StoryView],
        stories: &BTreeMap<String, StorySnapshot>,
    ) -> Result<String, AppError> {
        let mut phases: BTreeMap<String, Vec<&StoryView>> = BTreeMap::new();
        for view in views {
            for label in &view.story.labels {
                if let Some(number) = label.strip_prefix("phase:") {
                    phases.entry(number.to_string()).or_default().push(view);
                }
            }
        }
        if phases.is_empty() {
            return Ok(String::new());
        }

        // `tx.states()` and not `tx.state_map()`: the default open state is the
        // first *configured* one, and the map is alphabetical.
        let default_open = self
            .tx
            .states(self.project)?
            .into_iter()
            .find(|state| state.super_state == SuperState::Open)
            .map_or_else(|| "todo".to_string(), |state| state.slug);

        let mut body = String::from("\n## Phase Progress\n\n");
        for (number, members) in &phases {
            let total = members.len();
            let done = members
                .iter()
                .filter(|view| view.story.superstate == SuperState::Closed)
                .count();
            let in_progress = members
                .iter()
                .filter(|view| {
                    view.story.superstate == SuperState::Open && view.story.state != default_open
                })
                .count();
            let blocked = members
                .iter()
                .filter(|view| {
                    view.story.superstate == SuperState::Open && !is_ready(&view.story, stories)
                })
                .count();

            let title = members
                .iter()
                .find_map(|view| {
                    let prefix = format!("Phase {number}:");
                    let rest = view.story.title.strip_prefix(&prefix)?.trim();
                    (!rest.is_empty()).then(|| format!(": {rest}"))
                })
                .unwrap_or_default();

            let mut parts = vec![format!("{done}/{total} done")];
            if in_progress > 0 {
                parts.push(format!("{in_progress} in-progress"));
            }
            if blocked > 0 {
                parts.push(format!("{blocked} blocked"));
            }
            body.push_str(&format!(
                "- Phase {number}{title} -- {}\n",
                parts.join(", ")
            ));
        }
        Ok(body)
    }
}

/// How many ready stories `summary`, `report` and `context` preview.
const READY_PREVIEW: usize = 5;

/// `story handoff`'s window when `--since` is not given.
const DEFAULT_HANDOFF_HOURS: i64 = 24;

/// Every story in the project, keyed by id.
pub(crate) fn story_map(
    tx: &impl ReadOps,
    project: ProjectId,
) -> Result<BTreeMap<String, StorySnapshot>, AppError> {
    Ok(tx
        .stories(project, &StoryQuery::all())?
        .into_iter()
        .map(|row| (row.snapshot.id.clone(), row.snapshot))
        .collect())
}

/// Every story in the project as a view, with the cross-story facts —
/// integrity flags, progress rollups, and optionally the derived family
/// relationships — filled in.
///
/// One legacy behaviour is deliberately absent: `app::build_story_views` flags
/// a story that exists in *both* the open directory and the archive, the SH-20
/// split-brain shape. A story in the store is one row, so the condition is not
/// representable and there is nothing to flag.
pub fn story_views(
    tx: &impl ReadOps,
    project: ProjectId,
    include_derived: bool,
) -> Result<Vec<StoryView>, AppError> {
    let stories = story_map(tx, project)?;
    let mut issues = compute_integrity_issues(&stories);
    let derived_relationships = if include_derived {
        derive_family_relationships(&stories)
    } else {
        BTreeMap::new()
    };

    let progress: BTreeMap<String, _> = stories
        .values()
        .filter_map(|story| compute_progress(story, &stories).map(|p| (story.id.clone(), p)))
        .collect();

    let mut views = Vec::with_capacity(stories.len());
    for story in stories.into_values() {
        let id = story.id.clone();
        let mut flagged_reasons = issues.remove(&id).unwrap_or_default();
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
            derived_relationships: derived_relationships.get(&id).cloned().unwrap_or_default(),
            warnings: Vec::new(),
            flagged_reasons,
            stale_info: None,
            progress: progress.get(&id).cloned(),
        });
    }
    Ok(views)
}

/// The response for one story, with its derived family relationships.
pub fn story_view(
    tx: &impl ReadOps,
    project: ProjectId,
    id: &str,
) -> Result<crate::output::Response, AppError> {
    let view = story_views(tx, project, true)?
        .into_iter()
        .find(|candidate| candidate.story.id == id)
        .ok_or_else(|| AppError::NotFound(format!("story `{id}` not found")))?;
    Ok(crate::output::Response::Story(Box::new(view)))
}

/// Orders views the way `list`, `search` and the grouping commands do: by
/// story *number*, not by the lexicographic order of the id.
///
/// `SH-10` sorts after `SH-2` here and before it in a string comparison, which
/// is the difference between a list a human reads and one they re-sort in
/// their head.
pub fn sort_story_views(views: &mut [StoryView]) {
    views.sort_by_key(|view| numeric_story_id(&view.story.id));
}

/// The number half of a story id, or `u64::MAX` for an id that has none — so
/// an unparseable id sorts last rather than first.
fn numeric_story_id(id: &str) -> u64 {
    id.split('-')
        .nth(1)
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(u64::MAX)
}

/// A view carrying only the snapshot — what `search` returns.
fn bare_view(story: StorySnapshot) -> StoryView {
    StoryView {
        story,
        derived_relationships: Vec::new(),
        warnings: Vec::new(),
        flagged_reasons: Vec::new(),
        stale_info: None,
        progress: None,
    }
}

/// The snapshots behind a list of views, keyed by id — what `is_ready` needs.
fn view_map(views: &[StoryView]) -> BTreeMap<String, StorySnapshot> {
    views
        .iter()
        .map(|view| (view.story.id.clone(), view.story.clone()))
        .collect()
}

/// A story's type for counting purposes; untyped stories count as `Default`.
fn type_label(story: &StorySnapshot) -> String {
    story.story_type.as_deref().unwrap_or("Default").to_string()
}

/// `priority ASC, created_at ASC` — the legacy work-ordering comparator.
///
/// Not a total order: `created_at` has second precision, so two stories
/// created in the same second at the same priority tie, and the caller's
/// pre-existing order decides. That nondeterminism is a known production
/// defect, reproduced here rather than fixed because the golden corpus freezes
/// its output.
fn priority_then_age(a: &StorySnapshot, b: &StorySnapshot) -> std::cmp::Ordering {
    a.priority
        .cmp(&b.priority)
        .then_with(|| a.created_at.cmp(&b.created_at))
}

/// [`priority_then_age`] over a list of views.
fn sort_by_priority_then_age(views: &mut [StoryView]) {
    views.sort_by(|a, b| priority_then_age(&a.story, &b.story));
}

/// The counting half of `summary` and `report`, which agree on every field.
fn rollup(views: &[StoryView], stories: &BTreeMap<String, StorySnapshot>) -> SummaryView {
    let total_open = views
        .iter()
        .filter(|view| view.story.superstate == SuperState::Open)
        .count();

    let mut state_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut priority_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut type_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut blocked_count = 0;
    let mut flagged_count = 0;

    for view in views {
        *state_counts.entry(view.story.state.clone()).or_default() += 1;
        if view.story.priority != Priority::None {
            *priority_counts
                .entry(view.story.priority.as_str().to_string())
                .or_default() += 1;
        }
        *type_counts.entry(type_label(&view.story)).or_default() += 1;
        if !view.flagged_reasons.is_empty() {
            flagged_count += 1;
        }
        if view.story.superstate == SuperState::Open && !is_ready(&view.story, stories) {
            blocked_count += 1;
        }
    }

    SummaryView {
        total_open,
        total_closed: views.len() - total_open,
        by_state: state_counts.into_iter().collect(),
        by_priority: priority_counts.into_iter().collect(),
        by_type: type_counts.into_iter().collect(),
        blocked_count,
        flagged_count,
        ready_count: 0,
        ready_stories: Vec::new(),
    }
}

/// The dependency graph's parallel groups, as plain vectors.
fn collect_groups(groups: Vec<impl IntoIterator<Item = String>>) -> Vec<Vec<String>> {
    groups
        .into_iter()
        .map(|group| group.into_iter().collect())
        .collect()
}
