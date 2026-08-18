//! The curated tool surface: which MCP tools exist, what JSON they accept,
//! and how a tool call becomes a `story` invocation.
//!
//! **The anti-drift design, in one sentence:** a tool call never builds an
//! [`Invocation`] by hand. It builds an `argv` — the same shape
//! `std::env::args()` would hand `main`, one `String` per token — and calls
//! [`crate::cli::parse_invocation`], the exact function the real CLI binary
//! calls. There is no second parser here to fall out of sync with the first;
//! there is only one door, reached two ways. `tests/mcp_tool_drift.rs`
//! ("same-answer-two-doors") pins that a tool's arguments produce the
//! identical `Invocation` the equivalent typed command would.
//!
//! [`tool_for_variant`] is the other half: an exhaustive `match` with no
//! wildcard arm, so a 65th [`Invocation`] variant fails this file to compile
//! until someone has decided whether it gets a tool.
//!
//! This module reads no ambient state. Every tool takes `project` (and,
//! where the underlying verb writes, `actor`) as an explicit JSON argument —
//! never `$STORYHOOK_PROJECT`, never `$STORYHOOK_ACTOR`, never a working
//! directory. A long-lived server that read those from its own environment
//! would resolve every call as whichever shell happened to start it, which
//! is the SH-246 mistake recurring one layer out; see
//! `docs/spec/mcp-server.md`. `tests/mcp_tool_drift.rs` also enforces this
//! structurally: neither `std::env::var` nor `current_dir` may appear
//! anywhere under `src/mcp/`.

use serde_json::{Map, Value, json};

use crate::cli::{self, Invocation};

/// One argument a tool declares in its JSON Schema.
///
/// Purely descriptive — it drives [`json_schema`] and nothing else. A field
/// this table gets wrong does not silently misconstrue a command: it either
/// fails to deserialize (a name `parse_invocation` does not expect) or is
/// caught by `tests/mcp_tool_drift.rs` comparing this door's answer against
/// the CLI's.
pub struct FieldSpec {
    pub name: &'static str,
    pub kind: FieldKind,
    pub required: bool,
    pub description: &'static str,
}

/// The JSON types this server's tool arguments use — a small, closed set
/// because every field here is ultimately one `story` CLI flag or
/// positional, and the CLI's own vocabulary is `String`, `bool`, `usize` or
/// a list of strings.
pub enum FieldKind {
    Str,
    Bool,
    Uint,
    StrArray,
}

impl FieldKind {
    fn schema(&self) -> Value {
        match self {
            Self::Str => json!({ "type": "string" }),
            Self::Bool => json!({ "type": "boolean" }),
            Self::Uint => json!({ "type": "integer", "minimum": 0 }),
            Self::StrArray => json!({ "type": "array", "items": { "type": "string" } }),
        }
    }
}

/// Two arguments every tool takes, on top of its own [`FieldSpec`] list —
/// never part of `argv`, because they answer "which project" and "who is
/// asking", not "what to do".
const COMMON_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "project",
        kind: FieldKind::Str,
        required: true,
        description: "The storyhook project slug to act on. Required on every call: this \
                       server never infers a project from a working directory, because it has \
                       none of its own. `story project list` (via a host's shell, or a future \
                       tool) shows the slugs a store has.",
    },
    FieldSpec {
        name: "actor",
        kind: FieldKind::Str,
        required: false,
        description: "Who to record as having made this change, for the story's write history. \
                       Omit to leave it unrecorded.",
    },
];

/// The one function anywhere in this module that assembles an
/// `inputSchema`. `tests/mcp_tool_drift.rs::exactly_one_schema_object_is_built`
/// counts how many places construct a `"properties"` key and fails above
/// one — the guard against a second, hand-written schema for one tool ever
/// being added beside this one, which is the exact shape of v1's failure
/// (SH-9/SH-17: a field added to a command, never re-synced into a
/// hand-written schema).
fn json_schema(fields: &[FieldSpec]) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for field in COMMON_FIELDS.iter().chain(fields) {
        properties.insert(field.name.to_string(), {
            let mut schema = field.kind.schema();
            schema["description"] = Value::String(field.description.to_string());
            schema
        });
        if field.required {
            required.push(Value::String(field.name.to_string()));
        }
    }
    json!({
        "type": "object",
        "properties": Value::Object(properties),
        "required": Value::Array(required),
        "additionalProperties": false,
    })
}

