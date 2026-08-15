//! `story mcp` — a Model Context Protocol server exposing a curated slice of
//! storyhook to an AI agent host, over the same door every other client
//! uses ([`crate::invoke::HttpInvoker`], `/api/v1/invoke`).
//!
//! Design of record: `docs/spec/mcp-server.md`, including why this exists
//! (storyhook shipped and then deliberately deleted an MCP server in
//! 2026-04, SH-32) and the reversal's own "As built" section.
//!
//! # Why this is not a second binary
//!
//! [`crate::daemon::lifecycle::DaemonInfo::is_this_binary`] identifies a
//! usable daemon by its executable's path *and* modification time. A
//! second binary would never match, would evict the daemon the real `story`
//! binary is using, and the CLI's next command would evict that one back —
//! forever. `story mcp` is therefore a mode of the one binary, dispatched
//! in `main.rs` beside `tui` and the foreground daemon serve mode, never a
//! new `Invocation` variant.
//!
//! # Why every tool call takes `project` and `actor` explicitly
//!
//! This server is a long-lived process serving one stdio session. The
//! ordinary CLI resolves `$STORYHOOK_PROJECT` and `$STORYHOOK_ACTOR` from
//! its own environment exactly once, because it is a short-lived process
//! whose environment belongs to whoever just typed a command
//! (`src/main.rs`'s own doc comment on that resolution explains why). A
//! surviving process has no such guarantee — its environment is whatever
//! the host that spawned it happened to set, once, at launch — so reading
//! either variable here would silently misattribute every write for the
//! rest of the session to that launch-time snapshot. [`server::McpServer`]
//! reads neither; both travel as explicit JSON arguments on every call. See
//! [`tools`]'s module doc for the anti-drift design this enables.

mod protocol;
mod server;
mod tools;

pub use server::McpServer;
pub use tools::{TOOLS, tool_for_variant};

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::env::Environment;

    /// A server is trivial to build without a daemon or even a resolved
    /// real environment, because [`McpServer::new`] takes only
    /// process-startup values and does no I/O of its own —
    /// [`Environment::at`] builds one from a bare path, no filesystem touch
    /// required. What actually talks to a daemon is deferred to
    /// `tools/call`, exercised end to end in `tests/mcp_server.rs` against a
    /// real one via `storyhook_test_support::TestEnv`.
    fn test_env() -> Environment {
        Environment::at(std::env::temp_dir())
    }

    fn roundtrip(server: &McpServer, request: &str) -> serde_json::Value {
        let input = Cursor::new(format!("{request}\n"));
        let mut output = Vec::new();
        server
            .run(input, &mut output)
            .expect("the loop never fails on well-formed input");
        let text = String::from_utf8(output).expect("replies are UTF-8");
        serde_json::from_str(text.trim()).expect("a reply is exactly one JSON value")
    }

    #[test]
    fn initialize_names_the_negotiated_protocol_version_and_this_server() {
        let server = McpServer::new(test_env(), std::env::temp_dir(), 0);
        let reply = roundtrip(
            &server,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#,
        );
        assert_eq!(reply["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(reply["result"]["serverInfo"]["name"], "storyhook");
    }

    #[test]
    fn a_notification_gets_no_reply_at_all() {
        let server = McpServer::new(test_env(), std::env::temp_dir(), 0);
        let input = Cursor::new(
            b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n" as &[u8],
        );
        let mut output = Vec::new();
        server.run(input, &mut output).unwrap();
        assert!(
            output.is_empty(),
            "a notification must produce zero bytes of output"
        );
    }

    #[test]
    fn tools_list_names_every_curated_tool_exactly_once() {
        let server = McpServer::new(test_env(), std::env::temp_dir(), 0);
        let reply = roundtrip(&server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
        let names: Vec<&str> = reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), TOOLS.len());
        for tool in TOOLS {
            assert_eq!(
                names.iter().filter(|n| **n == tool.name).count(),
                1,
                "{} must appear exactly once",
                tool.name
            );
        }
    }

    #[test]
    fn malformed_json_gets_a_parse_error_with_a_null_id() {
        let server = McpServer::new(test_env(), std::env::temp_dir(), 0);
        let input = Cursor::new(b"not json\n" as &[u8]);
        let mut output = Vec::new();
        server.run(input, &mut output).unwrap();
        let reply: serde_json::Value =
            serde_json::from_str(std::str::from_utf8(&output).unwrap().trim()).unwrap();
        assert_eq!(reply["error"]["code"], protocol::PARSE_ERROR);
        assert_eq!(reply["id"], serde_json::Value::Null);
    }

    #[test]
    fn an_unknown_method_on_a_request_errors_but_a_missing_id_never_does() {
        let server = McpServer::new(test_env(), std::env::temp_dir(), 0);
        let reply = roundtrip(
            &server,
            r#"{"jsonrpc":"2.0","id":7,"method":"nonexistent"}"#,
        );
        assert_eq!(reply["error"]["code"], protocol::METHOD_NOT_FOUND);
        assert_eq!(reply["id"], 7);
    }

    #[test]
    fn calling_an_unknown_tool_is_a_protocol_error_not_a_tool_result() {
        let server = McpServer::new(test_env(), std::env::temp_dir(), 0);
        let reply = roundtrip(
            &server,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"story_nonexistent","arguments":{}}}"#,
        );
        assert!(
            reply.get("error").is_some(),
            "an unknown tool name must be a JSON-RPC error"
        );
    }

    #[test]
    fn calling_a_real_tool_with_no_project_is_a_tool_error_not_a_protocol_error() {
        let server = McpServer::new(test_env(), std::env::temp_dir(), 0);
        let reply = roundtrip(
            &server,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"story_summary","arguments":{}}}"#,
        );
        assert!(
            reply.get("result").is_some(),
            "a missing `project` is a tool-result isError, not a JSON-RPC error"
        );
        assert_eq!(reply["result"]["isError"], true);
    }
}
