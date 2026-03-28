use std::io::{self, BufRead, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::app;
use crate::cli::{CliOptions, GraphMode, Invocation};
use crate::output;

const SERVER_NAME: &str = "storyhook";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
}

pub fn run_mcp_server(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    for line in stdin.lock().lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(trimmed) {
            Ok(req) => req,
            Err(e) => {
                let response = JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: Value::Null,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32700,
                        message: format!("Parse error: {e}"),
                    }),
                };
                writeln!(stdout, "{}", serde_json::to_string(&response)?)?;
                stdout.flush()?;
                continue;
            }
        };

        if request.jsonrpc != "2.0" {
            let response = JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: request.id.unwrap_or(Value::Null),
                result: None,
                error: Some(JsonRpcError {
                    code: -32600,
                    message: "Invalid JSON-RPC version".to_string(),
                }),
            };
            writeln!(stdout, "{}", serde_json::to_string(&response)?)?;
            stdout.flush()?;
            continue;
        }

        let id = request.id.clone().unwrap_or(Value::Null);

        let response = match request.method.as_str() {
            "initialize" => handle_initialize(id),
            "notifications/initialized" => continue,
            "tools/list" => handle_tools_list(id),
            "tools/call" => handle_tools_call(id, request.params, root),
            "ping" => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(serde_json::json!({})),
                error: None,
            },
            _ => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32601,
                    message: format!("Method not found: {}", request.method),
                }),
            },
        };

        writeln!(stdout, "{}", serde_json::to_string(&response)?)?;
        stdout.flush()?;
    }

    Ok(())
}

fn handle_initialize(id: Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: Some(serde_json::json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": SERVER_NAME,
                "version": SERVER_VERSION
            }
        })),
        error: None,
    }
}