/// Builds the `story <verb> ...` argv a tool's arguments describe. Returns
/// a message (never a panic) for anything malformed enough that no argv can
/// be built at all — an absent required field, chiefly. Everything else (an
/// unknown state slug, a story that does not exist) is left for
/// `parse_invocation` and the daemon to refuse, the same as it refuses a
/// typed command.
pub type BuildArgv = fn(&Map<String, Value>) -> Result<Vec<String>, String>;

/// One curated tool: its name, its schema's field list, and how its
/// arguments become an `argv` for [`cli::parse_invocation`].
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub fields: &'static [FieldSpec],
    pub build_argv: BuildArgv,
}

fn str_arg(args: &Map<String, Value>, name: &str) -> Option<String> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn require_str(args: &Map<String, Value>, name: &str) -> Result<String, String> {
    str_arg(args, name).ok_or_else(|| format!("`{name}` is required"))
}

fn bool_arg(args: &Map<String, Value>, name: &str) -> bool {
    args.get(name).and_then(Value::as_bool).unwrap_or(false)
}

fn str_array_arg(args: &Map<String, Value>, name: &str) -> Vec<String> {
    args.get(name)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Pushes `--flag <value>` when `name` is present and non-empty.
fn push_opt(argv: &mut Vec<String>, args: &Map<String, Value>, name: &str, flag: &str) {
    if let Some(value) = str_arg(args, name) {
        argv.push(flag.to_string());
        argv.push(value);
    }
}

/// Pushes a bare `--flag` when `name` is `true`.
fn push_flag(argv: &mut Vec<String>, args: &Map<String, Value>, name: &str, flag: &str) {
    if bool_arg(args, name) {
        argv.push(flag.to_string());
    }
}

fn build_list(args: &Map<String, Value>) -> Result<Vec<String>, String> {
    let mut argv = vec!["list".to_string()];
    push_opt(&mut argv, args, "state", "--state");
    push_opt(&mut argv, args, "assignee", "--assignee");
    push_flag(&mut argv, args, "flagged", "--flagged");
    push_opt(&mut argv, args, "priority", "--priority");
    push_opt(&mut argv, args, "label", "--label");
    push_opt(&mut argv, args, "created_after", "--created-after");
    push_opt(&mut argv, args, "updated_after", "--updated-after");
    push_flag(&mut argv, args, "blocked", "--blocked");
    push_flag(&mut argv, args, "ready", "--ready");
    push_opt(&mut argv, args, "stale", "--stale");
    push_opt(&mut argv, args, "phase", "--phase");
    push_opt(&mut argv, args, "story_type", "--type");
    push_flag(&mut argv, args, "drafts", "--drafts");
    push_flag(&mut argv, args, "include_closed", "--include-closed");
    push_flag(&mut argv, args, "include_archived", "--include-archived");
    push_flag(&mut argv, args, "all", "--all");
    Ok(argv)
}

fn build_next(args: &Map<String, Value>) -> Result<Vec<String>, String> {
    let mut argv = vec!["next".to_string()];
    if let Some(count) = args.get("count").and_then(Value::as_u64) {
        argv.push("--count".to_string());
        argv.push(count.to_string());
    }
    push_opt(&mut argv, args, "phase", "--phase");
    Ok(argv)
}

fn build_show(args: &Map<String, Value>) -> Result<Vec<String>, String> {
    Ok(vec!["show".to_string(), require_str(args, "id")?])
}

fn build_search(args: &Map<String, Value>) -> Result<Vec<String>, String> {
    Ok(vec!["search".to_string(), require_str(args, "query")?])
}

fn build_summary(_args: &Map<String, Value>) -> Result<Vec<String>, String> {
    Ok(vec!["summary".to_string()])
}

fn build_new(args: &Map<String, Value>) -> Result<Vec<String>, String> {
    let mut argv = vec!["new".to_string(), require_str(args, "title")?];
    push_opt(&mut argv, args, "state", "--state");
    push_opt(&mut argv, args, "story_type", "--type");
    push_opt(&mut argv, args, "description", "--description");
    push_opt(&mut argv, args, "priority", "--priority");
    push_opt(&mut argv, args, "assignee", "--assignee");
    for label in str_array_arg(args, "labels") {
        argv.push("--label".to_string());
        argv.push(label);
    }
    push_flag(&mut argv, args, "draft", "--draft");
    Ok(argv)
}

/// `--if-state`/`--reason` must appear as a contiguous run immediately after
/// `<state>`, ahead of the free-text comment — `parse_move`'s own rule
/// (SH-205, SH-62). This mirrors that ordering exactly rather than
/// reimplementing its reasoning.
fn build_move(args: &Map<String, Value>) -> Result<Vec<String>, String> {
    let mut argv = vec![
        "move".to_string(),
        require_str(args, "id")?,
        require_str(args, "state")?,
    ];
    push_opt(&mut argv, args, "if_state", "--if-state");
    push_opt(&mut argv, args, "reason", "--reason");
    if let Some(comment) = str_arg(args, "comment") {
        argv.push(comment);
    }
    Ok(argv)
}

fn build_comment(args: &Map<String, Value>) -> Result<Vec<String>, String> {
    Ok(vec![
        "comment".to_string(),
        require_str(args, "id")?,
        require_str(args, "text")?,
    ])
}

fn build_assign(args: &Map<String, Value>) -> Result<Vec<String>, String> {
    Ok(vec![
        "assign".to_string(),
        require_str(args, "id")?,
        require_str(args, "member")?,
    ])
}

fn build_prioritize(args: &Map<String, Value>) -> Result<Vec<String>, String> {
    Ok(vec![
        "prioritize".to_string(),
        require_str(args, "id")?,
        require_str(args, "priority")?,
    ])
}

fn build_label(args: &Map<String, Value>) -> Result<Vec<String>, String> {
    let id = require_str(args, "id")?;
    let labels = str_array_arg(args, "labels");
    if labels.is_empty() {
        return Err("`labels` must have at least one entry".to_string());
    }
    Ok(vec!["label".to_string(), id, labels.join(",")])
}

fn build_relate(args: &Map<String, Value>) -> Result<Vec<String>, String> {
    Ok(vec![
        "relate".to_string(),
        require_str(args, "a")?,
        require_str(args, "relation")?,
        require_str(args, "b")?,
    ])
}

fn build_block(args: &Map<String, Value>) -> Result<Vec<String>, String> {
    Ok(vec![
        "block".to_string(),
        require_str(args, "id")?,
        require_str(args, "reason")?,
    ])
}

fn build_unblock(args: &Map<String, Value>) -> Result<Vec<String>, String> {
    Ok(vec!["unblock".to_string(), require_str(args, "id")?])
}

fn build_set(args: &Map<String, Value>) -> Result<Vec<String>, String> {
    let mut argv = vec!["set".to_string(), require_str(args, "id")?];
    push_opt(&mut argv, args, "title", "--title");
    push_opt(&mut argv, args, "state", "--state");
    push_opt(&mut argv, args, "priority", "--priority");
    push_opt(&mut argv, args, "assignee", "--assignee");
    push_opt(&mut argv, args, "labels", "--labels");
    push_opt(&mut argv, args, "blocked", "--blocked");
    push_flag(&mut argv, args, "unblocked", "--unblocked");
    push_opt(&mut argv, args, "json", "--json");
    push_opt(&mut argv, args, "story_type", "--type");
    push_opt(&mut argv, args, "description", "--description");
    Ok(argv)
}

fn build_context(args: &Map<String, Value>) -> Result<Vec<String>, String> {
    let mut argv = vec!["load-context".to_string()];
    push_opt(&mut argv, args, "format", "--format");
    Ok(argv)
}

const LIST_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "state",
        kind: FieldKind::Str,
        required: false,
        description: "Filter by state slug.",
    },
    FieldSpec {
        name: "assignee",
        kind: FieldKind::Str,
        required: false,
        description: "Filter by assignee id.",
    },
    FieldSpec {
        name: "flagged",
        kind: FieldKind::Bool,
        required: false,
        description: "Only flagged stories.",
    },
    FieldSpec {
        name: "priority",
        kind: FieldKind::Str,
        required: false,
        description: "Comma-separated priority levels.",
    },
    FieldSpec {
        name: "label",
        kind: FieldKind::Str,
        required: false,
        description: "Filter by label.",
    },
    FieldSpec {
        name: "created_after",
        kind: FieldKind::Str,
        required: false,
        description: "ISO 8601 date/time; only stories created after it.",
    },
    FieldSpec {
        name: "updated_after",
        kind: FieldKind::Str,
        required: false,
        description: "ISO 8601 date/time; only stories updated after it.",
    },
    FieldSpec {
        name: "blocked",
        kind: FieldKind::Bool,
        required: false,
        description: "Only blocked stories.",
    },
    FieldSpec {
        name: "ready",
        kind: FieldKind::Bool,
        required: false,
        description: "Only unblocked, dependency-clear stories.",
    },
    FieldSpec {
        name: "stale",
        kind: FieldKind::Str,
        required: false,
        description: "Only stories not updated within this duration, e.g. \"2h\", \"1d\", \"1w\".",
    },
    FieldSpec {
        name: "phase",
        kind: FieldKind::Str,
        required: false,
        description: "Filter by phase.",
    },
    FieldSpec {
        name: "story_type",
        kind: FieldKind::Str,
        required: false,
        description: "Filter by story type slug.",
    },
    FieldSpec {
        name: "drafts",
        kind: FieldKind::Bool,
        required: false,
        description: "Only draft stories.",
    },
    FieldSpec {
        name: "include_closed",
        kind: FieldKind::Bool,
        required: false,
        description: "Also show closed, unarchived stories (excluded by default).",
    },
    FieldSpec {
        name: "include_archived",
        kind: FieldKind::Bool,
        required: false,
        description: "Also show archived stories (excluded by default; implies include_closed).",
    },
    FieldSpec {
        name: "all",
        kind: FieldKind::Bool,
        required: false,
        description: "Shorthand for include_closed and include_archived together.",
    },
];

