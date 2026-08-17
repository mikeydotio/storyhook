//! `story mcp` end to end, as a real subprocess speaking stdio JSON-RPC to
//! a real daemon (SH-340, `docs/spec/mcp-server.md`).
//!
//! `tests/mcp_tool_drift.rs` proves a tool call constructs the right
//! `Invocation`, entirely in-process against `cli::parse_invocation`. This
//! file proves the other half, and does it the way `story mcp` actually
//! runs in production: as a child process of the real `story` binary,
//! never `McpServer` driven in-process from the test binary itself.
//! [`daemon::lifecycle::spawn_child`] execs `current_exe()` to start the
//! daemon, which inside a `cargo test` binary resolves to that test binary,
//! not `story` — so a test that called `McpServer::run` directly would have
//! the daemon it spawns try to re-exec the test harness as if it were
//! `story daemon --serve`, and crash. Spawning `story mcp` as a real
//! subprocess is what keeps `current_exe()` honest, the same reason every
//! other daemon-spawning test in this suite drives `story` as a process
//! rather than calling into `HttpInvoker` from the test binary's own.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};

use serde_json::{Value, json};
use storyhook::daemon::lifecycle;
use storyhook_test_support::TestEnv;

/// Stops whatever daemon `env` is running, even if the test panics first —
/// the same reasoning `tests/daemon_lifecycle.rs`'s own guard gives: a
/// daemon is a detached process, so nothing else reaps it between tests in
/// this binary.
struct DaemonGuard<'a>(&'a TestEnv);

impl Drop for DaemonGuard<'_> {
    fn drop(&mut self) {
        let _ = lifecycle::stop(&self.0.environment(), lifecycle::StopMode::Force);
    }
}

/// A live `story mcp` child process, with a JSON-RPC request/reply helper
/// over its piped stdin/stdout.
struct McpSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: i64,
}

impl McpSession {
    fn spawn(env: &TestEnv, cwd: &Path) -> Self {
        let mut cmd: Command = env.raw_story(cwd);
        cmd.arg("mcp");
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawning `story mcp`");
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    /// Sends one JSON-RPC request and reads the one reply line it produces.
    fn rpc(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.stdin, "{request}").expect("writing to story mcp's stdin");
        self.stdin.flush().expect("flushing story mcp's stdin");

        let mut line = String::new();
        let read = self
            .stdout
            .read_line(&mut line)
            .expect("reading story mcp's stdout");
        if read == 0 {
            let status = self.child.wait().ok();
            panic!("story mcp closed stdout with no reply (exited: {status:?})");
        }
        serde_json::from_str(line.trim())
            .unwrap_or_else(|e| panic!("story mcp's reply was not one JSON value ({e}): {line}"))
    }

    /// `tools/call`, returning the tool result (`{content, isError}`), not
    /// the outer JSON-RPC envelope — what every test below actually wants
    /// to assert on.
    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        self.rpc(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        )["result"]
            .clone()
    }
}

impl Drop for McpSession {
    fn drop(&mut self) {
        // The loop exits on its own once stdin reaches EOF, but a test that
        // panicked mid-call has no graceful way to signal that — `kill` is
        // the guarantee. Nothing else here reaps a detached process (this
        // one is not detached; it is this test's own child), but an
        // un-`wait`ed child is a zombie until someone does.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text_of(result: &Value) -> &str {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text content in {result}"))
}

#[test]
fn a_full_story_lifecycle_through_the_curated_tools() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let project = env.project().prefix("SH").build();
    let slug = project.slug();
    let mut mcp = McpSession::spawn(&env, project.path());

    let created = mcp.call(
        "story_new",
        json!({ "project": slug, "title": "Fix the thing", "priority": "high" }),
    );
    assert_eq!(created["isError"], false, "story_new failed: {created}");
    let created_json: Value =
        serde_json::from_str(text_of(&created)).expect("story_new's text is JSON");
    let id = created_json["story"]["story"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("story_new did not report an id: {created_json}"))
        .to_string();

    let shown = mcp.call("story_show", json!({ "project": slug, "id": id }));
    assert_eq!(shown["isError"], false);
    assert!(text_of(&shown).contains("Fix the thing"));

    let listed = mcp.call("story_list", json!({ "project": slug }));
    assert_eq!(listed["isError"], false);
    assert!(text_of(&listed).contains(&id));

    let commented = mcp.call(
        "story_comment",
        json!({ "project": slug, "id": id, "text": "reviewed" }),
    );
    assert_eq!(commented["isError"], false);

    let moved = mcp.call(
        "story_move",
        json!({ "project": slug, "id": id, "state": "in-progress" }),
    );
    assert_eq!(moved["isError"], false, "story_move failed: {moved}");

    let summary = mcp.call("story_summary", json!({ "project": slug }));
    assert_eq!(summary["isError"], false);
    let summary_text = text_of(&summary);
    assert!(
        summary_text.contains("in-progress") || summary_text.contains("in_progress"),
        "summary after the move should mention the in-progress state: {summary_text}"
    );
}

