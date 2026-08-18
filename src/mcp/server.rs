//! Method dispatch: turns one parsed [`super::protocol::Incoming`] request
//! into a JSON-RPC result, for every method this server answers.
//!
//! `tools/call` is the only method that reaches storyhook itself. Every
//! other method here is protocol bookkeeping — `initialize`, `ping`, the
//! `notifications/*` this server acknowledges by doing nothing.

use std::io::BufRead;
use std::path::PathBuf;

use serde_json::{Map, Value, json};

use crate::api::wire::ProjectSelector;
use crate::domain::provenance::ActorLabel;
use crate::env::Environment;
use crate::invoke::{HttpInvoker, InvokeRequest, Invoker};
use crate::output::{self, Response};

use super::protocol::{self, Incoming};
use super::tools::{self, BuildError};

/// The MCP protocol revision this server negotiates. 2025-11-25 is what
/// clients speak today; the 2026-07-28 revision's stateless redesign is out
/// of scope for this server (`docs/spec/mcp-server.md`'s "As built").
const PROTOCOL_VERSION: &str = "2025-11-25";

/// A bridge from one stdio session to the storyhook daemon, over the exact
/// door the CLI uses ([`HttpInvoker`]).
///
/// Stateless per call, on purpose: every field here is fixed at
/// construction from values [`crate::main`] already resolved once at
/// process start — the same `cwd` and hook depth an ordinary command
/// resolves — and nothing a tool call does can change them. Which project
/// and which actor a call acts on are never among these fields; they travel
/// as explicit tool arguments instead, read fresh on every call. See this
/// module's parent doc comment and `docs/spec/mcp-server.md`.
pub struct McpServer {
    env: Environment,
    cwd: PathBuf,
    hook_depth: u32,
}

impl McpServer {
    #[must_use]
    pub fn new(env: Environment, cwd: PathBuf, hook_depth: u32) -> Self {
        Self {
            env,
            cwd,
            hook_depth,
        }
    }

    /// Runs the stdio loop: one JSON-RPC message per line in, at most one
    /// line out per request (nothing for a notification), until `input`
    /// reaches EOF.
    pub fn run(&self, input: impl BufRead, mut output: impl std::io::Write) -> std::io::Result<()> {
        for line in input.lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(reply) = self.handle_line(trimmed) {
                writeln!(output, "{reply}")?;
                output.flush()?;
            }
        }
        Ok(())
    }

    /// Handles one line, returning the reply to write (`None` for a
    /// notification, which gets none).
    fn handle_line(&self, line: &str) -> Option<String> {
        let incoming = match protocol::parse_line(line) {
            Ok(incoming) => incoming,
            Err(error) => return Some(Value::from(error).to_string()),
        };
        let Some(id) = incoming.id.clone() else {
            // A notification. `notifications/initialized` is the only one a
            // well-behaved client sends unprompted; anything else unknown is
            // silently ignored too, per JSON-RPC 2.0 — a notification gets
            // no reply under any circumstance, including an unrecognized
            // method.
            return None;
        };
        Some(self.dispatch(id, &incoming).to_string())
    }

    fn dispatch(&self, id: Value, incoming: &Incoming) -> Value {
        match incoming.method.as_str() {
            "initialize" => protocol::ok_reply(id, self.initialize()),
            "ping" => protocol::ok_reply(id, json!({})),
            "tools/list" => protocol::ok_reply(id, tools::list()),
            "tools/call" => match self.call_tool(&incoming.params) {
                Ok(result) => protocol::ok_reply(id, result),
                Err((code, message)) => protocol::error_reply(id, code, &message),
            },
            other => protocol::error_reply(
                id,
                protocol::METHOD_NOT_FOUND,
                &format!("method not found: {other}"),
            ),
        }
    }

    fn initialize(&self) -> Value {
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "storyhook",
                "version": env!("CARGO_PKG_VERSION"),
            },
        })
    }

    /// `tools/call`. `Err` is a protocol-level fault (an unknown tool, a
    /// malformed `params`) — everything downstream of a real tool lookup is
    /// a *result*, `isError` true or false, never a JSON-RPC error: a bad
    /// `--project` or a story that does not exist is the tool doing its
    /// job, not this server failing at its own.
    fn call_tool(&self, params: &Value) -> Result<Value, (i32, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((protocol::INVALID_PARAMS, "\"name\" is required".to_string()))?;
        let empty = Map::new();
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(map)) => map,
            Some(_) => {
                return Err((
                    protocol::INVALID_PARAMS,
                    "\"arguments\" must be an object".to_string(),
                ));
            }
        };

        let invocation = match tools::build_invocation(name, arguments) {
            Ok(invocation) => invocation,
            Err(BuildError::UnknownTool) => {
                return Err((protocol::INVALID_PARAMS, format!("unknown tool: {name}")));
            }
            Err(BuildError::BadArguments(detail)) => return Ok(error_result(&detail)),
        };

        // A cheap production sanity check, not only a test one: the
        // `Invocation` this tool actually built must be the one its own
        // entry in `tool_for_variant` names. A mismatch here means this
        // module's own tables disagree with each other, which no caller can
        // cause and no caller should see explained as their mistake.
        debug_assert_eq!(
            tools::tool_for_variant(&invocation),
            Some(name),
            "story_{name} built an Invocation its own tool_for_variant entry does not name",
        );

        // `project` is validated present (non-empty) by `build_invocation`
        // before this point, via `COMMON_FIELDS`.
        let project_slug = arguments
            .get("project")
            .and_then(Value::as_str)
            .expect("build_invocation already required a non-empty `project`")
            .to_string();

        let actor = match arguments.get("actor").and_then(Value::as_str) {
            Some(raw) => match ActorLabel::parse(raw) {
                Ok(actor) => actor,
                Err(error) => return Ok(error_result(&error.to_string())),
            },
            None => None,
        };

        let request = InvokeRequest::new(invocation)
            .project(Some(ProjectSelector::Flag { slug: project_slug }))
            .actor(actor);

        let invoker = HttpInvoker::new(self.env.clone(), &self.cwd).hook_depth(self.hook_depth);
        match invoker.invoke(request) {
            Ok(Response::ConfirmationRequired(plan)) => Ok(error_result(&format!(
                "{}\n\nThis is a destructive action that the `story` CLI would ask a human to \
                 confirm interactively. This tool does not support that two-step flow, so \
                 nothing was changed. Run the equivalent `story` command with `--force` from a \
                 terminal if you are certain.",
                plan.headline(),
            ))),
            Ok(response) => Ok(json!({
                "content": [{ "type": "text", "text": output::render_response(&response, true, false) }],
                "isError": false,
            })),
            Err(error) => Ok(error_result(&output::render_error(&error, true))),
        }
    }
}

/// A tool-call result reporting an execution failure — `isError: true`, per
/// MCP's own contract: this is a successful RPC whose *content* says the
/// requested action did not happen. `text` is plain text in the caller's
/// own sentences, or storyhook's own `--json` error envelope when it came
/// from [`output::render_error`] — either is valid MCP text content.
fn error_result(text: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": true,
    })
}