const NEXT_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "count",
        kind: FieldKind::Uint,
        required: false,
        description: "How many stories to return. Defaults to 1.",
    },
    FieldSpec {
        name: "phase",
        kind: FieldKind::Str,
        required: false,
        description: "Restrict to this phase.",
    },
];

const SHOW_FIELDS: &[FieldSpec] = &[FieldSpec {
    name: "id",
    kind: FieldKind::Str,
    required: true,
    description: "The story id, e.g. \"SH-42\".",
}];

const SEARCH_FIELDS: &[FieldSpec] = &[FieldSpec {
    name: "query",
    kind: FieldKind::Str,
    required: true,
    description: "Full-text search query.",
}];

const NEW_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "title",
        kind: FieldKind::Str,
        required: true,
        description: "The story's title.",
    },
    FieldSpec {
        name: "state",
        kind: FieldKind::Str,
        required: false,
        description: "Initial state slug. Defaults to the project's initial state.",
    },
    FieldSpec {
        name: "story_type",
        kind: FieldKind::Str,
        required: false,
        description: "Story type slug.",
    },
    FieldSpec {
        name: "description",
        kind: FieldKind::Str,
        required: false,
        description: "Free-text description.",
    },
    FieldSpec {
        name: "priority",
        kind: FieldKind::Str,
        required: false,
        description: "Priority level: critical, high, medium, low, or none.",
    },
    FieldSpec {
        name: "assignee",
        kind: FieldKind::Str,
        required: false,
        description: "Member id to assign.",
    },
    FieldSpec {
        name: "labels",
        kind: FieldKind::StrArray,
        required: false,
        description: "Labels to attach.",
    },
    FieldSpec {
        name: "draft",
        kind: FieldKind::Bool,
        required: false,
        description: "Create as a draft rather than a live story.",
    },
];

