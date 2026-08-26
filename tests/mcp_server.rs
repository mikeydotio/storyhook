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

/// A helper for the claim tests: every comment on a story, oldest first.
fn comments_of(mcp: &mut McpSession, slug: &str, id: &str) -> Vec<String> {
    let shown = mcp.call("story_show", json!({ "project": slug, "id": id }));
    assert_eq!(shown["isError"], false, "story_show failed: {shown}");
    let value: Value = serde_json::from_str(text_of(&shown)).expect("story_show's text is JSON");
    value["story"]["story"]["comments"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| entry["text"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Creates one story and returns its id.
fn seed(mcp: &mut McpSession, slug: &str, title: &str) -> String {
    let created = mcp.call("story_new", json!({ "project": slug, "title": title }));
    assert_eq!(created["isError"], false, "story_new failed: {created}");
    let value: Value = serde_json::from_str(text_of(&created)).expect("story_new's text is JSON");
    value["story"]["story"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("story_new did not report an id: {value}"))
        .to_string()
}

/// The whole point of SH-479: before it, an agent over MCP could only
/// `story_move`, which cannot compare-and-swap on the state it just read
/// without a round trip in between.
#[test]
fn claiming_and_releasing_a_story_round_trips_through_the_curated_tools() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let project = env.project().prefix("SH").build();
    let slug = project.slug();
    let mut mcp = McpSession::spawn(&env, project.path());
    let id = seed(&mut mcp, &slug, "Claimable over MCP");

    let claimed = mcp.call("story_claim", json!({ "project": slug, "id": id }));
    assert_eq!(claimed["isError"], false, "story_claim failed: {claimed}");
    let claimed_json: Value = serde_json::from_str(text_of(&claimed)).expect("JSON");
    assert_eq!(claimed_json["story"]["story"]["state"], "in-progress");

    let released = mcp.call("story_unclaim", json!({ "project": slug, "id": id }));
    assert_eq!(
        released["isError"], false,
        "story_unclaim failed: {released}"
    );
    let released_json: Value = serde_json::from_str(text_of(&released)).expect("JSON");
    assert_eq!(released_json["result"], "ok");
    assert_eq!(released_json["story"]["story"]["state"], "todo");
    assert_eq!(released_json["unclaimed_from"], "in-progress");
}

/// The asymmetry between the two tools' defaults, proved end to end rather
/// than argued. A claim's default sentence names the *caller's* host and tmux
/// window and is composed client-side, so this long-lived server — started by
/// whichever shell the agent host happened to run it from — has nothing
/// honest to say and says nothing. An unclaim's default is composed in the
/// store, so it arrives complete.
#[test]
fn a_claim_over_mcp_is_silent_by_default_and_an_unclaim_is_not() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let project = env.project().prefix("SH").build();
    let slug = project.slug();
    let mut mcp = McpSession::spawn(&env, project.path());
    let id = seed(&mut mcp, &slug, "Silent claim");

    let claimed = mcp.call("story_claim", json!({ "project": slug, "id": id }));
    assert_eq!(claimed["isError"], false, "story_claim failed: {claimed}");
    assert!(
        comments_of(&mut mcp, &slug, &id).is_empty(),
        "a claim with no `comment` argument must post nothing at all"
    );

    let released = mcp.call("story_unclaim", json!({ "project": slug, "id": id }));
    assert_eq!(
        released["isError"], false,
        "story_unclaim failed: {released}"
    );
    let posted = comments_of(&mut mcp, &slug, &id);
    assert_eq!(
        posted.len(),
        1,
        "the release's own default comment is composed in the store and must survive the \
         trip over MCP: {posted:?}"
    );
}

#[test]
fn a_comment_argument_is_what_the_claim_posts() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let project = env.project().prefix("SH").build();
    let slug = project.slug();
    let mut mcp = McpSession::spawn(&env, project.path());
    let id = seed(&mut mcp, &slug, "Spoken claim");

    let claimed = mcp.call(
        "story_claim",
        json!({ "project": slug, "id": id, "comment": "claimed by the review agent" }),
    );
    assert_eq!(claimed["isError"], false, "story_claim failed: {claimed}");
    assert_eq!(
        comments_of(&mut mcp, &slug, &id),
        vec!["claimed by the review agent".to_string()]
    );
}

