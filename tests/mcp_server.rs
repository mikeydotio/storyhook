use assert_cmd::Command;
use tempfile::tempdir;

fn mcp_request(dir: &std::path::Path, requests: &str) -> String {
    let output = Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir)
        .arg("--mcp")
        .write_stdin(requests)
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap()
}

fn parse_response(line: &str) -> serde_json::Value {
    serde_json::from_str(line).expect("valid JSON response")
}

#[test]
fn mcp_initialize() {
    let dir = tempdir().unwrap();
    let input = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
    let output = mcp_request(dir.path(), input);
    let response = parse_response(output.trim());
    assert_eq!(response["result"]["serverInfo"]["name"], "storyhook");
    assert!(response["result"]["protocolVersion"].is_string());
}

#[test]
fn mcp_tools_list() {
    let dir = tempdir().unwrap();
    let input = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#;
    let output = mcp_request(dir.path(), input);
    let response = parse_response(output.trim());
    let tools = response["result"]["tools"].as_array().unwrap();
    assert!(tools.len() >= 10);

    let tool_names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(tool_names.contains(&"storyhook_list_stories"));
    assert!(tool_names.contains(&"storyhook_get_story"));
    assert!(tool_names.contains(&"storyhook_create_story"));
    assert!(tool_names.contains(&"storyhook_get_summary"));
    assert!(tool_names.contains(&"storyhook_get_next"));
    assert!(tool_names.contains(&"storyhook_search"));
    assert!(tool_names.contains(&"storyhook_get_graph"));
    assert!(tool_names.contains(&"storyhook_bulk_create"));
}

#[test]
fn mcp_create_and_get_story() {
    let dir = tempdir().unwrap();
    // Init project first
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();

    let requests = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"storyhook_create_story","arguments":{"title":"MCP test story"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"storyhook_get_story","arguments":{"id":"SH-1"}}}"#,
    );

    let output = mcp_request(dir.path(), requests);
    let lines: Vec<&str> = output.trim().lines().collect();
    assert_eq!(lines.len(), 2);

    // Create response
    let create_resp = parse_response(lines[0]);
    assert_eq!(create_resp["id"], 1);
    let content = create_resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(content.contains("SH-1"));

    // Get response
    let get_resp = parse_response(lines[1]);
    assert_eq!(get_resp["id"], 2);
    let content = get_resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(content.contains("MCP test story"));
}

#[test]
fn mcp_list_stories() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["new", "Story A"])
        .assert()
        .success();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["new", "Story B"])
        .assert()
        .success();

    let input = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"storyhook_list_stories","arguments":{}}}"#;
    let output = mcp_request(dir.path(), input);
    let response = parse_response(output.trim());
    let content = response["result"]["content"][0]["text"].as_str().unwrap();
    assert!(content.contains("SH-1"));
    assert!(content.contains("SH-2"));
}

#[test]
fn mcp_get_summary() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["new", "Task"])
        .assert()
        .success();

    let input = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"storyhook_get_summary","arguments":{}}}"#;
    let output = mcp_request(dir.path(), input);
    let response = parse_response(output.trim());
    let content = response["result"]["content"][0]["text"].as_str().unwrap();
    assert!(content.contains("total_open"));
}

#[test]
fn mcp_search() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["new", "Authentication module"])
        .assert()
        .success();

    let input = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"storyhook_search","arguments":{"query":"auth"}}}"#;
    let output = mcp_request(dir.path(), input);
    let response = parse_response(output.trim());
    let content = response["result"]["content"][0]["text"].as_str().unwrap();
    assert!(content.contains("SH-1"));
}

#[test]
fn mcp_unknown_tool() {
    let dir = tempdir().unwrap();
    let input = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"nonexistent_tool","arguments":{}}}"#;
    let output = mcp_request(dir.path(), input);
    let response = parse_response(output.trim());
    let content = response["result"]["content"][0]["text"].as_str().unwrap();
    assert!(content.contains("unknown tool"));
    assert!(response["result"]["isError"].as_bool().unwrap());
}

#[test]
fn mcp_unknown_method() {
    let dir = tempdir().unwrap();
    let input = r#"{"jsonrpc":"2.0","id":1,"method":"unknown/method","params":{}}"#;
    let output = mcp_request(dir.path(), input);
    let response = parse_response(output.trim());
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Method not found")
    );
}

#[test]
fn mcp_ping() {
    let dir = tempdir().unwrap();
    let input = r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":{}}"#;
    let output = mcp_request(dir.path(), input);
    let response = parse_response(output.trim());
    assert!(response["result"].is_object());
    assert!(response["error"].is_null());
}