const MOVE_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "id",
        kind: FieldKind::Str,
        required: true,
        description: "The story id.",
    },
    FieldSpec {
        name: "state",
        kind: FieldKind::Str,
        required: true,
        description: "The target state slug.",
    },
    FieldSpec {
        name: "if_state",
        kind: FieldKind::Str,
        required: false,
        description: "Only move if the story is currently in this state; otherwise refuse \
                       (optimistic concurrency).",
    },
    FieldSpec {
        name: "reason",
        kind: FieldKind::Str,
        required: false,
        description: "An `awaiting` reason to set atomically with the move, e.g. when moving \
                       to a blocked state.",
    },
    FieldSpec {
        name: "comment",
        kind: FieldKind::Str,
        required: false,
        description: "A comment to attach to the transition.",
    },
];

const COMMENT_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "id",
        kind: FieldKind::Str,
        required: true,
        description: "The story id.",
    },
    FieldSpec {
        name: "text",
        kind: FieldKind::Str,
        required: true,
        description: "The comment text.",
    },
];

const ASSIGN_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "id",
        kind: FieldKind::Str,
        required: true,
        description: "The story id.",
    },
    FieldSpec {
        name: "member",
        kind: FieldKind::Str,
        required: true,
        description: "The member id to assign.",
    },
];