#[test]
fn claiming_next_takes_the_story_story_next_would_have_answered() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let project = env.project().prefix("SH").build();
    let slug = project.slug();
    let mut mcp = McpSession::spawn(&env, project.path());
    let low = seed(&mut mcp, &slug, "Later");
    let high = seed(&mut mcp, &slug, "Sooner");
    let raised = mcp.call(
        "story_prioritize",
        json!({ "project": slug, "id": high, "priority": "critical" }),
    );
    assert_eq!(raised["isError"], false, "{raised}");

    let claimed = mcp.call("story_claim", json!({ "project": slug, "next": true }));
    assert_eq!(claimed["isError"], false, "story_claim failed: {claimed}");
    let claimed_json: Value = serde_json::from_str(text_of(&claimed)).expect("JSON");
    assert_eq!(claimed_json["story"]["story"]["id"], high.as_str());
    assert_eq!(claimed_json["story"]["story"]["state"], "in-progress");
    assert_ne!(claimed_json["story"]["story"]["id"], low.as_str());
}

/// A mutating tool that named no story is refused before anything is written
/// — never resolved to `next`, which is `story claim`'s own rule (SH-476) and
/// matters more here, where the caller is a model filling in a JSON object.
#[test]
fn a_claim_naming_neither_id_nor_next_is_a_tool_error_that_writes_nothing() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let project = env.project().prefix("SH").build();
    let slug = project.slug();
    let mut mcp = McpSession::spawn(&env, project.path());
    let id = seed(&mut mcp, &slug, "Must not be claimed by accident");

    let refused = mcp.call("story_claim", json!({ "project": slug }));
    assert_eq!(refused["isError"], true, "{refused}");
    let text = text_of(&refused);
    assert!(
        text.contains("id") && text.contains("next"),
        "the refusal must name both fields: {text}"
    );

    let shown = mcp.call("story_show", json!({ "project": slug, "id": id }));
    let value: Value = serde_json::from_str(text_of(&shown)).expect("JSON");
    assert_eq!(
        value["story"]["story"]["state"], "todo",
        "a refused claim must have written nothing"
    );
}

#[test]
fn claiming_a_story_someone_else_holds_is_a_tool_error_naming_the_actual_state() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let project = env.project().prefix("SH").build();
    let slug = project.slug();
    let mut mcp = McpSession::spawn(&env, project.path());
    let id = seed(&mut mcp, &slug, "Contended");

    let first = mcp.call("story_claim", json!({ "project": slug, "id": id }));
    assert_eq!(first["isError"], false, "{first}");

    let second = mcp.call("story_claim", json!({ "project": slug, "id": id }));
    assert_eq!(
        second["isError"], true,
        "a second claim must be refused: {second}"
    );
    let text = text_of(&second);
    assert!(
        text.contains("in-progress"),
        "the refusal must name the state the story is actually in: {text}"
    );
}

#[test]
fn story_new_with_omitted_metadata_inherits_the_service_defaults() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let project = env.project().prefix("SH").build();
    let slug = project.slug();
    let mut mcp = McpSession::spawn(&env, project.path());

    let created = mcp.call(
        "story_new",
        json!({ "project": slug, "title": "Defaulted via MCP" }),
    );
    assert_eq!(created["isError"], false, "story_new failed: {created}");
    let created_json: Value =
        serde_json::from_str(text_of(&created)).expect("story_new's text is JSON");

    assert_eq!(created_json["story"]["story"]["priority"], "low");
    assert_eq!(created_json["story"]["story"]["story_type"], "normal");
    assert!(created_json.get("warnings").is_none(), "{created_json}");
}

#[test]
fn story_new_rejects_none_without_consuming_an_id() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let project = env.project().prefix("SH").build();
    let slug = project.slug();
    let mut mcp = McpSession::spawn(&env, project.path());

    let rejected = mcp.call(
        "story_new",
        json!({ "project": slug, "title": "Invalid", "priority": "none" }),
    );
    assert_eq!(rejected["isError"], true, "{rejected}");
    assert!(text_of(&rejected).contains("critical, high, medium, low"));

    let created = mcp.call(
        "story_new",
        json!({ "project": slug, "title": "First valid" }),
    );
    assert_eq!(created["isError"], false, "{created}");
    let created_json: Value = serde_json::from_str(text_of(&created)).unwrap();
    assert_eq!(created_json["story"]["story"]["id"], "SH-1");
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