fn handle_tools_list(id: Value) -> JsonRpcResponse {
    let tools = serde_json::json!({
        "tools": [
            {
                "name": "storyhook_list_stories",
                "description": "List open stories with optional filters by state, assignee, priority, label, or status flags. Returns an array of story summaries. Use this for browsing the backlog or finding specific stories. For the single best task to work on next, use storyhook_get_next instead.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "state": {"type": "string", "description": "Filter by state slug"},
                        "assignee": {"type": "string", "description": "Filter by assignee ID"},
                        "priority": {"type": "string", "description": "Comma-separated priority levels"},
                        "label": {"type": "string", "description": "Comma-separated labels"},
                        "flagged": {"type": "boolean", "description": "Only flagged stories"},
                        "blocked": {"type": "boolean", "description": "Only blocked stories"},
                        "ready": {"type": "boolean", "description": "Only ready stories"},
                        "stale": {"type": "string", "description": "Show stories with updated_at older than duration (e.g. 2h, 1d, 1w)"}
                    }
                }
            },
            {
                "name": "storyhook_get_story",
                "description": "Get full details for a single story by its ID, including state, priority, labels, comments, relationships, and timestamps. Use this when you need the complete context for a specific story.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string", "description": "Story ID (e.g., SH-1)"}
                    },
                    "required": ["id"]
                }
            },
            {
                "name": "storyhook_create_story",
                "description": "Create a new story with a title and optional priority, labels, and assignee. Returns the created story with its assigned ID. For bulk creation from a spec, use storyhook_decompose_spec instead.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "title": {"type": "string", "description": "Story title"},
                        "priority": {"type": "string", "description": "Priority: critical, high, medium, low, none"},
                        "labels": {"type": "string", "description": "Comma-separated labels"},
                        "assignee": {"type": "string", "description": "Assignee member ID"}
                    },
                    "required": ["title"]
                }
            },
            {
                "name": "storyhook_update_story",
                "description": "Update a story's state, priority, labels, assignee, or awaiting status. Processes one update field per call in priority order (state > priority > labels > assignee > awaiting). Transitioning to a CLOSED state automatically archives the story.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string", "description": "Story ID"},
                        "state": {"type": "string", "description": "New state slug"},
                        "priority": {"type": "string", "description": "New priority level"},
                        "labels_add": {"type": "string", "description": "Labels to add (comma-separated)"},
                        "labels_remove": {"type": "string", "description": "Labels to remove (comma-separated)"},
                        "assignee": {"type": "string", "description": "Assignee member ID"},
                        "awaiting": {"type": "string", "description": "Set awaiting reason (use empty string to clear)"}
                    },
                    "required": ["id"]
                }
            },
            {
                "name": "storyhook_add_comment",
                "description": "Add a timestamped comment to a story. Use for progress notes, decisions, or context that should be preserved in the story's event log. Comments are append-only and form part of the audit trail.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string", "description": "Story ID"},
                        "text": {"type": "string", "description": "Comment text"}
                    },
                    "required": ["id", "text"]
                }
            },
            {
                "name": "storyhook_get_summary",
                "description": "Get a project summary with story counts by state and priority, plus lists of ready, blocked, and stale stories. Use this for a quick project health check. For a full session-start context document, use the CLI command 'story context' instead.",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "storyhook_get_next",
                "description": "Get the highest-priority ready stories to work on. A story is 'ready' when all its predecessors are closed and it has no awaiting blockers. Returns stories sorted by priority then creation time. Use this to pick the next task at session start or after completing work.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "count": {"type": "integer", "description": "Number of stories to return (default: 1)"}
                    }
                }
            },
            {
                "name": "storyhook_search",
                "description": "Full-text search across story titles, comments, and labels. Searches both open and archived stories. Use when you know part of a story's content but not its ID. For filtered browsing by state or priority, use storyhook_list_stories instead.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Search query"}
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "storyhook_get_graph",
                "description": "Analyze the dependency graph of open stories. Shows relationships, bottlenecks, and parallelizable work. Use 'critical_path' to find the longest dependency chain, 'parallel_groups' to find independent work streams, or 'blocked_by' to trace why a specific story is blocked.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "mode": {
                            "type": "string",
                            "enum": ["overview", "critical_path", "parallel_groups"],
                            "description": "Graph analysis mode"
                        },
                        "blocked_by": {"type": "string", "description": "Story ID for blocked-by analysis"}
                    }
                }
            },
            {
                "name": "storyhook_scaffold",
                "description": "Generate agent instruction files that teach AI tools how to use storyhook. Creates AGENTS.md (universal), CLAUDE.md (Claude Code), or .cursorrules (Cursor). Use after project init to set up AI agent integration.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "kind": {
                            "type": "string",
                            "enum": ["agents-md", "claude-md", "cursor-rules"],
                            "description": "Template kind: agents-md, claude-md, or cursor-rules"
                        }
                    },
                    "required": ["kind"]
                }
            },
            {
                "name": "storyhook_decompose_spec",
                "description": "Parse a Markdown or YAML specification into individual stories with relationships, priorities, and labels. Use --dry-run to preview without creating. For creating single stories, use storyhook_create_story instead.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "content": {"type": "string", "description": "Markdown content to decompose"},
                        "dry_run": {"type": "boolean", "description": "If true, return JSON without creating stories"}
                    },
                    "required": ["content"]
                }
            },
            {
                "name": "storyhook_bulk_create",
                "description": "Create multiple stories at once from a JSON array. Each story can have a title, priority, labels, and relationships (by ref_index to other stories in the same batch or by other_id for existing stories). Use for programmatic batch creation.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "stories": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "title": {"type": "string"},
                                    "priority": {"type": "string"},
                                    "labels": {"type": "array", "items": {"type": "string"}},
                                    "relationships": {
                                        "type": "array",
                                        "items": {
                                            "type": "object",
                                            "properties": {
                                                "relation": {"type": "string"},
                                                "ref_index": {"type": "integer"},
                                                "other_id": {"type": "string"}
                                            },
                                            "required": ["relation"]
                                        }
                                    }
                                },
                                "required": ["title"]
                            },
                            "description": "Array of stories to create"
                        }
                    },
                    "required": ["stories"]
                }
            },
            {
                "name": "storyhook_generate_report",
                "description": "Generate a project status report in text or standalone HTML format. The HTML report is suitable for stakeholder updates. For a quick summary, use storyhook_get_summary instead.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "format": {
                            "type": "string",
                            "enum": ["html", "text"],
                            "description": "Output format: html or text (default: text)"
                        }
                    }
                }
            },
            {
                "name": "storyhook_sync_git",
                "description": "Scan recent git commits for story ID references and link them to stories via comments. Also auto-transitions stories based on commit patterns (e.g., 'closes SH-1'). Safe to run repeatedly — skips already-synced commits. Use after git operations to keep stories up to date.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "since": {
                            "type": "string",
                            "description": "Duration to look back (e.g. 7d, 24h, 2w). Default: 7d"
                        }
                    }
                }
            }
        ]
    });

    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: Some(tools),
        error: None,
    }
}