const PRIORITIZE_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "id",
        kind: FieldKind::Str,
        required: true,
        description: "The story id.",
    },
    FieldSpec {
        name: "priority",
        kind: FieldKind::Str,
        required: true,
        description: "Priority level: critical, high, medium, low, or none.",
    },
];

const LABEL_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "id",
        kind: FieldKind::Str,
        required: true,
        description: "The story id.",
    },
    FieldSpec {
        name: "labels",
        kind: FieldKind::StrArray,
        required: true,
        description: "Labels to add.",
    },
];

const RELATE_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "a",
        kind: FieldKind::Str,
        required: true,
        description: "The first story id.",
    },
    FieldSpec {
        name: "relation",
        kind: FieldKind::Str,
        required: true,
        description: "The relationship type, e.g. \"blocks\", \"parent-of\", \"relates-to\".",
    },
    FieldSpec {
        name: "b",
        kind: FieldKind::Str,
        required: true,
        description: "The second story id.",
    },
];

const BLOCK_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "id",
        kind: FieldKind::Str,
        required: true,
        description: "The story id.",
    },
    FieldSpec {
        name: "reason",
        kind: FieldKind::Str,
        required: true,
        description: "Why the story is blocked.",
    },
];

const UNBLOCK_FIELDS: &[FieldSpec] = &[FieldSpec {
    name: "id",
    kind: FieldKind::Str,
    required: true,
    description: "The story id.",
}];

const SET_FIELDS: &[FieldSpec] = &[
    FieldSpec {
        name: "id",
        kind: FieldKind::Str,
        required: true,
        description: "The story id.",
    },
    FieldSpec {
        name: "title",
        kind: FieldKind::Str,
        required: false,
        description: "New title.",
    },
    FieldSpec {
        name: "state",
        kind: FieldKind::Str,
        required: false,
        description: "New state slug.",
    },
    FieldSpec {
        name: "priority",
        kind: FieldKind::Str,
        required: false,
        description: "New priority level.",
    },
    FieldSpec {
        name: "assignee",
        kind: FieldKind::Str,
        required: false,
        description: "New assignee member id.",
    },
    FieldSpec {
        name: "labels",
        kind: FieldKind::Str,
        required: false,
        description: "Comma-separated labels, replacing the story's current set.",
    },
    FieldSpec {
        name: "blocked",
        kind: FieldKind::Str,
        required: false,
        description: "An `awaiting` reason to set.",
    },
    FieldSpec {
        name: "unblocked",
        kind: FieldKind::Bool,
        required: false,
        description: "Clear the `awaiting` reason.",
    },
    FieldSpec {
        name: "json",
        kind: FieldKind::Str,
        required: false,
        description: "A raw JSON object of additional fields to merge.",
    },
    FieldSpec {
        name: "story_type",
        kind: FieldKind::Str,
        required: false,
        description: "New story type slug.",
    },
    FieldSpec {
        name: "description",
        kind: FieldKind::Str,
        required: false,
        description: "New description.",
    },
];

