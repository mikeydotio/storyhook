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
//! Every id-list this file returns sorts by story *number*
//! ([`domain::story_number`]), not by the lexicographic order of the id
//! string — `list`, `search`, `epic list` and `phase show` via
//! [`sort_story_views`], `graph`'s roots/leaves/blocked-chain/parallel-groups
//! and `handoff`'s created/updated/closed sections via a direct sort at the
//! same call sites (SH-64; both used to iterate a map keyed by the id string
//! instead, `SH-1, SH-10, SH-11, SH-2, …`). `context`'s blocked list picked up
//! the same fix in passing — same file, same root cause, no separate story.
//!
//! `graph`'s `critical_path` is the one exception, deliberately: it is a
//! dependency *chain*, not a roster, and resorting it by id would replace the
//! path it reports with a different, meaningless one.
//!
//! `summary`, `report` and `context`'s ready lists rank by
//! [`domain::ready_order`](crate::domain::ready_order) (own priority, parent
//! epic priority, then story number) instead of bare story number — a total order the legacy comparator
//! did not have (SH-63). `next` extends that comparator into a dependency-aware
//! execution order: each result virtually completes before the next is chosen,
//! so a blocked successor can appear after its blocker (SH-450).
//! `report_data`'s `next_ids` is that same list, sharing the same
//! `execution_queue` helper `next` truncates — the web dashboard reads it as
//! the "Next" board sort and the List view's "Order" column, since the browser
//! cannot call `story next` itself (SH-407, SH-450).

use std::collections::{BTreeMap, BTreeSet};

use crate::cli::GraphMode;
use crate::domain::{
    self, DependencyGraph, Priority, StateDef, StorySnapshot, SuperState, compute_display_state,
    compute_integrity_issues, compute_progress, derive_family_relationships, has_children,
    is_claimable, is_ready, last_activity_type, parse_duration,
};
use crate::error::AppError;
use crate::output::{
    BlockedChainView, GraphOverview, GraphView, ProjectSnapshotView, ReferencedBy, ReportData,
    StaleInfo, StoryView, SummaryView,
};
use crate::store::{GlobalSeq, ProjectId, ReadOps, StoryNo, StoryQuery, StoryRow, partition_known};

use super::project_prefix;

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
    /// `--drafts` (SH-175): narrows to drafts only. Drafts are otherwise
    /// shown inline in the default `list` output — a draft is OPEN, so
    /// SH-409's closed/archived/deleted visibility filter never touches it,
    /// and this flag still never *hides* anything on its own the way it
    /// would if it were a default exclusion.
    pub drafts: bool,
    /// `--unassessed` (SH-359): narrows to stories whose priority nobody has
    /// ever chosen. Not the same as `--priority none`, which also returns every
    /// story deliberately parked there — the distinction this flag exists for.
    pub unassessed: bool,
    /// `--include-closed` (SH-409): widens the default OPEN-only visibility to
    /// also show CLOSED-superstate stories that are not archived (hidden).
    /// Implied by [`include_archived`](Self::include_archived) — an archived
    /// story is always closed too, since [`domain::fold_story`] clears
    /// `hidden_at` the instant a story reopens.
    pub include_closed: bool,
    /// `--include-archived` (SH-409): widens the default visibility to also
    /// show hidden (`story archive`d) stories. Implies
    /// [`include_closed`](Self::include_closed).
    pub include_archived: bool,
}

/// Optional narrowing shared by both doors onto the ready queue.
///
/// `story next` reads through it and `story claim --next` selects through it
/// inside the claim transaction. Keeping the three filters in one value makes
/// it impossible for the read and write paths to disagree about their inputs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadyQueueFilters<'a> {
    /// Require the `phase:<N>` label.
    pub phase: Option<&'a str>,
    /// Require membership in this epic's transitive descendant subtree.
    pub epic: Option<&'a str>,
    /// Omit stories carrying any value in this label CSV.
    pub exclude_label: Option<&'a str>,
}

/// What `story list` (SH-409) returns: the visible stories, plus a note when
/// the default visibility filter (or an explicit `--state <closed slug>`)
/// changed what showed up.
#[derive(Clone, Debug, Default)]
pub struct ListOutcome {
    pub views: Vec<StoryView>,
    pub message: Option<String>,
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
    /// A `BTreeMap`, so iterating it is lexicographic by id — `doctor` reads
    /// it for lookups and issue detection, where order is not observable.
    /// Every place order *is* observable (`graph`, `context`, `summary`) sorts
    /// its output explicitly instead of relying on this map's own iteration
    /// order (SH-63, SH-64).
    pub fn story_map(&self) -> Result<BTreeMap<String, StorySnapshot>, AppError> {
        story_map(self.tx, self.project)
    }

