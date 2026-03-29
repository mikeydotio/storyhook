use std::collections::BTreeMap;

/// Extended help topics for `story help <command>`.
/// Each topic provides agent-friendly guidance: when to use, examples, and related commands.
pub fn get_help_topic(topic: &str) -> Option<&'static str> {
    TOPICS.get(topic).copied()
}

pub fn list_topics() -> Vec<&'static str> {
    TOPICS.keys().copied().collect()
}

static TOPICS: std::sync::LazyLock<BTreeMap<&'static str, &'static str>> =
    std::sync::LazyLock::new(|| {
        let mut m = BTreeMap::new();

        m.insert(
            "init",
            r#"story init [--prefix <PREFIX>]

Initialize a storyhook project in the current directory. Creates the
.storyhook/ directory with default states (todo, done), an empty members
file, and a CLAUDE.md with workflow instructions.

When to use:
  At the start of a new project, or when you enter a repo that doesn't
  have .storyhook/ yet. Only needs to be run once per project.

Examples:
  story init                  # Default prefix "SH" → stories are SH-1, SH-2, ...
  story init --prefix API     # Custom prefix → API-1, API-2, ...

Related:
  story member add  — Add team members after init
  story state add   — Add custom states (e.g., in-progress, review)
  story new         — Create your first story
"#,
        );

        m.insert(
            "new",
            r#"story new <title>

Create a new story with the given title. Returns the assigned ID.

When to use:
  When you have a discrete piece of work to track. For bulk creation
  from a spec, use 'story decompose' instead.

Examples:
  story new "Implement authentication middleware"
  story new "Fix: login page returns 500 on empty password"
  story new "Refactor database connection pooling" --json

Related:
  story decompose  — Create multiple stories from a spec file
  story <id> priority  — Set priority after creation
  story <id> label     — Add labels after creation
"#,
        );

        m.insert(
            "list",
            r#"story list [filters]

List open stories with optional filters. Returns all open stories by
default. Combine multiple filters to narrow results.

When to use:
  When you need to browse or filter the backlog. For the single best
  task to work on next, use 'story next' instead.

Filters:
  --state <slug>          Filter by state (e.g., todo, in-progress)
  --assignee <id>         Filter by assignee member ID or GitHub handle
  --priority <levels>     Comma-separated: critical,high,medium,low,none
  --label <labels>        Comma-separated label filter
  --flagged               Only stories with integrity warnings
  --blocked               Only stories blocked by dependencies or awaiting
  --ready                 Only stories ready to work on (unblocked)
  --stale <duration>      Stories not updated in duration (e.g., 2d, 1w)
  --created-after <date>  Stories created after ISO date
  --updated-after <date>  Stories updated after ISO date

Examples:
  story list                          # All open stories
  story list --ready                  # Unblocked, ready to work on
  story list --priority critical,high # Only critical and high priority
  story list --blocked --json         # Blocked stories as JSON
  story list --stale 3d               # Not updated in 3 days

Related:
  story next     — Get the highest-priority ready story
  story summary  — Aggregate counts by state and priority
  story search   — Full-text search across stories
"#,
        );

        m.insert(
            "next",
            r#"story next [--count <n>]

Get the highest-priority ready stories. A story is "ready" when all its
predecessors are closed and it has no awaiting blockers.

When to use:
  At session start to pick your first task, or after completing a story
  to find the next one. Prefer this over 'story list' when you need a
  single actionable item.

Examples:
  story next                # Top-priority ready story
  story next --count 3      # Top 3 ready stories
  story next --json         # Structured JSON output

Related:
  story context  — Full project overview (use first in a new session)
  story list     — All stories with filters (use for exploration)
  story graph    — Dependency visualization (use to understand blockers)
"#,
        );

        m.insert(
            "summary",
            r#"story summary

Show project summary with story counts by state and priority, plus
lists of ready, blocked, and stale stories.

When to use:
  For a quick overview of project health. Use 'story context' for a
  more comprehensive session-start document.

Examples:
  story summary             # Human-readable summary
  story summary --json      # Structured JSON output

Related:
  story context  — Full project context document
  story list     — Detailed filtered listing
  story report   — HTML or text report
"#,
        );

        m.insert(
            "context",
            r#"story context [--format markdown|json]

Generate a comprehensive project context document suitable for AI agent
session initialization. Includes project state, open stories, blocked
items, and ready tasks.

When to use:
  At the start of every session. This is the primary command for
  understanding what's happening in the project.

Examples:
  story context                       # Markdown format (default)
  story context --format json         # JSON format
  story context --format markdown     # Explicit markdown

Related:
  story next     — Pick the next task to work on
  story handoff  — Generate end-of-session summary
  story summary  — Quick state/priority counts
"#,
        );

        m.insert(
            "handoff",
            r#"story handoff [--since <duration>]

Generate a session handoff document summarizing what changed in the
given time window. Includes stories created, updated, and completed.

When to use:
  At the end of a work session, or when switching contexts between
  different projects or tasks.

Examples:
  story handoff                   # Default: last 2 hours
  story handoff --since 4h        # Last 4 hours
  story handoff --since 1d        # Last day

Related:
  story context   — Session start (pair with handoff at session end)
  story sync-git  — Link git commits to stories before handoff
"#,
        );

        m.insert(
            "search",
            r#"story search <query>

Full-text search across story titles, comments, and labels. Searches
both open and archived stories.

When to use:
  When you know part of a story's title or content but not its ID.
  For filtered browsing, use 'story list' instead.

Examples:
  story search "authentication"
  story search "bug fix login"
  story search "database" --json

Related:
  story list  — Filter by state, priority, label, etc.
  story <id>  — Show a specific story by ID
"#,
        );

        m.insert(
            "decompose",
            r#"story decompose <file> [--dry-run]
story decompose --stdin [--dry-run]

Parse a markdown or YAML specification into individual stories with
relationships, priorities, and labels. Supports both file input and
stdin.

When to use:
  When you have a spec, PRD, or feature description that needs to be
  broken into trackable work items. Use --dry-run first to preview.

Examples:
  story decompose spec.md --dry-run     # Preview without creating
  story decompose spec.md               # Create stories from spec
  story decompose spec.yaml             # YAML format supported
  cat spec.md | story decompose --stdin # Read from stdin

Related:
  story new        — Create a single story manually
  story graph      — Visualize dependencies after decomposition
  story import     — Bulk import from JSON
"#,
        );

        m.insert(
            "graph",
            r#"story graph [--critical-path] [--blocked-by <id>] [--parallel-groups]

Analyze the dependency graph of open stories. Shows relationships,
bottlenecks, and parallelizable work.

When to use:
  After creating stories with dependencies, to understand execution
  order, identify bottlenecks, or find parallelizable work.

Modes:
  (default)           Overview of all relationships
  --critical-path     Longest dependency chain (highest impact path)
  --blocked-by <id>   What is blocking this specific story
  --parallel-groups   Stories that can be worked on simultaneously

Examples:
  story graph                         # Full overview
  story graph --critical-path         # Identify the critical path
  story graph --blocked-by SH-5      # Why is SH-5 blocked?
  story graph --parallel-groups --json # Parallelizable work as JSON

Related:
  story list --blocked  — Simple list of blocked stories
  story list --ready    — Stories ready to work on now
  story next            — Highest-priority ready story
"#,
        );

        m.insert(
            "doctor",
            r#"story doctor [--fix]

Run integrity checks on the storyhook project data. Detects orphaned
relationships, invalid states, and data inconsistencies.

When to use:
  When stories seem inconsistent, after manual edits to .storyhook/
  files, or periodically as a health check. Use --fix to auto-repair.

Examples:
  story doctor          # Check only, report issues
  story doctor --fix    # Attempt to fix issues found

Related:
  story list --flagged  — Stories with integrity warnings
  story summary         — Project state overview
"#,
        );

        m.insert(
            "report",
            r#"story report [--html]

Generate a project report. Default is text format; --html produces a
standalone HTML document.

When to use:
  When you need a shareable project status document. The HTML report
  is suitable for stakeholder updates.

Examples:
  story report              # Text report to stdout
  story report --html       # HTML report to stdout
  story report --html > status.html  # Save to file

Related:
  story summary  — Quick summary counts
  story context  — Agent-friendly context document
"#,
        );

        m.insert(
            "sync-git",
            r#"story sync-git [--since <duration>]

Scan recent git commits for story ID references and add comments to
those stories. Also auto-transitions stories based on commit patterns
(e.g., "closes SH-1").

When to use:
  After git operations (commit, merge, pull) to link code changes to
  stories. Safe to run repeatedly — skips already-synced commits.

Examples:
  story sync-git                  # Default: last 7 days
  story sync-git --since 1h      # Last hour only
  story sync-git --since 2w      # Last 2 weeks

Related:
  story hooks install  — Auto-sync via git hooks
  story handoff        — End-of-session summary
"#,
        );

        m.insert(
            "scaffold",
            r#"story scaffold agents-md|claude-md|cursor-rules

Generate agent instruction files for different AI coding tools. These
files teach agents how to use storyhook in the project.

When to use:
  After 'story init' to set up AI agent integration. Run the scaffold
  matching your agent tool.

Variants:
  agents-md      Universal AGENTS.md (works with most AI agents)
  claude-md      Claude Code-specific instructions
  cursor-rules   Cursor editor rules

Examples:
  story scaffold agents-md        # Generate AGENTS.md
  story scaffold claude-md        # Generate Claude Code snippet
  story scaffold cursor-rules     # Generate .cursorrules content

Related:
  story init    — Initialize project (creates .storyhook/CLAUDE.md)
  story context — Project state for session start
"#,
        );

        m.insert(
            "hooks",
            r#"story hooks install|uninstall|list|test <event_type>

Manage storyhook event hooks. Event hooks fire custom commands when
story events occur (create, state change, close, etc.).

When to use:
  'install' / 'uninstall': Set up or remove git hooks for auto-sync.
  'list': See configured event hooks.
  'test': Fire a test event to verify hook configuration.

Examples:
  story hooks install             # Install git hooks (post-commit, etc.)
  story hooks uninstall           # Remove git hooks
  story hooks list                # Show event hook configuration
  story hooks test on_create      # Test the on_create hook

Related:
  story sync-git     — Manual git sync (hooks automate this)
  .storyhook/hooks.toml — Event hook configuration file
"#,
        );

        m.insert(
            "mcp-config",
            r#"story mcp-config [options]

Configure MCP (Model Context Protocol) server integration for AI coding
tools. Supports Claude Code, Cursor, Codex CLI, and Antigravity.

When to use:
  To set up the storyhook MCP server in your AI tool. Run without
  flags for an interactive setup wizard.

Options:
  (no flags)              Interactive multi-provider setup
  --install <provider>    Install for: claude, cursor, codex, antigravity
  --uninstall <provider>  Remove configuration
  --uninstall-all         Remove from all providers
  --scope project         Project-level config (vs user-level)

Examples:
  story mcp-config                    # Interactive setup
  story mcp-config --install claude   # Install for Claude Code
  story mcp-config --scope project    # Project-level config

Related:
  story --mcp     — Start MCP server directly (stdio)
  story scaffold  — Alternative: instruction files instead of MCP
"#,
        );

        m.insert(
            "tui",
            r#"story tui

Launch an interactive terminal user interface for managing stories. The
TUI provides a dashboard, a Kanban-style board view, story detail editing,
filtering, and story creation — all without leaving the terminal.

When to use:
  When you want a visual overview of project state, or prefer to browse,
  edit, and move stories interactively rather than via individual CLI
  commands.

Views:
  Dashboard (1)   Project summary with metrics and recent activity
  Board (2)       Stories grouped by state as a scrollable table

Key bindings (board view):
  j/k, Up/Down    Navigate between rows
  h/l             Jump to previous/next section header
  Enter           Open story detail modal
  > / <           Move story to next/previous state
  n               Create a new story
  /               Focus the filter bar
  Space           Toggle section collapsed/expanded
  ?               Show full keybinding reference
  q               Quit

Filtering:
  Press / to focus the filter bar, then type a filter query:
    state:todo        Filter by state
    priority:high     Filter by priority
    assignee:mikey    Filter by assignee
    label:bug         Filter by label
    blocked           Only blocked stories
    ready             Only unblocked stories
    <free text>       Substring match on title or ID

Mouse support:
  Click on a story row to select it. Double-click to open the detail
  modal. Scroll wheel navigates the board. Click outside a modal to
  close it.

Examples:
  story tui                     # Launch the TUI in the current project

Related:
  story list     — CLI-based story listing with filters
  story summary  — Project summary (similar to TUI dashboard)
  story context  — Full project context for AI agents
"#,
        );

        m.insert(
            "import",
            r#"story import [<file>]

Import stories from a JSON file or stdin. Expects an array of story
objects with at minimum a "title" field.

When to use:
  For bulk story creation from structured data. For markdown/YAML
  specs, use 'story decompose' instead.

Examples:
  story import stories.json           # From file
  cat stories.json | story import     # From stdin

Related:
  story export     — Export all stories to JSON
  story decompose  — Create stories from markdown/YAML spec
  story new        — Create a single story
"#,
        );

        m.insert(
            "export",
            r#"story export

Export all open stories as a JSON array. Useful for backup, migration,
or external processing.

When to use:
  For data backup, migration to another system, or feeding into
  external tools.

Examples:
  story export                        # JSON to stdout
  story export > backup.json          # Save to file

Related:
  story import  — Import stories from JSON
  story report  — Human-readable report
"#,
        );

        m.insert(
            "json-format",
            r#"JSON Output Format Reference

All storyhook CLI commands support structured JSON output via the global
--json flag. This document describes the envelope format, per-command
response shapes, error format, and exit codes.

== Global --json flag ==

Pass --json as a global option (before or after the subcommand) to get
machine-readable JSON instead of human-readable text:

  story list --json
  story --json SH-1
  story summary --json

The flag applies to every command. When --json is active, all output
(success and error) is valid JSON printed to stdout.

== JSON Envelope ==

Every successful response is wrapped in a standard envelope:

  {
    "result": "ok",
    <data fields depending on command>,
    "warnings": ["..."],          // omitted when empty
    "flagged_reasons": ["..."]    // omitted when empty
  }

The "result" field is always "ok" for success. Data fields vary by
command (see below). The "warnings" and "flagged_reasons" arrays are
present only when non-empty.

== Per-Command Response Shapes ==

Commands returning a single story ("story" field):
  story new <title>           -> "story": StoryView
  story <id>                  -> "story": StoryView
  story <id> "<comment>"      -> "story": StoryView
  story <id> assign <member>  -> "story": StoryView
  story <id> is <state>       -> "story": StoryView
  story <id> awaits "<reason>" -> "story": StoryView
  story <id> awaits --clear   -> "story": StoryView
  story <id> priority <level> -> "story": StoryView
  story <id> label <csv>      -> "story": StoryView
  story <id> reopen           -> "story": StoryView
  story <a> <rel> <b>         -> "story": StoryView (of story a)
  story next                  -> "story": StoryView (single result)

  StoryView object:
    {
      "story": {
        "id": "SH-1",
        "title": "Implement auth",
        "state": "todo",
        "superstate": "open",
        "priority": "high",
        "assignee": "alice",
        "labels": ["backend"],
        "awaiting": null,
        "relationships": [
          {"relation": "follows", "other_id": "SH-2"}
        ],
        "comments": [
          {"at": "2025-01-15T10:00:00Z", "text": "Started work"}
        ],
        "created_at": "2025-01-15T09:00:00Z",
        "updated_at": "2025-01-15T10:00:00Z",
        "closed_at": null
      },
      "derived_relationships": [],
      "warnings": [],
      "flagged_reasons": [],
      "stale_info": null
    }

Commands returning a story list ("stories" field):
  story list [filters]        -> "stories": [StoryView, ...]
  story search <query>        -> "stories": [StoryView, ...]
  story next --count <n>      -> "stories": [StoryView, ...]
  story import [file]          -> "stories": [StoryView, ...]
  story decompose <file>       -> "stories": [StoryView, ...]

Commands returning a summary ("summary" field):
  story summary               -> "summary": SummaryView
  story report                -> "summary": SummaryView

  SummaryView object:
    {
      "total_open": 5,
      "total_closed": 3,
      "by_state": [["todo", 3], ["in-progress", 2]],
      "by_priority": [["high", 2], ["medium", 1]],
      "blocked_count": 1,
      "flagged_count": 0,
      "ready_count": 4,
      "ready_stories": [StoryView, ...]
    }

Commands returning a graph ("graph" field):
  story graph                 -> "graph": GraphView
  story graph --critical-path -> "graph": GraphView
  story graph --blocked-by <id> -> "graph": GraphView
  story graph --parallel-groups -> "graph": GraphView

  GraphView object:
    {
      "critical_path": ["SH-1", "SH-3", "SH-5"],
      "blocked_chain": {"source": "SH-2", "blocked": ["SH-4"]},
      "parallel_groups": [["SH-1", "SH-2"], ["SH-3"]],
      "overview": {
        "total_open": 5,
        "total_edges": 3,
        "roots": ["SH-1"],
        "leaves": ["SH-5"]
      }
    }

  Fields are present only for the requested mode. For example,
  --critical-path only populates "critical_path"; other fields are null.

Commands returning issues ("issues" field):
  story doctor                -> "issues": ["issue description", ...]

Commands returning a message ("message" field):
  story init                  -> "message": "initialized story project..."
  story member add            -> "message": "added member alice"
  story state add/remove      -> "message": "added state in-progress (open)"
  story export                -> "message": "<json array of stories>"
  story context               -> "message": "<markdown or json string>"
  story handoff               -> "message": "<markdown string>"
  story report --html         -> "message": "<html string>"
  story scaffold              -> "message": "<template content>"
  story hooks install/...     -> "message": "<status text>"
  story sync-git              -> "message": "scanned N commits..."
  story mcp-config            -> "message": "<config json or instructions>"
  story next (no results)     -> "message": "no ready stories"
  story help <topic>          -> "message": "<help text>"

== Error Format ==

Errors produce:

  {
    "result": "error",
    "error": "story `SH-99` not found",
    "exit_code": 3
  }

== Exit Codes ==

  0  Success
  2  Usage error or validation error (bad arguments, invalid input)
  3  Not found (story ID does not exist)
  4  Lock timeout (another process holds the project lock)
  5  Integrity or storage error (corrupt data, I/O failure)

== MCP (Model Context Protocol) ==

The MCP server (story --mcp) wraps the same JSON envelope inside
standard JSON-RPC 2.0 responses. Each tool call returns:

  {
    "jsonrpc": "2.0",
    "id": <request-id>,
    "result": {
      "content": [{
        "type": "text",
        "text": "<storyhook JSON envelope as a string>"
      }]
    }
  }

The "text" field contains the serialized storyhook JSON envelope
(the same format described above). Parse the text value as JSON to
access the structured data.

== Examples ==

Show a story:

  $ story SH-1 --json
  {
    "result": "ok",
    "story": {
      "story": {
        "id": "SH-1",
        "title": "Add login page",
        "state": "todo",
        "superstate": "open",
        "priority": "high",
        "assignee": null,
        "labels": [],
        "awaiting": null,
        "relationships": [],
        "comments": [],
        "created_at": "2025-01-15T09:00:00Z",
        "updated_at": "2025-01-15T09:00:00Z",
        "closed_at": null
      },
      "derived_relationships": [],
      "warnings": [],
      "flagged_reasons": []
    }
  }

List stories:

  $ story list --ready --json
  {
    "result": "ok",
    "stories": [
      {
        "story": {
          "id": "SH-1",
          "title": "Add login page",
          "state": "todo",
          "superstate": "open",
          "priority": "high",
          "assignee": null,
          "labels": [],
          ...
        },
        ...
      }
    ]
  }

Error:

  $ story SH-999 --json
  {
    "result": "error",
    "error": "story `SH-999` not found",
    "exit_code": 3
  }
"#,
        );

        m
    });

#[cfg(test)]
mod tests {
    use super::get_help_topic;

    #[test]
    fn json_format_topic_exists_and_covers_key_concepts() {
        let content = get_help_topic("json-format").expect("json-format topic should exist");
        assert!(content.contains("\"result\""), "should document result field");
        assert!(content.contains("--json"), "should document --json flag");
        assert!(content.contains("\"story\""), "should document story field");
        assert!(content.contains("\"stories\""), "should document stories field");
        assert!(content.contains("\"summary\""), "should document summary field");
        assert!(content.contains("\"graph\""), "should document graph field");
        assert!(content.contains("\"issues\""), "should document issues field");
        assert!(content.contains("\"message\""), "should document message field");
        assert!(content.contains("exit_code"), "should document exit codes");
        assert!(content.contains("JSON-RPC"), "should document MCP format");
    }

    #[test]
    fn json_format_topic_listed() {
        let topics = super::list_topics();
        assert!(topics.contains(&"json-format"), "json-format should appear in topic list");
    }
}