const CONTEXT_FIELDS: &[FieldSpec] = &[FieldSpec {
    name: "format",
    kind: FieldKind::Str,
    required: false,
    description: "\"markdown\" or \"json\". Defaults to markdown.",
}];

/// The curated tool surface — the whole story, in one table.
pub const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "story_list",
        description: "List open stories with optional filters. For the single best story to \
                      work on next, use story_next instead.",
        fields: LIST_FIELDS,
        build_argv: build_list,
    },
    ToolDef {
        name: "story_next",
        description: "The highest-priority ready story or stories.",
        fields: NEXT_FIELDS,
        build_argv: build_next,
    },
    ToolDef {
        name: "story_show",
        description: "Full details for a single story, including its comments.",
        fields: SHOW_FIELDS,
        build_argv: build_show,
    },
    ToolDef {
        name: "story_search",
        description: "Full-text search across all stories.",
        fields: SEARCH_FIELDS,
        build_argv: build_search,
    },
    ToolDef {
        name: "story_summary",
        description: "Counts of stories by state and priority.",
        fields: &[],
        build_argv: build_summary,
    },
    ToolDef {
        name: "story_new",
        description: "Create a new story.",
        fields: NEW_FIELDS,
        build_argv: build_new,
    },
    ToolDef {
        name: "story_move",
        description: "Transition a story to a new state, e.g. todo to in-progress to done.",
        fields: MOVE_FIELDS,
        build_argv: build_move,
    },
    ToolDef {
        name: "story_comment",
        description: "Add a timestamped comment to a story.",
        fields: COMMENT_FIELDS,
        build_argv: build_comment,
    },
    ToolDef {
        name: "story_assign",
        description: "Assign a story to a team member.",
        fields: ASSIGN_FIELDS,
        build_argv: build_assign,
    },
    ToolDef {
        name: "story_prioritize",
        description: "Set a story's priority.",
        fields: PRIORITIZE_FIELDS,
        build_argv: build_prioritize,
    },
    ToolDef {
        name: "story_label",
        description: "Add labels to a story.",
        fields: LABEL_FIELDS,
        build_argv: build_label,
    },
    ToolDef {
        name: "story_relate",
        description: "Add a relationship between two stories, e.g. blocks, parent-of, \
                      relates-to.",
        fields: RELATE_FIELDS,
        build_argv: build_relate,
    },
    ToolDef {
        name: "story_block",
        description: "Mark a story as blocked, with a reason.",
        fields: BLOCK_FIELDS,
        build_argv: build_block,
    },
    ToolDef {
        name: "story_unblock",
        description: "Clear a story's blocked status.",
        fields: UNBLOCK_FIELDS,
        build_argv: build_unblock,
    },
    ToolDef {
        name: "story_set",
        description: "Update multiple fields on a story at once.",
        fields: SET_FIELDS,
        build_argv: build_set,
    },
    ToolDef {
        name: "story_context",
        description: "A session-start context document: project summary, priorities, and \
                      blockers.",
        fields: CONTEXT_FIELDS,
        build_argv: build_context,
    },
];

/// `tools/list`'s answer: one entry per [`TOOLS`], each schema built by the
/// single canonical [`json_schema`].
pub fn list() -> Value {
    let tools: Vec<Value> = TOOLS
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": json_schema(tool.fields),
            })
        })
        .collect();
    json!({ "tools": tools })
}