/// MCP `story_new` inherits `story new`'s unassessed-priority warning
/// (SH-354/SH-359) with zero MCP-side code (SH-358): `build_new` builds an
/// argv and calls `cli::parse_invocation`, the exact function the CLI itself
/// calls, so a tool call with no `priority` produces byte-identically the
/// same `Invocation::New { priority: None, .. }` `story new` with no
/// `--priority` does. This test proves that claim rather than asserting it —
/// the map in SH-358's own plan found no MCP call site for the warning text at
/// all, which only means "inherited for free" if a call through the tool
/// actually carries it.
#[test]
fn story_new_with_no_priority_inherits_the_unassessed_warning() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let project = env.project().prefix("SH").build();
    let slug = project.slug();
    let mut mcp = McpSession::spawn(&env, project.path());

    let created = mcp.call(
        "story_new",
        json!({ "project": slug, "title": "Unassessed via MCP" }),
    );
    assert_eq!(created["isError"], false, "story_new failed: {created}");
    let created_json: Value =
        serde_json::from_str(text_of(&created)).expect("story_new's text is JSON");

    let warnings = created_json["warnings"]
        .as_array()
        .expect("the envelope must carry a `warnings` array when nobody assessed the story");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().is_some_and(|w| w.contains("priority not set"))),
        "expected the unassessed-priority warning, got: {created_json}"
    );
}

#[test]
fn a_missing_project_is_a_tool_error_naming_the_field() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let project = env.project().prefix("SH").build();
    let mut mcp = McpSession::spawn(&env, project.path());

    let result = mcp.call("story_summary", json!({}));
    assert_eq!(result["isError"], true);
    assert!(text_of(&result).contains("project"));
}

#[test]
fn an_unknown_project_slug_is_a_tool_error_not_a_crash() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let project = env.project().prefix("SH").build();
    let mut mcp = McpSession::spawn(&env, project.path());

    let result = mcp.call("story_summary", json!({ "project": "NOPE" }));
    assert_eq!(result["isError"], true);
    assert!(
        text_of(&result).contains("NOPE") || text_of(&result).to_lowercase().contains("no project"),
        "the refusal should say something about the unresolvable project: {result}"
    );
}

#[test]
fn one_server_instance_answers_two_different_projects_independently() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let a = env.project().prefix("AAA").build();
    let b = env.project().prefix("BBB").build();
    let mut mcp = McpSession::spawn(&env, a.path());

    let created_a = mcp.call(
        "story_new",
        json!({ "project": a.slug(), "title": "belongs to A" }),
    );
    assert_eq!(created_a["isError"], false, "{created_a}");
    let created_b = mcp.call(
        "story_new",
        json!({ "project": b.slug(), "title": "belongs to B" }),
    );
    assert_eq!(created_b["isError"], false, "{created_b}");

    let list_a = mcp.call("story_list", json!({ "project": a.slug() }));
    let list_b = mcp.call("story_list", json!({ "project": b.slug() }));
    assert!(text_of(&list_a).contains("belongs to A"));
    assert!(
        !text_of(&list_a).contains("belongs to B"),
        "story A's list must not leak story B"
    );
    assert!(text_of(&list_b).contains("belongs to B"));
    assert!(
        !text_of(&list_b).contains("belongs to A"),
        "story B's list must not leak story A"
    );
}

#[test]
fn calling_an_unknown_tool_is_a_protocol_error() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let project = env.project().prefix("SH").build();
    let mut mcp = McpSession::spawn(&env, project.path());

    let reply = mcp.rpc(
        "tools/call",
        json!({ "name": "story_nonexistent", "arguments": {} }),
    );
    assert!(
        reply.get("error").is_some(),
        "an unknown tool must be a JSON-RPC error, not a result: {reply}"
    );
    assert!(reply.get("result").is_none());
}

#[test]
fn tools_list_matches_the_curated_table_and_every_schema_requires_project() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let project = env.project().prefix("SH").build();
    let mut mcp = McpSession::spawn(&env, project.path());

    let reply = mcp.rpc("tools/list", json!({}));
    let tools = reply["result"]["tools"]
        .as_array()
        .expect("tools/list must return an array");
    assert_eq!(tools.len(), storyhook::mcp::TOOLS.len());
    for tool in tools {
        let required = tool["inputSchema"]["required"]
            .as_array()
            .unwrap_or_else(|| panic!("{tool} has no \"required\" array"));
        assert!(
            required.iter().any(|v| v == "project"),
            "{} does not require \"project\"",
            tool["name"]
        );
    }
}

#[test]
fn initialize_negotiates_before_any_tool_call() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let project = env.project().prefix("SH").build();
    let mut mcp = McpSession::spawn(&env, project.path());

    let reply = mcp.rpc(
        "initialize",
        json!({ "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } }),
    );
    assert_eq!(reply["result"]["serverInfo"]["name"], "storyhook");
    assert_eq!(reply["result"]["protocolVersion"], "2025-11-25");
}
