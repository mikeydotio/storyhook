use serde::Serialize;

use crate::domain::{Priority, ProgressRollup, StoryRelation, StorySnapshot, SuperState};
use crate::error::AppError;

#[derive(Clone, Debug, Serialize)]
pub struct StaleInfo {
    pub last_activity_at: String,
    pub last_activity_type: String,
    pub days_stale: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct StoryView {
    pub story: StorySnapshot,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derived_relationships: Vec<StoryRelation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flagged_reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_info: Option<StaleInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<ProgressRollup>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SummaryView {
    pub total_open: usize,
    pub total_closed: usize,
    pub by_state: Vec<(String, usize)>,
    pub by_priority: Vec<(String, usize)>,
    pub by_type: Vec<(String, usize)>,
    pub blocked_count: usize,
    pub flagged_count: usize,
    pub ready_count: usize,
    pub ready_stories: Vec<StoryView>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReportData {
    pub summary: SummaryView,
    pub stories: Vec<StoryView>,
    pub ready_ids: Vec<String>,
    pub blocked_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct GraphView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub critical_path: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_chain: Option<BlockedChainView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_groups: Option<Vec<Vec<String>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overview: Option<GraphOverview>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BlockedChainView {
    pub source: String,
    pub blocked: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct GraphOverview {
    pub total_open: usize,
    pub total_edges: usize,
    pub roots: Vec<String>,
    pub leaves: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PhaseView {
    pub phase: String,
    pub title: Option<String>,
    pub total: usize,
    pub done: usize,
    pub in_progress: usize,
    pub todo: usize,
    pub blocked: usize,
    pub story_ids: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum Response {
    Message(String),
    Story(Box<StoryView>),
    Stories(Vec<StoryView>, Option<String>),
    Summary(Box<SummaryView>),
    Graph(Box<GraphView>),
    Issues(Vec<String>),
    PhaseList(Vec<PhaseView>),
    /// Raw JSON output — bypasses normal envelope wrapping.
    /// Used by session-start and similar commands that need exact JSON control.
    RawJson(String),
}

#[derive(Serialize)]
struct JsonEnvelope<'a> {
    result: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    story: Option<&'a StoryView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stories: Option<&'a [StoryView]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<&'a SummaryView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    graph: Option<&'a GraphView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    issues: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phases: Option<&'a [PhaseView]>,
    #[serde(default, skip_serializing_if = "<[_]>::is_empty")]
    warnings: &'a [String],
    #[serde(default, skip_serializing_if = "<[_]>::is_empty")]
    flagged_reasons: &'a [String],
}

pub fn render_response(response: &Response, json: bool, quiet: bool) -> String {
    // RawJson always outputs directly, regardless of --json or --quiet flags
    if let Response::RawJson(raw) = response {
        return format!("{raw}\n");
    }

    if quiet {
        return String::new();
    }

    if json {
        return render_json(response);
    }

    render_human(response)
}

pub fn render_error(error: &AppError, json: bool) -> String {
    if json {
        if let AppError::StateConflict(expected, actual) = error {
            return format!(
                "{}\n",
                serde_json::json!({
                    "result": "conflict",
                    "error": error.to_string(),
                    "exit_code": error.exit_code(),
                    "expected": expected,
                    "actual": actual,
                })
            );
        }
        return format!(
            "{}\n",
            serde_json::json!({
                "result": "error",
                "error": error.to_string(),
                "exit_code": error.exit_code(),
            })
        );
    }

    format!("error: {error}\n")
}

fn render_json(response: &Response) -> String {
    let rendered = match response {
        Response::Message(message) => serde_json::to_string_pretty(&JsonEnvelope {
            result: "ok",
            message: Some(message),
            story: None,
            stories: None,
            summary: None,
            graph: None,
            issues: None,
            phases: None,
            warnings: &[],
            flagged_reasons: &[],
        }),
        Response::Story(view) => serde_json::to_string_pretty(&JsonEnvelope {
            result: "ok",
            message: None,
            story: Some(view.as_ref()),
            stories: None,
            summary: None,
            graph: None,
            issues: None,
            phases: None,
            warnings: &view.warnings,
            flagged_reasons: &view.flagged_reasons,
        }),
        Response::Stories(stories, msg) => serde_json::to_string_pretty(&JsonEnvelope {
            result: "ok",
            message: msg.as_deref(),
            story: None,
            stories: Some(stories),
            summary: None,
            graph: None,
            issues: None,
            phases: None,
            warnings: &[],
            flagged_reasons: &[],
        }),
        Response::Summary(summary) => serde_json::to_string_pretty(&JsonEnvelope {
            result: "ok",
            message: None,
            story: None,
            stories: None,
            summary: Some(summary.as_ref()),
            graph: None,
            issues: None,
            phases: None,
            warnings: &[],
            flagged_reasons: &[],
        }),
        Response::Graph(graph) => serde_json::to_string_pretty(&JsonEnvelope {
            result: "ok",
            message: None,
            story: None,
            stories: None,
            summary: None,
            graph: Some(graph.as_ref()),
            issues: None,
            phases: None,
            warnings: &[],
            flagged_reasons: &[],
        }),
        Response::Issues(issues) => serde_json::to_string_pretty(&JsonEnvelope {
            result: "ok",
            message: None,
            story: None,
            stories: None,
            summary: None,
            graph: None,
            issues: Some(issues),
            phases: None,
            warnings: &[],
            flagged_reasons: &[],
        }),
        Response::PhaseList(phase_views) => serde_json::to_string_pretty(&JsonEnvelope {
            result: "ok",
            message: None,
            story: None,
            stories: None,
            summary: None,
            graph: None,
            issues: None,
            phases: Some(phase_views),
            warnings: &[],
            flagged_reasons: &[],
        }),
        Response::RawJson(raw) => {
            // Should not reach here — render_response handles RawJson before calling render_json.
            return format!("{raw}\n");
        }
    }
    .expect("response should serialize");

    format!("{rendered}\n")
}

fn render_human(response: &Response) -> String {
    match response {
        Response::Message(message) => format!("{message}\n"),
        Response::Story(view) => render_story(view),
        Response::Stories(stories, msg) => {
            if stories.is_empty() {
                return "no stories found\n".to_string();
            }

            let mut body = String::new();
            if let Some(msg) = msg {
                body.push_str(msg);
                body.push('\n');
            }
            for story in stories {
                let flagged = if story.flagged_reasons.is_empty() {
                    ""
                } else {
                    " [flagged]"
                };
                let priority = if story.story.priority != Priority::None {
                    format!(" ({})", story.story.priority.as_str())
                } else {
                    String::new()
                };
                let type_badge = match story.story.story_type.as_deref() {
                    Some(t) => format!(" [{}]", t),
                    None => " [Default]".to_string(),
                };
                let progress_summary = if let Some(ref p) = story.progress {
                    format!(" ({}/{})", p.children_done, p.children_total)
                } else {
                    String::new()
                };
                let labels = if story.story.labels.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", story.story.labels.join(", "))
                };
                let stale = if let Some(ref info) = story.stale_info {
                    format!(
                        " [stale {}d, last: {}]",
                        info.days_stale, info.last_activity_type
                    )
                } else {
                    String::new()
                };
                let deleted = if story.story.deleted {
                    " [deleted]"
                } else {
                    ""
                };
                body.push_str(&format!(
                    "{} [{}]{}{} {}{}{}{}{}{}\n",
                    story.story.id,
                    story.story.state,
                    priority,
                    type_badge,
                    story.story.title,
                    progress_summary,
                    labels,
                    deleted,
                    flagged,
                    stale
                ));
            }
            body
        }
        Response::Summary(summary) => render_summary(summary),
        Response::Graph(graph) => render_graph(graph),
        Response::Issues(issues) => {
            if issues.is_empty() {
                return "no integrity issues found\n".to_string();
            }
            let mut body = String::new();
            for issue in issues {
                body.push_str(issue);
                body.push('\n');
            }
            body
        }
        Response::PhaseList(phase_views) => {
            if phase_views.is_empty() {
                return "no phases found\n".to_string();
            }
            let mut body = String::new();
            for pv in phase_views {
                let title_str = pv
                    .title
                    .as_ref()
                    .map(|t| format!(": {t}"))
                    .unwrap_or_default();
                let status = if pv.total == 0 {
                    "(empty)".to_string()
                } else {
                    let mut parts = Vec::new();
                    parts.push(format!("{}/{} done", pv.done, pv.total));
                    if pv.in_progress > 0 {
                        parts.push(format!("{} in-progress", pv.in_progress));
                    }
                    if pv.blocked > 0 {
                        parts.push(format!("{} blocked", pv.blocked));
                    }
                    format!("({})", parts.join(", "))
                };
                body.push_str(&format!(
                    "Phase {}{} -- {} {}\n",
                    pv.phase, title_str, pv.total, status
                ));
            }
            body
        }
        Response::RawJson(raw) => {
            // Should not reach here — render_response handles RawJson before calling render_human.
            format!("{raw}\n")
        }
    }
}

fn render_story(view: &StoryView) -> String {
    let story = &view.story;
    let assignee = story.assignee.as_deref().unwrap_or("-");
    let mut body = String::new();
    body.push_str(&format!("{} {}\n", story.id, story.title));
    let deleted_marker = if story.deleted { ", deleted" } else { "" };
    body.push_str(&format!(
        "state: {} ({}{deleted_marker})\n",
        story.state,
        story.superstate.as_str()
    ));
    body.push_str(&format!("assignee: {assignee}\n"));
    body.push_str(&format!("priority: {}\n", story.priority.as_str()));
    let type_display = story.story_type.as_deref().unwrap_or("Default");
    body.push_str(&format!("type: {type_display}\n"));
    if story.labels.is_empty() {
        body.push_str("labels: -\n");
    } else {
        body.push_str(&format!("labels: {}\n", story.labels.join(", ")));
    }
    if let Some(awaiting) = &story.awaiting {
        body.push_str(&format!("awaiting: {awaiting}\n"));
    }

    if let Some(closed_at) = &story.closed_at {
        body.push_str(&format!("closed_at: {closed_at}\n"));
    }

    if let Some(reason) = &story.deleted_reason {
        body.push_str(&format!("deleted_reason: {reason}\n"));
    }

    if view.flagged_reasons.is_empty() {
        body.push_str("flagged: no\n");
    } else {
        body.push_str("flagged: yes\n");
        for reason in &view.flagged_reasons {
            body.push_str(&format!("flagged_reason: {reason}\n"));
        }
    }

    if !story.relationships.is_empty() {
        body.push_str("relationships:\n");
        for relation in &story.relationships {
            body.push_str(&format!("- {} {}\n", relation.relation, relation.other_id));
        }
    }

    if !view.derived_relationships.is_empty() {
        body.push_str("derived_relationships:\n");
        for relation in &view.derived_relationships {
            body.push_str(&format!("- {} {}\n", relation.relation, relation.other_id));
        }
    }

    if let Some(ref progress) = view.progress {
        let pct = (progress.children_done as f64 / progress.children_total as f64 * 100.0) as u64;
        body.push_str(&format!(
            "progress: {}/{} children done ({}%)\n",
            progress.children_done, progress.children_total, pct
        ));
    }

    if !story.comments.is_empty() {
        body.push_str("comments:\n");
        for comment in &story.comments {
            body.push_str(&format!("- {} {}\n", comment.at, comment.text));
        }
    }

    body
}

fn render_summary(summary: &SummaryView) -> String {
    let mut body = String::new();
    let total = summary.total_open + summary.total_closed;
    body.push_str(&format!(
        "stories: {} ({} open, {} closed)\n",
        total, summary.total_open, summary.total_closed
    ));

    if !summary.by_state.is_empty() {
        body.push_str("by state:\n");
        for (state, count) in &summary.by_state {
            body.push_str(&format!("  {state}: {count}\n"));
        }
    }

    if summary.by_priority.iter().any(|(_, c)| *c > 0) {
        body.push_str("by priority:\n");
        for (priority, count) in &summary.by_priority {
            if *count > 0 {
                body.push_str(&format!("  {priority}: {count}\n"));
            }
        }
    }

    if !summary.by_type.is_empty() {
        body.push_str("by type:\n");
        for (type_name, count) in &summary.by_type {
            body.push_str(&format!("  {type_name}: {count}\n"));
        }
    }

    body.push_str(&format!("blocked: {}\n", summary.blocked_count));
    body.push_str(&format!("flagged: {}\n", summary.flagged_count));
    body.push_str(&format!("ready: {}\n", summary.ready_count));

    if !summary.ready_stories.is_empty() {
        body.push_str("ready stories:\n");
        for view in &summary.ready_stories {
            let priority = if view.story.priority != Priority::None {
                format!(" ({})", view.story.priority.as_str())
            } else {
                String::new()
            };
            body.push_str(&format!(
                "  {} [{}]{} {}\n",
                view.story.id, view.story.state, priority, view.story.title
            ));
        }
    }

    body
}

fn render_graph(graph: &GraphView) -> String {
    let mut body = String::new();

    if let Some(ref overview) = graph.overview {
        body.push_str(&format!("open stories: {}\n", overview.total_open));
        body.push_str(&format!("dependency edges: {}\n", overview.total_edges));
        if !overview.roots.is_empty() {
            body.push_str(&format!(
                "roots (no predecessors): {}\n",
                overview.roots.join(", ")
            ));
        }
        if !overview.leaves.is_empty() {
            body.push_str(&format!(
                "leaves (no successors): {}\n",
                overview.leaves.join(", ")
            ));
        }
    }

    if let Some(ref path) = graph.critical_path {
        if path.is_empty() {
            body.push_str("critical path: (none)\n");
        } else {
            body.push_str(&format!("critical path ({} stories):\n", path.len()));
            body.push_str(&format!("  {}\n", path.join(" -> ")));
        }
    }

    if let Some(ref chain) = graph.blocked_chain {
        if chain.blocked.is_empty() {
            body.push_str(&format!("nothing is blocked by {}\n", chain.source));
        } else {
            body.push_str(&format!(
                "blocked by {} ({} stories):\n",
                chain.source,
                chain.blocked.len()
            ));
            for id in &chain.blocked {
                body.push_str(&format!("  {id}\n"));
            }
        }
    }

    if let Some(ref groups) = graph.parallel_groups {
        body.push_str(&format!("parallel groups: {}\n", groups.len()));
        for (i, group) in groups.iter().enumerate() {
            body.push_str(&format!("  group {}: {}\n", i + 1, group.join(", ")));
        }
    }

    body
}

pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(ch),
        }
    }
    out
}

pub fn render_html_report(
    summary: &SummaryView,
    stories: &[StoryView],
    is_ready_fn: &dyn Fn(&str) -> bool,
    is_blocked_fn: &dyn Fn(&str) -> bool,
) -> String {
    let total = summary.total_open + summary.total_closed;

    let state_colors = [
        "#3b82f6", "#10b981", "#f59e0b", "#ef4444", "#8b5cf6", "#ec4899", "#06b6d4", "#84cc16",
        "#f97316", "#6366f1",
    ];

    let state_bar = build_state_bar(summary, total, &state_colors);
    let state_legend = build_state_legend(summary, total, &state_colors);
    let priority_html = build_priority_section(summary);
    let type_html = build_type_section(summary);
    let table_rows = build_table_rows(stories, is_ready_fn, is_blocked_fn);

    let stories_table = if stories.is_empty() {
        String::from("<p class=\"empty\">No stories in this project.</p>")
    } else {
        format!(
            "<table>\n<thead><tr><th>ID</th><th>Title</th><th>State</th><th>Priority</th><th>Labels</th><th>Assignee</th><th>Updated</th></tr></thead>\n<tbody>\n{}</tbody>\n</table>",
            table_rows
        )
    };

    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Storyhook Report</title>
<style>
:root {{
  --bg: #ffffff;
  --fg: #1a1a2e;
  --bg-card: #f8f9fa;
  --border: #e2e8f0;
  --muted: #94a3b8;
  --row-blocked-bg: #fef2f2;
  --row-ready-bg: #f0fdf4;
  --row-blocked-border: #fca5a5;
  --row-ready-border: #86efac;
  --table-header-bg: #f1f5f9;
  --table-hover: #f8fafc;
}}
@media (prefers-color-scheme: dark) {{
  :root {{
    --bg: #0f172a;
    --fg: #e2e8f0;
    --bg-card: #1e293b;
    --border: #334155;
    --muted: #64748b;
    --row-blocked-bg: #450a0a;
    --row-ready-bg: #052e16;
    --row-blocked-border: #991b1b;
    --row-ready-border: #166534;
    --table-header-bg: #1e293b;
    --table-hover: #1e293b;
  }}
}}
* {{ margin:0; padding:0; box-sizing:border-box; }}
body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; background:var(--bg); color:var(--fg); line-height:1.6; padding:2rem; max-width:1200px; margin:0 auto; }}
h1 {{ font-size:1.5rem; font-weight:700; margin-bottom:0.25rem; }}
.subtitle {{ color:var(--muted); font-size:0.875rem; margin-bottom:1.5rem; }}
.stats {{ display:flex; gap:1rem; flex-wrap:wrap; margin-bottom:1.5rem; }}
.stat-card {{ background:var(--bg-card); border:1px solid var(--border); border-radius:0.5rem; padding:1rem 1.25rem; min-width:120px; }}
.stat-value {{ font-size:1.5rem; font-weight:700; }}
.stat-label {{ font-size:0.75rem; color:var(--muted); text-transform:uppercase; letter-spacing:0.05em; }}
.section {{ margin-bottom:1.5rem; }}
.section-title {{ font-size:0.875rem; font-weight:600; text-transform:uppercase; letter-spacing:0.05em; color:var(--muted); margin-bottom:0.5rem; }}
.bar-chart {{ display:flex; height:1.5rem; border-radius:0.375rem; overflow:hidden; margin-bottom:0.5rem; }}
.bar-segment {{ min-width:2px; transition:width 0.3s; }}
.legend {{ display:flex; flex-wrap:wrap; gap:0.75rem; font-size:0.8125rem; }}
.legend-item {{ display:inline-flex; align-items:center; gap:0.25rem; }}
.legend-dot {{ width:0.625rem; height:0.625rem; border-radius:50%; display:inline-block; }}
.priorities {{ display:flex; gap:0.5rem; flex-wrap:wrap; }}
.priority-badge {{ font-size:0.75rem; padding:0.125rem 0.5rem; border-radius:9999px; font-weight:500; }}
.priority-critical {{ background:#fef2f2; color:#dc2626; border:1px solid #fca5a5; }}
.priority-high {{ background:#fff7ed; color:#ea580c; border:1px solid #fdba74; }}
.priority-medium {{ background:#fefce8; color:#ca8a04; border:1px solid #fde047; }}
.priority-low {{ background:#f0fdf4; color:#16a34a; border:1px solid #86efac; }}
.priority-none {{ background:var(--bg-card); color:var(--muted); border:1px solid var(--border); }}
@media (prefers-color-scheme: dark) {{
  .priority-critical {{ background:#450a0a; color:#f87171; border-color:#991b1b; }}
  .priority-high {{ background:#431407; color:#fb923c; border-color:#9a3412; }}
  .priority-medium {{ background:#422006; color:#facc15; border-color:#854d0e; }}
  .priority-low {{ background:#052e16; color:#4ade80; border-color:#166534; }}
}}
table {{ width:100%; border-collapse:collapse; font-size:0.875rem; }}
thead th {{ text-align:left; padding:0.625rem 0.75rem; background:var(--table-header-bg); border-bottom:2px solid var(--border); font-weight:600; font-size:0.75rem; text-transform:uppercase; letter-spacing:0.05em; color:var(--muted); }}
tbody td {{ padding:0.5rem 0.75rem; border-bottom:1px solid var(--border); vertical-align:top; }}
tbody tr:hover {{ background:var(--table-hover); }}
.row-blocked {{ background:var(--row-blocked-bg); border-left:3px solid var(--row-blocked-border); }}
.row-ready {{ background:var(--row-ready-bg); border-left:3px solid var(--row-ready-border); }}
.col-id {{ font-family:ui-monospace,SFMono-Regular,monospace; white-space:nowrap; font-size:0.8125rem; }}
.col-date {{ white-space:nowrap; color:var(--muted); font-size:0.8125rem; }}
.label {{ display:inline-block; font-size:0.6875rem; padding:0.0625rem 0.375rem; border-radius:9999px; background:var(--bg-card); border:1px solid var(--border); margin-right:0.25rem; }}
.muted {{ color:var(--muted); }}
.empty {{ text-align:center; padding:2rem; color:var(--muted); }}
</style>
</head>
<body>
<h1>Storyhook Report</h1>
<p class="subtitle">Generated {generated_at} &middot; {total} stories</p>

<div class="stats">
<div class="stat-card"><div class="stat-value">{total}</div><div class="stat-label">Total</div></div>
<div class="stat-card"><div class="stat-value">{open}</div><div class="stat-label">Open</div></div>
<div class="stat-card"><div class="stat-value">{closed}</div><div class="stat-label">Closed</div></div>
<div class="stat-card"><div class="stat-value">{blocked}</div><div class="stat-label">Blocked</div></div>
<div class="stat-card"><div class="stat-value">{ready}</div><div class="stat-label">Ready</div></div>
</div>

<div class="section">
<div class="section-title">State Distribution</div>
<div class="bar-chart">{state_bar}</div>
<div class="legend">{state_legend}</div>
</div>

<div class="section">
<div class="section-title">Priority Breakdown</div>
<div class="priorities">{priority_html}</div>
</div>

<div class="section">
<div class="section-title">Type Breakdown</div>
<div class="priorities">{type_html}</div>
</div>

<div class="section">
<div class="section-title">Stories</div>
{stories_table}
</div>

</body>
</html>
"##,
        generated_at = html_escape(&chrono::Utc::now().format("%Y-%m-%d %H:%M UTC").to_string()),
        total = total,
        open = summary.total_open,
        closed = summary.total_closed,
        blocked = summary.blocked_count,
        ready = summary.ready_count,
        state_bar = state_bar,
        state_legend = state_legend,
        priority_html = priority_html,
        type_html = type_html,
        stories_table = stories_table,
    )
}

fn build_state_bar(summary: &SummaryView, total: usize, colors: &[&str]) -> String {
    let mut html = String::new();
    if total > 0 {
        for (i, (state, count)) in summary.by_state.iter().enumerate() {
            let pct = (*count as f64 / total as f64) * 100.0;
            if pct > 0.0 {
                let color = colors[i % colors.len()];
                html.push_str(&format!(
                    "<div class=\"bar-segment\" style=\"width:{pct:.1}%;background:{color}\" title=\"{}: {} ({pct:.0}%)\"></div>",
                    html_escape(state), count
                ));
            }
        }
    }
    html
}

fn build_state_legend(summary: &SummaryView, total: usize, colors: &[&str]) -> String {
    let mut html = String::new();
    for (i, (state, count)) in summary.by_state.iter().enumerate() {
        let color = colors[i % colors.len()];
        let pct = if total > 0 {
            (*count as f64 / total as f64) * 100.0
        } else {
            0.0
        };
        html.push_str(&format!(
            "<span class=\"legend-item\"><span class=\"legend-dot\" style=\"background:{color}\"></span>{} {} ({pct:.0}%)</span>",
            html_escape(state), count
        ));
    }
    html
}

fn build_priority_section(summary: &SummaryView) -> String {
    let mut html = String::new();
    for (priority, count) in &summary.by_priority {
        if *count > 0 {
            let cls = match priority.as_str() {
                "critical" => "priority-critical",
                "high" => "priority-high",
                "medium" => "priority-medium",
                "low" => "priority-low",
                _ => "priority-none",
            };
            html.push_str(&format!(
                "<span class=\"priority-badge {cls}\">{}: {count}</span>",
                html_escape(priority)
            ));
        }
    }
    if html.is_empty() {
        html.push_str("<span class=\"muted\">No priorities set</span>");
    }
    html
}

fn build_type_section(summary: &SummaryView) -> String {
    let mut html = String::new();
    for (type_name, count) in &summary.by_type {
        html.push_str(&format!(
            "<span class=\"priority-badge priority-none\">{}: {count}</span>",
            html_escape(type_name)
        ));
    }
    if html.is_empty() {
        html.push_str("<span class=\"muted\">No types set</span>");
    }
    html
}

fn build_table_rows(
    stories: &[StoryView],
    is_ready_fn: &dyn Fn(&str) -> bool,
    is_blocked_fn: &dyn Fn(&str) -> bool,
) -> String {
    let mut sorted: Vec<&StoryView> = stories.iter().collect();
    sorted.sort_by(|a, b| {
        a.story
            .priority
            .cmp(&b.story.priority)
            .then_with(|| a.story.state.cmp(&b.story.state))
            .then_with(|| a.story.title.cmp(&b.story.title))
    });

    let mut html = String::new();
    for view in &sorted {
        let s = &view.story;
        let row_class = if s.superstate == SuperState::Open && is_blocked_fn(&s.id) {
            " class=\"row-blocked\""
        } else if is_ready_fn(&s.id) {
            " class=\"row-ready\""
        } else {
            ""
        };

        let priority_cls = match s.priority {
            Priority::Critical => "priority-critical",
            Priority::High => "priority-high",
            Priority::Medium => "priority-medium",
            Priority::Low => "priority-low",
            Priority::None => "priority-none",
        };

        let labels_html = if s.labels.is_empty() {
            String::from("<span class=\"muted\">-</span>")
        } else {
            s.labels
                .iter()
                .map(|l| format!("<span class=\"label\">{}</span>", html_escape(l)))
                .collect::<Vec<_>>()
                .join(" ")
        };

        let assignee = s
            .assignee
            .as_deref()
            .map(html_escape)
            .unwrap_or_else(|| String::from("<span class=\"muted\">-</span>"));

        let updated = &s.updated_at;
        let updated_display = if updated.len() >= 10 {
            html_escape(&updated[..10])
        } else {
            html_escape(updated)
        };

        html.push_str(&format!(
            "<tr{row_class}><td class=\"col-id\">{}</td><td>{}</td><td>{}</td><td><span class=\"priority-badge {priority_cls}\">{}</span></td><td>{labels_html}</td><td>{assignee}</td><td class=\"col-date\">{updated_display}</td></tr>\n",
            html_escape(&s.id),
            html_escape(&s.title),
            html_escape(&s.state),
            html_escape(s.priority.as_str()),
        ));
    }
    html
}