    /// Every story as a view, with the cross-story facts filled in.
    pub fn story_views(&self, include_derived: bool) -> Result<Vec<StoryView>, AppError> {
        story_views(self.tx, self.project, include_derived)
    }

    /// The state a claim (`story move <id> <active>`, a first commit
    /// mention) moves a story into — see [`domain::active_state`]. `None`
    /// for a legacy project with no resolvable one, in which case every
    /// [`is_claimable`] call below falls back to exactly [`is_ready`].
    fn active_state(&self) -> Result<Option<StateDef>, AppError> {
        Ok(domain::active_state(&self.tx.states(self.project)?))
    }

    /// `story show <id>` — one story, with its derived family relationships.
    ///
    /// Archived stories are found too: the legacy path looked in the open
    /// directory *and* the archive, and a story is one row here.
    pub fn show(&self, id: &str) -> Result<StoryView, AppError> {
        match story_view(self.tx, self.project, id)? {
            crate::output::Response::Story(view) => Ok(*view),
            other => Err(AppError::Storage(format!(
                "internal: story view answered with {other:?}"
            ))),
        }
    }

    /// Everything a client that holds a *model* needs, in one read.
    ///
    /// The catalog in **configured** order — `tx.states()`, never
    /// `tx.state_map()`, which is alphabetical and would put a board's columns
    /// in the wrong order — the members, and every unarchived story. One
    /// transaction, so a client cannot observe a catalog from one instant and
    /// stories from another.
    pub fn project_snapshot(&self) -> Result<ProjectSnapshotView, AppError> {
        let project = self
            .tx
            .project(self.project)?
            .ok_or_else(|| AppError::Storage(format!("project {} does not exist", self.project)))?;
        // `.draft(false)` (SH-175): the web board is a curated "what's
        // actionable" view, and the council verdict on SH-175 keeps that
        // curation separate from `story list`'s own filter chain rather than
        // routing both through the same one — `list` (SH-409) now excludes
        // closed/archived by default too, but independently, via its
        // own `is_visible` pass, not this query.
        // Do not pre-filter on the materialized `archived` column: an epic's
        // stored state is deliberately dormant while its effective state is
        // derived from children. Apply OPEN visibility after that projection.
        let story_rows = self
            .tx
            .stories(self.project, &StoryQuery::all().draft(false))?;
        // Carried alongside `stories`, not merged into it, for the
        // Drafts popover and its count badge — see the field's own doc
        // comment on `ProjectSnapshotView`.
        let draft_rows = self
            .tx
            .stories(self.project, &StoryQuery::all().draft(true))?;

        let mut effective = story_map(self.tx, self.project)?;
        let stories: Vec<_> = story_rows
            .iter()
            .filter_map(|row| effective.remove(&row.snapshot.id))
            .filter(|story| story.superstate == SuperState::Open)
            .collect();
        let drafts: Vec<_> = draft_rows
            .iter()
            .filter_map(|row| effective.remove(&row.snapshot.id))
            .collect();
        // `head_global_seqs` (SH-336) covers exactly the projected snapshots
        // returned above, even though effective OPEN filtering had to happen
        // after the broader materialized-row read.
        let visible_ids: BTreeSet<&str> = stories
            .iter()
            .chain(drafts.iter())
            .map(|story| story.id.as_str())
            .collect();
        let mut head_global_seqs = BTreeMap::new();
        for row in story_rows.iter().chain(draft_rows.iter()) {
            if visible_ids.contains(row.snapshot.id.as_str()) {
                head_global_seqs.insert(row.snapshot.id.clone(), row.head_global_seq);
            }
        }

        Ok(ProjectSnapshotView {
            slug: project.slug,
            prefix: project.prefix,
            states: self.tx.states(self.project)?,
            members: self.tx.members(self.project)?,
            stories,
            drafts,
            head_global_seqs,
        })
    }