fn handle_tools_call(id: Value, params: Option<Value>, root: &Path) -> JsonRpcResponse {
    let params = match params {
        Some(p) => p,
        None => {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32602,
                    message: "Missing params".to_string(),
                }),
            };
        }
    };

    let tool_name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    let invocation = match build_invocation(&tool_name, &arguments) {
        Ok(inv) => inv,
        Err(e) => {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(tool_error(&e)),
                error: None,
            };
        }
    };

    let options = CliOptions {
        json: true,
        quiet: false,
        no_hooks: false,
        invocation,
    };

    match app::run(root, options.clone()) {
        Ok(response) => {
            let rendered = output::render_response(&response, true, false);
            let content = rendered.trim().to_string();
            JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(serde_json::json!({
                    "content": [{
                        "type": "text",
                        "text": content
                    }]
                })),
                error: None,
            }
        }
        Err(e) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(tool_error(&e.to_string())),
            error: None,
        },
    }
}

fn tool_error(message: &str) -> Value {
    serde_json::json!({
        "content": [{
            "type": "text",
            "text": message
        }],
        "isError": true
    })
}

fn get_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn get_bool(args: &Value, key: &str) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn build_invocation(tool_name: &str, arguments: &Value) -> Result<Invocation, String> {
    match tool_name {
        "storyhook_list_stories" => Ok(Invocation::List {
            state: get_str(arguments, "state"),
            assignee: get_str(arguments, "assignee"),
            flagged: get_bool(arguments, "flagged"),
            priority: get_str(arguments, "priority"),
            label: get_str(arguments, "label"),
            created_after: get_str(arguments, "created_after"),
            updated_after: get_str(arguments, "updated_after"),
            blocked: get_bool(arguments, "blocked"),
            ready: get_bool(arguments, "ready"),
            stale: get_str(arguments, "stale"),
        }),
        "storyhook_get_story" => {
            let id = get_str(arguments, "id").ok_or("missing required parameter: id")?;
            Ok(Invocation::Show { id })
        }
        "storyhook_create_story" => {
            let title = get_str(arguments, "title").ok_or("missing required parameter: title")?;
            Ok(Invocation::New { title })
        }
        "storyhook_update_story" => {
            let id = get_str(arguments, "id").ok_or("missing required parameter: id")?;
            // Process updates in priority order — only one action per call
            if let Some(state) = get_str(arguments, "state") {
                return Ok(Invocation::SetState {
                    id,
                    state,
                    comment: None,
                });
            }
            if let Some(priority) = get_str(arguments, "priority") {
                return Ok(Invocation::SetPriority { id, priority });
            }
            if let Some(labels_add) = get_str(arguments, "labels_add") {
                let add: Vec<String> = labels_add
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                return Ok(Invocation::SetLabels {
                    id,
                    add,
                    remove: Vec::new(),
                });
            }
            if let Some(labels_remove) = get_str(arguments, "labels_remove") {
                let remove: Vec<String> = labels_remove
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                return Ok(Invocation::SetLabels {
                    id,
                    add: Vec::new(),
                    remove,
                });
            }
            if let Some(assignee) = get_str(arguments, "assignee") {
                return Ok(Invocation::Assign {
                    id,
                    member: assignee,
                });
            }
            if let Some(awaiting) = get_str(arguments, "awaiting") {
                if awaiting.is_empty() {
                    return Ok(Invocation::ClearAwaiting { id });
                }
                return Ok(Invocation::SetAwaiting { id, awaiting });
            }
            Err("no update fields provided".to_string())
        }
        "storyhook_add_comment" => {
            let id = get_str(arguments, "id").ok_or("missing required parameter: id")?;
            let text = get_str(arguments, "text").ok_or("missing required parameter: text")?;
            Ok(Invocation::Comment { id, text })
        }
        "storyhook_get_summary" => Ok(Invocation::Summary),
        "storyhook_get_next" => {
            let count = arguments.get("count").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
            Ok(Invocation::Next {
                count: count.max(1),
            })
        }
        "storyhook_search" => {
            let query = get_str(arguments, "query").ok_or("missing required parameter: query")?;
            Ok(Invocation::Search { query })
        }
        "storyhook_get_graph" => {
            if let Some(blocked_by) = get_str(arguments, "blocked_by") {
                return Ok(Invocation::Graph {
                    mode: GraphMode::BlockedBy(blocked_by),
                });
            }
            let mode = get_str(arguments, "mode").unwrap_or_else(|| "overview".to_string());
            match mode.as_str() {
                "overview" => Ok(Invocation::Graph {
                    mode: GraphMode::Overview,
                }),
                "critical_path" => Ok(Invocation::Graph {
                    mode: GraphMode::CriticalPath,
                }),
                "parallel_groups" => Ok(Invocation::Graph {
                    mode: GraphMode::ParallelGroups,
                }),
                _ => Err(format!("unknown graph mode: {mode}")),
            }
        }
        "storyhook_scaffold" => {
            let kind = get_str(arguments, "kind").ok_or("missing required parameter: kind")?;
            if kind != "agents-md" && kind != "claude-md" && kind != "cursor-rules" {
                return Err(format!("invalid scaffold kind: {kind}"));
            }
            Ok(Invocation::Scaffold { kind })
        }
        "storyhook_decompose_spec" => {
            let content =
                get_str(arguments, "content").ok_or("missing required parameter: content")?;
            let dry_run = get_bool(arguments, "dry_run");
            // Write content to a temp file
            let temp_dir = std::env::temp_dir();
            let temp_file =
                temp_dir.join(format!("storyhook-decompose-mcp-{}.md", std::process::id()));
            std::fs::write(&temp_file, &content)
                .map_err(|e| format!("failed to write temp file: {e}"))?;
            Ok(Invocation::Decompose {
                file: Some(temp_file.to_string_lossy().to_string()),
                stdin: false,
                dry_run,
            })
        }
        "storyhook_bulk_create" => {
            let stories = arguments
                .get("stories")
                .ok_or("missing required parameter: stories")?;
            let stories_json =
                serde_json::to_string(stories).map_err(|e| format!("invalid stories: {e}"))?;
            // Write to a temp file and use Import
            let temp_dir = std::env::temp_dir();
            let temp_file = temp_dir.join(format!("storyhook-import-{}.json", std::process::id()));
            std::fs::write(&temp_file, &stories_json)
                .map_err(|e| format!("failed to write temp file: {e}"))?;
            Ok(Invocation::Import {
                file: Some(temp_file.to_string_lossy().to_string()),
            })
        }
        "storyhook_generate_report" => {
            let format = get_str(arguments, "format").unwrap_or_else(|| "text".to_string());
            Ok(Invocation::Report {
                html: format == "html",
            })
        }
        "storyhook_sync_git" => Ok(Invocation::SyncGit {
            since: get_str(arguments, "since"),
        }),
        _ => Err(format!("unknown tool: {tool_name}")),
    }
}