/// Which curated tool, if any, an [`Invocation`] variant maps to.
///
/// Exhaustive over every variant, with no wildcard arm: a 65th variant is a
/// compile error here until this match says whether it gets a tool. This is
/// the anti-drift mechanism's compile-time half. It also runs in production,
/// not only in tests — [`super::server::call_tool`] uses it as a cheap
/// sanity check that the `Invocation` a tool actually built is the one its
/// own table says it targets.
#[must_use]
pub fn tool_for_variant(invocation: &Invocation) -> Option<&'static str> {
    match invocation {
        Invocation::List { .. } => Some("story_list"),
        Invocation::Next { .. } => Some("story_next"),
        Invocation::Show { .. } => Some("story_show"),
        Invocation::Search { .. } => Some("story_search"),
        Invocation::Summary => Some("story_summary"),
        Invocation::New { .. } => Some("story_new"),
        Invocation::SetState { .. } => Some("story_move"),
        Invocation::Comment { .. } => Some("story_comment"),
        Invocation::Assign { .. } => Some("story_assign"),
        Invocation::SetPriority { .. } => Some("story_prioritize"),
        Invocation::SetLabels { .. } => Some("story_label"),
        Invocation::Relate { .. } => Some("story_relate"),
        Invocation::SetAwaiting { .. } => Some("story_block"),
        Invocation::ClearAwaiting { .. } => Some("story_unblock"),
        Invocation::SetFields { .. } => Some("story_set"),
        Invocation::Context { .. } => Some("story_context"),

        Invocation::Help
        | Invocation::Project { .. }
        | Invocation::Publish { .. }
        | Invocation::MemberAdd { .. }
        | Invocation::State { .. }
        | Invocation::Report { .. }
        | Invocation::Doctor { .. }
        | Invocation::DoctorAbandoned { .. }
        | Invocation::DoctorCrashes { .. }
        | Invocation::Log { .. }
        | Invocation::Reopen { .. }
        | Invocation::Hide { .. }
        | Invocation::Unhide { .. }
        | Invocation::HideState { .. }
        | Invocation::Delete { .. }
        | Invocation::Purge { .. }
        | Invocation::BulkUpdate { .. }
        | Invocation::Import { .. }
        | Invocation::Decompose { .. }
        | Invocation::Export
        | Invocation::ImportProject { .. }
        | Invocation::Migrate { .. }
        | Invocation::Handoff { .. }
        | Invocation::Phase { .. }
        | Invocation::Type { .. }
        | Invocation::Epic { .. }
        | Invocation::Graph { .. }
        | Invocation::Hooks { .. }
        | Invocation::Scaffold { .. }
        | Invocation::CommitSync { .. }
        | Invocation::GithubSync { .. }
        | Invocation::LinkPr { .. }
        | Invocation::UnlinkPr { .. }
        | Invocation::PrCheck { .. }
        | Invocation::GithubAuth { .. }
        | Invocation::HelpTopic { .. }
        | Invocation::HelpCompact
        | Invocation::HelpAll
        | Invocation::Plugin { .. }
        | Invocation::Web { .. }
        | Invocation::Token { .. }
        | Invocation::Daemon { .. }
        | Invocation::Store { .. }
        | Invocation::SessionStart
        | Invocation::Update { .. }
        | Invocation::Version
        | Invocation::ProjectSnapshot
        | Invocation::History { .. }
        | Invocation::Attachment { .. } => None,
    }
}

/// What building a tool call's `Invocation` failed at.
pub enum BuildError {
    /// The tool name in `tools/call` is not one this server declares.
    UnknownTool,
    /// The arguments could not become a valid command — a missing required
    /// field, or (rarely) the same shape of mistake a typed command would
    /// reject, such as free text that happens to look like an unknown flag.
    BadArguments(String),
}

/// Looks up `tool_name` in [`TOOLS`] and turns `arguments` into an
/// [`Invocation`], entirely through [`cli::parse_invocation`] — the same
/// function `main` calls for a typed command.
pub fn build_invocation(
    tool_name: &str,
    arguments: &Map<String, Value>,
) -> Result<Invocation, BuildError> {
    let tool = TOOLS
        .iter()
        .find(|tool| tool.name == tool_name)
        .ok_or(BuildError::UnknownTool)?;
    for field in COMMON_FIELDS.iter().filter(|f| f.required) {
        if str_arg(arguments, field.name).is_none() {
            return Err(BuildError::BadArguments(format!(
                "`{}` is required",
                field.name
            )));
        }
    }
    let argv = (tool.build_argv)(arguments).map_err(BuildError::BadArguments)?;
    cli::parse_invocation(&argv).map_err(|e| BuildError::BadArguments(e.to_string()))
}