    /// `story list` with every filter the CLI grammar allows.
    ///
    /// Filters are applied in the legacy order and are conjunctive. `--stale`
    /// is last because it also *annotates* the survivors with
    /// [`StaleInfo`], which costs one event read per remaining story.
    ///
    /// Visibility (SH-409) is applied *after* every other filter, over the
    /// same conjunctive result: a caller who also passed `--label CLI` sees
    /// counts of hidden stories that carry that label, not every hidden
    /// story in the project. A hard-deleted story has no row to filter.
    pub fn list(&self, filters: &ListFilters) -> Result<ListOutcome, AppError> {
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
            let wanted = label_csv_values(label_csv);
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
            let active = self.active_state()?;
            views.retain(|view| is_claimable(&view.story, &stories, active.as_ref()));
        }
        if filters.drafts {
            views.retain(|view| view.story.draft);
        }
        if filters.unassessed {
            views.retain(|view| !view.story.priority_assessed);
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

        // Naming a closed state is an unambiguous request for it — refusing
        // (or silently returning nothing) would be a worse answer than
        // showing it and saying why. Only the closed exclusion lifts: a
        // hidden (archived) story named by `--state` still needs
        // `--include-archived`, matching the dashboard's own Done column,
        // which shows closed cards but keeps archived ones behind a toggle.
        let lifted_state = match &filters.state {
            Some(slug) if !filters.include_closed && !filters.include_archived => {
                let states = self.tx.states(self.project)?;
                states
                    .iter()
                    .find(|def| &def.slug == slug)
                    .filter(|def| def.super_state == SuperState::Closed)
                    .map(|_| slug.as_str())
            }
            _ => None,
        };
        let show_closed =
            filters.include_closed || filters.include_archived || lifted_state.is_some();
        let show_archived = filters.include_archived;

        let (visible, hidden): (Vec<StoryView>, Vec<StoryView>) = views
            .into_iter()
            .partition(|view| is_visible(&view.story, show_closed, show_archived));

        Ok(ListOutcome {
            views: visible,
            message: build_visibility_message(lifted_state, &hidden),
        })
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

    /// `story next` — the executable story order a worker should follow.
    ///
    /// The first result is ready now. Each later result is what becomes
    /// executable after every preceding result virtually completes, so an
    /// open dependency constrains order instead of making its successor
    /// permanently absent (SH-450). Leaf stories only: a parent whose children
    /// are ready is not itself work. Every available frontier is sorted by
    /// [`domain::ready_order`]: own priority, parent epic priority, then story
    /// number — a total order, so
    /// asking twice with nothing changed in between always returns the same
    /// list (SH-63).
    pub fn next(&self, count: usize, phase: Option<&str>) -> Result<Vec<StoryView>, AppError> {
        self.next_filtered(
            count,
            ReadyQueueFilters {
                phase,
                ..ReadyQueueFilters::default()
            },
        )
    }

    /// The filtered form of [`Self::next`] shared with `story claim --next`.
    pub fn next_filtered(
        &self,
        count: usize,
        filters: ReadyQueueFilters<'_>,
    ) -> Result<Vec<StoryView>, AppError> {
        let views = self.story_views(false)?;
        let stories = view_map(&views);
        let active = self.active_state()?;
        let epic_descendants = match filters.epic {
            Some(epic_id) => {
                let epic = stories
                    .get(epic_id)
                    .ok_or_else(|| AppError::NotFound(format!("story `{epic_id}` not found")))?;
                if !domain::is_epic(epic) {
                    return Err(AppError::Validation(format!(
                        "story `{epic_id}` is not an epic"
                    )));
                }
                Some(domain::descendant_ids(&stories, epic_id))
            }
            None => None,
        };
        let excluded_labels = filters
            .exclude_label
            .map_or_else(Vec::new, label_csv_values);
        let mut execution = execution_queue(
            &views,
            &stories,
            active.as_ref(),
            filters.phase,
            epic_descendants.as_ref(),
            &excluded_labels,
        );
        execution.truncate(count);
        Ok(execution)
    }

    /// `story summary` — the project's rollup plus its top five ready stories.
    pub fn summary(&self) -> Result<SummaryView, AppError> {
        let views = self.story_views(false)?;
        let stories = view_map(&views);
        let mut summary = rollup(&views, &stories);

        let active = self.active_state()?;
        let mut ready: Vec<StoryView> = views
            .into_iter()
            .filter(|view| is_claimable(&view.story, &stories, active.as_ref()))
            .collect();
        sort_ready(&mut ready, &stories);
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

        let active = self.active_state()?;
        let mut ready_ids = Vec::new();
        let mut blocked_ids = Vec::new();
        for view in &views {
            if view.story.superstate == SuperState::Open {
                if is_claimable(&view.story, &stories, active.as_ref()) {
                    ready_ids.push(view.story.id.clone());
                } else if !is_ready(&view.story, &stories) {
                    blocked_ids.push(view.story.id.clone());
                }
                // Neither: an open, unblocked story someone has already
                // claimed (SH-236) — visible in `by_state`, but not offered
                // as ready work and not reported as stuck.
            }
        }
        // `rollup` leaves the ready count at zero because each surface fills it
        // from its own ready set; report's is the open ready ids it just
        // collected. (`is_claimable` is false for a closed story, so this
        // equals `summary`'s count — the two are computed differently and
        // agree.)
        summary.ready_count = ready_ids.len();

        let next_ids = execution_queue(&views, &stories, active.as_ref(), None, None, &[])
            .into_iter()
            .map(|view| view.story.id)
            .collect();

        Ok(ReportData {
            summary,
            stories: views,
            ready_ids,
            blocked_ids,
            next_ids,
        })
    }

    /// `story report` in its non-HTML form: the summary, with the ready
    /// preview filled in from the report's own ready set.
    pub fn report_summary(&self) -> Result<SummaryView, AppError> {
        let data = self.report_data()?;
        let ready_ids: BTreeSet<String> = data.ready_ids.into_iter().collect();
        let stories: BTreeMap<String, StorySnapshot> = data
            .stories
            .iter()
            .map(|view| (view.story.id.clone(), view.story.clone()))
            .collect();
        let mut ready: Vec<StoryView> = data
            .stories
            .into_iter()
            .filter(|view| ready_ids.contains(&view.story.id))
            .collect();
        sort_ready(&mut ready, &stories);

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
                    let mut ids: Vec<String> = open
                        .iter()
                        .filter(|story| {
                            !story.relationships.iter().any(|relation| {
                                relation.relation == wanted && open_neighbour(&relation.other_id)
                            })
                        })
                        .map(|story| story.id.clone())
                        .collect();
                    ids.sort_by_key(|id| domain::story_number(id));
                    ids
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
                let mut blocked: Vec<String> = graph.blocked_chain(id).into_iter().collect();
                blocked.sort_by_key(|id| domain::story_number(id));
                GraphView {
                    critical_path: None,
                    blocked_chain: Some(BlockedChainView {
                        source: id.clone(),
                        blocked,
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
    /// The JSON form is returned as a *string* rather than a typed value:
    /// `invoke::dispatch` wraps it in [`Response::RawJson`](crate::output::Response::RawJson),
    /// which prints it as-is regardless of the global `--json`/`--quiet`
    /// flags, the same fix the export wave made for `story export` (SH-66).
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

        let mut blocked: Vec<&StoryView> = views
            .iter()
            .filter(|view| {
                view.story.superstate == SuperState::Open && !is_ready(&view.story, &stories)
            })
            .collect();
        blocked.sort_by_key(|view| domain::story_number(&view.story.id));
        let active = self.active_state()?;
        let mut ready: Vec<&StoryView> = views
            .iter()
            .filter(|view| is_claimable(&view.story, &stories, active.as_ref()))
            .collect();
        ready.sort_by(|a, b| domain::ready_order(&a.story, &b.story, &stories));
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
    /// Each of the three sections below sorts by story number, not by the
    /// legacy directory-listing order `all_stories_legacy_order` still walks
    /// to build them (SH-64).
    ///
    /// # Each heading is bucketed on the fact it names (SH-280)
    ///
    /// This is the end-of-session record, read by a person or an agent who
    /// was not there — `plugins/story/hooks/stop-handoff.sh` runs
    /// `handoff --since 4h` and feeds the result to the next session
    /// verbatim. Nothing parses it, so its only contract is that every line
    /// is true.
    ///
    /// "Closed" used to mean `superstate == CLOSED` and `updated_at` inside
    /// the window, which is a different claim: `updated_at` is bumped by any
    /// append, and three writes reach a story that closed months ago —
    /// `story hide`/`unhide` (SH-43), `story comment` (SH-261) and
    /// `commit-sync`'s commit link (SH-279). Any one of them re-reported a
    /// long-finished story as this session's work.
    ///
    /// So "Closed" asks about `closed_at`, the fact it actually claims.
    /// `closed_at` is always the *current* closure: [`domain::fold_story`]
    /// clears it when a story reopens into an OPEN state and restamps it on
    /// the next close, so a story closed twice reports the closure that
    /// really happened in this window, not the first one.
    ///
    /// Membership is three separate questions — is `closed_at`,
    /// `created_at` or `updated_at` inside the window — rather than one
    /// `updated_at` pre-filter followed by a superstate split. The three
    /// timestamps are not ordered against one another: [`domain::fold_story`]
    /// sets `updated_at` to the *last replayed* event's `at`, not the
    /// greatest one seen, and nothing — no schema CHECK, no guard in
    /// `append_and_fold` — stops a later event from carrying an earlier
    /// `at` than its predecessor's. A restored import replaying an old
    /// export, or a system clock that stepped back, can leave `updated_at`
    /// behind `closed_at`. A single `updated_at` gate ahead of the split
    /// would then silently drop a closure that genuinely happened in the
    /// window — the same defect this method exists to fix, reached from the
    /// other direction.
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
            // `closed_at.is_some()` *is* `superstate == CLOSED` — tied
            // together by two schema CHECKs on the row this snapshot was
            // read from (`archived = (closed_at IS NOT NULL)` and
            // `(superstate = 'CLOSED') = archived`,
            // `0004_state_superstate_agree.sql`). Testing the superstate too
            // would restate that invariant in Rust and, on a row where the
            // two ever disagreed, quietly route around it instead of letting
            // the store's own constraint be what fails.
            if story
                .closed_at
                .as_deref()
                .is_some_and(|at| at >= threshold.as_str())
            {
                closed.push(story);
            } else if story.created_at.as_str() >= threshold.as_str() {
                created.push(story);
            } else if story.updated_at.as_str() >= threshold.as_str() {
                updated.push(story);
            }
        }
        for bucket in [&mut created, &mut updated, &mut closed] {
            bucket.sort_by_key(|story: &StorySnapshot| domain::story_number(&story.id));
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
    /// lexicographic id order — `storage::load_all_snapshots`'s order. Both
    /// callers re-sort what they take from this before it becomes visible
    /// output: `search` numerically via [`sort_story_views`], `handoff`
    /// per-bucket (SH-64). This traversal order is otherwise unobserved.
    fn all_stories_legacy_order(&self) -> Result<Vec<StorySnapshot>, AppError> {
        let stories = story_map(self.tx, self.project)?;
        let mut all = Vec::with_capacity(stories.len());
        for superstate in [SuperState::Open, SuperState::Closed] {
            all.extend(
                stories
                    .values()
                    .filter(|story| story.superstate == superstate)
                    .cloned(),
            );
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
///
/// `pub` so `tests/help_topic_references.rs` can pin the help text's stated
/// default against this value rather than a second hand-copied literal —
/// `src/help_topics.rs` named the wrong one (found in passing, SH-280).
pub const DEFAULT_HANDOFF_HOURS: i64 = 24;

/// Every story row in the project, keyed by id.
///
/// The one store read [`story_map`] and [`story_views`] both build on, so
/// `story_views` can read [`StoryRow::head_global_seq`] (SH-336) off the same
/// rows `story_map` already discards, without a second `tx.stories` trip.
fn story_rows(
    tx: &impl ReadOps,
    project: ProjectId,
) -> Result<BTreeMap<String, StoryRow>, AppError> {
    Ok(tx
        .stories(project, &StoryQuery::all())?
        .into_iter()
        .map(|row| (row.snapshot.id.clone(), row))
        .collect())
}

/// Every story in the project, keyed by id.
pub(crate) fn story_map(
    tx: &impl ReadOps,
    project: ProjectId,
) -> Result<BTreeMap<String, StorySnapshot>, AppError> {
    let mut stories: BTreeMap<String, StorySnapshot> = story_rows(tx, project)?
        .into_iter()
        .map(|(id, row)| (id, row.snapshot))
        .collect();
    domain::apply_computed_epic_states(&mut stories, &tx.states(project)?);
    Ok(stories)
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
    let rows = story_rows(tx, project)?;
    // `head_global_seq` (SH-336) travels alongside `stories` rather than
    // through it — `stories` is the snapshot map every function below already
    // expects, and widening its value type would touch every one of them for
    // a fact only the final `StoryView` construction needs.
    let head_global_seq: BTreeMap<String, GlobalSeq> = rows
        .iter()
        .map(|(id, row)| (id.clone(), row.head_global_seq))
        .collect();
    let mut stories: BTreeMap<String, StorySnapshot> = rows
        .into_iter()
        .map(|(id, row)| (id, row.snapshot))
        .collect();
    let states = tx.states(project)?;
    domain::apply_computed_epic_states(&mut stories, &states);
    // SH-286's rule reaches here too, and for the same reason it reaches the
    // doctor: absence from this map is not absence from the project, and a
    // `StoryView` that says otherwise is `story show` printing a dangling
    // relation to a story that is right there in the events.
    let unattested = super::integrity::unattested_endpoints(tx, project, &stories)?;
    let mut issues = compute_integrity_issues(&stories, &unattested);
    let derived_relationships = if include_derived {
        derive_family_relationships(&stories)
    } else {
        BTreeMap::new()
    };

    let progress: BTreeMap<String, _> = stories
        .values()
        .filter_map(|story| compute_progress(story, &stories).map(|p| (story.id.clone(), p)))
        .collect();

    let display_state: BTreeMap<String, String> = stories
        .values()
        .filter_map(|story| {
            compute_display_state(story, &stories, &states).map(|s| (story.id.clone(), s))
        })
        .collect();

    // Gated on `include_derived`, same as `derived_relationships` above and
    // for the same reason: `list`/`next`/`summary` and the rest of the
    // `false` callers have no use for it, so there is no reason to pay for a
    // project-wide `story_pr_links` read on every one of them. One read for
    // the whole project rather than one per story, when it does run — the
    // same shape `progress`/`display_state` use, and for the same reason:
    // `story_view` answers a single-story question by filtering the *whole*
    // project's views (see below), so a per-story query here would run once
    // per story in the project on every `story show`.
    //
    // `comment_mentions` rides the same gate and is computed here for the same
    // reason, with one difference worth naming: it is not a store read at all
    // (SH-220). Every comment thread is already folded into `stories` above, so
    // this is a scan over data in hand rather than a second trip to the store —
    // which is also why a retracted comment needs no invalidation path.
    let mut pr_links_by_id: BTreeMap<String, Vec<crate::store::PrLink>> = BTreeMap::new();
    let mut comment_mentions_by_id: BTreeMap<String, Vec<domain::CommentMention>> = BTreeMap::new();
    if include_derived {
        let prefix = project_prefix(tx, project)?;
        for (story_no, link) in tx.pr_links(project)? {
            pr_links_by_id
                .entry(story_no.to_id(&prefix))
                .or_default()
                .push(link);
        }
        comment_mentions_by_id = domain::derive_comment_mentions(&prefix, &stories);
    }

    let mut views = Vec::with_capacity(stories.len());
    for story in stories.into_values() {
        let id = story.id.clone();
        // Rendered back to sentences here, and only here: `flagged_reasons`
        // is a published JSON field (SH-244 typed the *checks*, not this).
        let mut flagged_reasons: Vec<String> = issues
            .remove(&id)
            .unwrap_or_default()
            .into_iter()
            .map(|finding| finding.message)
            .collect();
        if story
            .relationships
            .iter()
            .any(|relation| relation.relation == "obviated-by")
        {
            flagged_reasons.push("story is obviated by another story".to_string());
        }
        flagged_reasons.sort();
        flagged_reasons.dedup();

        let referenced_by = ReferencedBy {
            commits: story.referenced_by_commits.clone(),
            prs: pr_links_by_id.get(&id).cloned().unwrap_or_default(),
            comment_mentions: comment_mentions_by_id.remove(&id).unwrap_or_default(),
        };

        views.push(StoryView {
            story,
            derived_relationships: derived_relationships.get(&id).cloned().unwrap_or_default(),
            referenced_by,
            warnings: Vec::new(),
            flagged_reasons,
            stale_info: None,
            progress: progress.get(&id).cloned(),
            display_state: display_state.get(&id).cloned(),
            head_global_seq: head_global_seq.get(&id).copied(),
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
/// their head. [`domain::story_number`] is the same parser `ready_order` uses
/// for its own tiebreak — one story-number parser, not two.
pub fn sort_story_views(views: &mut [StoryView]) {
    views.sort_by_key(|view| domain::story_number(&view.story.id));
}

/// A view carrying only the snapshot — what `search` returns.
///
/// `referenced_by.commits` still comes along for free (it is folded into the
/// snapshot itself, no store read required); `referenced_by.prs` does not — a
/// project-wide join is exactly the per-story-view work this helper exists to
/// skip, same as `derived_relationships` below.
fn bare_view(story: StorySnapshot) -> StoryView {
    let referenced_by = ReferencedBy::commits_only(story.referenced_by_commits.clone());
    StoryView {
        story,
        derived_relationships: Vec::new(),
        referenced_by,
        warnings: Vec::new(),
        flagged_reasons: Vec::new(),
        stale_info: None,
        progress: None,
        display_state: None,
        // No row read here — a project-wide `story_rows` read is exactly the
        // per-view work this helper exists to skip, same as `referenced_by.prs`
        // above. `None` tells a comparator to fall back to its previous
        // tiebreak (SH-336).
        head_global_seq: None,
    }
}

/// Whether a story survives `story list`'s default visibility filter
/// (SH-409).
///
/// An archived (hidden) story is a *subset* of closed stories, not a sibling
/// category — [`domain::fold_story`] clears
/// `hidden_at` the instant a story's superstate resolves back to OPEN — so
/// `show_archived` alone, without `show_closed`, still has to reveal it.
fn is_visible(story: &StorySnapshot, show_closed: bool, show_archived: bool) -> bool {
    match story.superstate {
        SuperState::Open => true,
        SuperState::Closed => {
            if story.hidden_at.is_some() {
                show_archived
            } else {
                show_closed
            }
        }
    }
}

/// Builds `story list`'s `message` (SH-409): why closed stories showed up
/// (only when an explicit `--state <closed slug>` is what lifted the
/// exclusion, named by `lifted_state`) and what the default hid, in counts
/// that already reflect every other filter the caller passed — `hidden` is
/// the post-filter, pre-visibility remainder, so a hidden count next to
/// `--label CLI` counts only hidden stories that carry that label.
///
/// `None` when there is nothing to say: no lift happened and nothing was
/// hidden. Every other combination collapses onto one `; `-joined line
/// rather than a clause per condition, so `--json` and human output both
/// get one sentence to render instead of a growing list of them.
fn build_visibility_message(lifted_state: Option<&str>, hidden: &[StoryView]) -> Option<String> {
    let mut closed = 0usize;
    let mut archived = 0usize;
    for view in hidden {
        if view.story.hidden_at.is_some() {
            archived += 1;
        } else {
            closed += 1;
        }
    }

    let mut parts = Vec::new();
    if let Some(slug) = lifted_state {
        parts.push(format!(
            "including closed stories: `{slug}` is a closed state"
        ));
    }

    let hidden_count = closed + archived;
    if hidden_count > 0 {
        let mut clauses = Vec::new();
        if closed > 0 {
            clauses.push(format!("{closed} closed"));
        }
        if archived > 0 {
            clauses.push(format!("{archived} archived"));
        }
        let flags = match (closed > 0, archived > 0) {
            (true, true) => "--include-closed, --include-archived or --all",
            (true, false) => "--include-closed or --all",
            (false, true) => "--include-archived or --all",
            (false, false) => unreachable!("hidden_count > 0 implies closed or archived is set"),
        };
        parts.push(format!(
            "{} stor{} match but {} not shown — add {flags}",
            clauses.join(" and "),
            if hidden_count == 1 { "y" } else { "ies" },
            if hidden_count == 1 { "is" } else { "are" },
        ));
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("; "))
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

/// [`domain::ready_order`] over a list of views — the ready-list ordering
/// shared by `summary`, `report` and `context`. `execution_queue` uses the
/// same tuple as its ordered frontier rather than sorting a completed list.
fn sort_ready(views: &mut [StoryView], stories: &BTreeMap<String, StorySnapshot>) {
    views.sort_by(|a, b| domain::ready_order(&a.story, &b.story, stories));
}

/// The execution queue `story next --count N` hands out, in full and in order.
///
/// Kahn's topological traversal begins with every immediately claimable leaf.
/// Popping one node virtually completes it: each successor loses that open
/// predecessor and joins the frontier once none remain. The frontier's key is
/// the exact [`domain::ready_order`] tuple, so priority is reconsidered every
/// time work becomes executable rather than only once at the beginning.
/// Nothing here mutates a snapshot or the store (SH-450).
///
/// A story can enter the graph only if it would be claimable with dependency
/// lookups removed: this retains `is_claimable`'s closed/draft/blocked/
/// awaiting/obviated/active gates without duplicating them, and additionally
/// excludes [`domain::is_human_only`] — the one place the reserved
/// `human-only` label takes effect (SH-453, assumption A1 of
/// `docs/spec/full-auto-engine.md`). It filters *here*, in the queue, rather
/// than in `is_claimable` or `is_ready`, so a `human-only` story goes on
/// reading as ready to every surface a person consults while never being
/// offered as anyone's next assignment. Two consequences are deliberate:
/// `story claim --next` inherits the filter for free, because
/// `StoryService::claim_next` selects through [`QueryService::next`] — the
/// spec asks for exactly that, since filtering what an agent *looks at*
/// without filtering what it *takes* leaves the half that matters unguarded;
/// and `report_data`'s `next_ids` loses the story too, so the dashboard's
/// "Next" sort ranks its card last (`nextRank`'s `Infinity`) instead of
/// disagreeing with `story next` about the order — the card still renders,
/// and is still in `ready_ids`. A story `blocked-by` a `human-only` one
/// stays unranked for the reason the next paragraph gives, which is the
/// right answer here: that work genuinely cannot start until a person does
/// the human-only half. Every open
/// `blocked-by` target still contributes a predecessor, including one outside
/// that candidate set. Such a predecessor is never popped, deliberately, so a
/// claimed/manual-blocked/epic/out-of-phase blocker, or a dependency cycle,
/// leaves its downstream work unranked instead of pretending it can run.
///
/// This is the one implementation behind two callers: [`QueryService::next`],
/// which truncates it to `count`, and [`QueryService::report_data`], which
/// needs the ids in this exact order for the web dashboard's "Next" board sort
/// and List "Order" column (SH-407, SH-450) — the browser cannot call
/// `story next` itself,
/// `/api/v1/invoke` being loopback- and master-token-gated
/// (`src/api/rpc.rs`), so the server computes the queue once and ships the
/// order rather than the dashboard re-deriving it in JS, a duplicate of
/// this exact predicate this project has already paid for once (SH-240).
fn execution_queue(
    views: &[StoryView],
    stories: &BTreeMap<String, StorySnapshot>,
    active: Option<&StateDef>,
    phase: Option<&str>,
    epic_descendants: Option<&BTreeSet<String>>,
    excluded_labels: &[String],
) -> Vec<StoryView> {
    let no_open_blockers = BTreeMap::<String, StorySnapshot>::new();
    let candidates: BTreeMap<&str, &StoryView> = views
        .iter()
        .filter(|view| {
            is_claimable(&view.story, &no_open_blockers, active)
                && !has_children(&view.story)
                && !domain::is_human_only(&view.story)
                && phase.is_none_or(|phase| view.story.labels.contains(&format!("phase:{phase}")))
                && epic_descendants.is_none_or(|ids| ids.contains(&view.story.id))
                && !excluded_labels
                    .iter()
                    .any(|label| view.story.labels.contains(label))
        })
        .map(|view| (view.story.id.as_str(), view))
        .collect();

    let mut predecessor_counts = BTreeMap::<&str, usize>::new();
    let mut successors = BTreeMap::<&str, BTreeSet<&str>>::new();
    for (id, view) in &candidates {
        let open_predecessors: BTreeSet<&str> = view
            .story
            .relationships
            .iter()
            .filter(|relation| relation.relation == "blocked-by")
            .filter_map(|relation| {
                stories
                    .get(&relation.other_id)
                    .filter(|blocker| blocker.superstate == SuperState::Open)
                    .map(|_| relation.other_id.as_str())
            })
            .collect();
        predecessor_counts.insert(id, open_predecessors.len());
        for predecessor in open_predecessors {
            successors.entry(predecessor).or_default().insert(id);
        }
    }

    let mut frontier = BTreeSet::<(Priority, Priority, u64, &str)>::new();
    for (id, count) in &predecessor_counts {
        if *count == 0 {
            let story = &candidates[id].story;
            frontier.insert((
                story.priority.clone(),
                domain::parent_epic_priority(story, stories),
                domain::story_number(id),
                id,
            ));
        }
    }

    let mut execution = Vec::with_capacity(candidates.len());
    while let Some((_, _, _, id)) = frontier.pop_first() {
        execution.push(candidates[id].clone());
        if let Some(blocked) = successors.get(id) {
            for successor in blocked {
                let Some(count) = predecessor_counts.get_mut(successor) else {
                    continue;
                };
                *count -= 1;
                if *count == 0 {
                    let story = &candidates[successor].story;
                    frontier.insert((
                        story.priority.clone(),
                        domain::parent_epic_priority(story, stories),
                        domain::story_number(successor),
                        successor,
                    ));
                }
            }
        }
    }
    execution
}

/// The label CSV grammar shared by `story list --label` and
/// `story next --exclude-label`: split commas, trim whitespace, drop empty
/// entries, preserve case, and treat unknown names as ordinary values.
fn label_csv_values(csv: &str) -> Vec<String> {
    csv.split(',')
        .map(|raw| raw.trim().to_string())
        .filter(|label| !label.is_empty())
        .collect()
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

/// The dependency graph's parallel groups, as plain vectors — each group
/// sorted by story number, and the groups themselves ordered by their
/// lowest-numbered member, rather than by [`DependencyGraph`]'s internal
/// `BTreeSet<String>` iteration (SH-64).
fn collect_groups(groups: Vec<impl IntoIterator<Item = String>>) -> Vec<Vec<String>> {
    let mut groups: Vec<Vec<String>> = groups
        .into_iter()
        .map(|group| {
            let mut members: Vec<String> = group.into_iter().collect();
            members.sort_by_key(|id| domain::story_number(id));
            members
        })
        .collect();
    groups.sort_by_key(|members| members.first().map(|id| domain::story_number(id)));
    groups
}
