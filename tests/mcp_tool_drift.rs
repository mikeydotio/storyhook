//! The MCP server's anti-drift guards (SH-340, `docs/spec/mcp-server.md`).
//!
//! v1's MCP server (`src/mcp.rs`, deleted 2026-04-07, SH-32) hand-wrote a
//! JSON Schema and a hand-written `Invocation` mapping per tool — a second,
//! human-synchronized copy of the CLI's command surface. Stories SH-9 and
//! SH-17 exist in this tracker's own history for no purpose but re-syncing
//! one field into those schemas after it drifted. This file is what stands
//! between the new server and the same fate: every check here fails loudly
//! the moment a tool's declared shape stops matching what it actually sends
//! to `cli::parse_invocation` — the same function the real CLI binary
//! calls, and the only place either door decides what an `Invocation`
//! means.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Map, Value, json};

use storyhook::cli::{self, Invocation};
use storyhook::mcp::{TOOLS, tool_for_variant};

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

fn map(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

// ---------------------------------------------------------------------------
// 1. The tool table and the exhaustive fence agree with each other.
// ---------------------------------------------------------------------------

#[test]
fn every_curated_tool_has_a_reverse_entry_in_tool_for_variant() {
    assert_eq!(
        TOOLS.len(),
        16,
        "the curated tool count changed — update this floor deliberately"
    );
    // The tool table's field metadata (`FieldSpec`/`FieldKind`) lives in a
    // private module, on purpose — schema-generation detail, not part of
    // this crate's public surface. This loop never names that type: `tool`
    // and `field` are used purely by field access, with their types
    // inferred from `TOOLS` itself, which is all Rust's privacy rules
    // require for that to compile.
    for tool in TOOLS {
        let mut minimal = Map::new();
        for field in tool.fields {
            if !field.required {
                continue;
            }
            let value = match field.name {
                "id" | "a" | "b" | "member" => json!("SH-1"),
                "labels" => json!(["x"]),
                _ => json!("x"),
            };
            minimal.insert(field.name.to_string(), value);
        }
        // `story set <id>` alone is refused by `parse_set` — it requires at
        // least one field to actually change, a cross-field rule no single
        // `FieldSpec::required` can express. Not a special case in the
        // schema, only in this test's otherwise-generic "smallest legal
        // call" construction.
        if tool.name == "story_set" {
            minimal.insert("title".to_string(), json!("x"));
        }
        let built = (tool.build_argv)(&minimal).unwrap_or_else(|e| {
            panic!(
                "{}'s own build_argv rejected its own minimal arguments: {e}",
                tool.name
            )
        });
        let invocation = cli::parse_invocation(&built).unwrap_or_else(|e| {
            panic!("{} built an argv parse_invocation refused: {e}", tool.name)
        });
        assert_eq!(
            tool_for_variant(&invocation),
            Some(tool.name),
            "{} built an Invocation that tool_for_variant maps to a different tool (or none)",
            tool.name
        );
    }
}

// ---------------------------------------------------------------------------
// 2. Same-answer-two-doors: a tool's construction matches an independently
//    hand-written equivalent CLI invocation, field for field.
// ---------------------------------------------------------------------------

fn call(tool_name: &str, args: &Map<String, Value>) -> Invocation {
    let tool = TOOLS
        .iter()
        .find(|t| t.name == tool_name)
        .unwrap_or_else(|| panic!("no tool named {tool_name} — this test's own name is stale"));
    let built = (tool.build_argv)(args).unwrap_or_else(|e| panic!("{tool_name}: {e}"));
    cli::parse_invocation(&built).unwrap_or_else(|e| panic!("{tool_name}: {e}"))
}

#[test]
fn story_list_matches_the_equivalent_cli_invocation() {
    let via_tool = call(
        "story_list",
        &map(&[
            ("project", json!("SH")),
            ("state", json!("todo")),
            ("assignee", json!("mikey")),
            ("flagged", json!(true)),
            ("priority", json!("high,critical")),
            ("label", json!("bug")),
            ("created_after", json!("2026-01-01")),
            ("updated_after", json!("2026-06-01")),
            ("blocked", json!(true)),
            ("ready", json!(false)),
            ("stale", json!("2h")),
            ("phase", json!("1")),
            ("story_type", json!("epic")),
            ("drafts", json!(true)),
        ]),
    );
    let via_cli = cli::parse_invocation(&argv(&[
        "list",
        "--state",
        "todo",
        "--assignee",
        "mikey",
        "--flagged",
        "--priority",
        "high,critical",
        "--label",
        "bug",
        "--created-after",
        "2026-01-01",
        "--updated-after",
        "2026-06-01",
        "--blocked",
        "--stale",
        "2h",
        "--phase",
        "1",
        "--type",
        "epic",
        "--drafts",
    ]))
    .unwrap();
    assert_eq!(via_tool, via_cli);
}

#[test]
fn story_next_matches_the_equivalent_cli_invocation() {
    let via_tool = call(
        "story_next",
        &map(&[
            ("project", json!("SH")),
            ("count", json!(3)),
            ("phase", json!("2")),
        ]),
    );
    let via_cli = cli::parse_invocation(&argv(&["next", "--count", "3", "--phase", "2"])).unwrap();
    assert_eq!(via_tool, via_cli);
}

#[test]
fn story_next_defaults_count_to_one_exactly_as_the_cli_does() {
    let via_tool = call("story_next", &map(&[("project", json!("SH"))]));
    let via_cli = cli::parse_invocation(&argv(&["next"])).unwrap();
    assert_eq!(via_tool, via_cli);
}

#[test]
fn story_show_matches_the_equivalent_cli_invocation() {
    let via_tool = call(
        "story_show",
        &map(&[("project", json!("SH")), ("id", json!("SH-42"))]),
    );
    let via_cli = cli::parse_invocation(&argv(&["show", "SH-42"])).unwrap();
    assert_eq!(via_tool, via_cli);
}

#[test]
fn story_search_matches_the_equivalent_cli_invocation() {
    let via_tool = call(
        "story_search",
        &map(&[
            ("project", json!("SH")),
            ("query", json!("dashboard timeout")),
        ]),
    );
    let via_cli = cli::parse_invocation(&argv(&["search", "dashboard timeout"])).unwrap();
    assert_eq!(via_tool, via_cli);
}

#[test]
fn story_summary_matches_the_equivalent_cli_invocation() {
    let via_tool = call("story_summary", &map(&[("project", json!("SH"))]));
    let via_cli = cli::parse_invocation(&argv(&["summary"])).unwrap();
    assert_eq!(via_tool, via_cli);
}

#[test]
fn story_new_matches_the_equivalent_cli_invocation() {
    let via_tool = call(
        "story_new",
        &map(&[
            ("project", json!("SH")),
            ("title", json!("Fix the thing")),
            ("state", json!("todo")),
            ("story_type", json!("bug")),
            ("description", json!("It is broken")),
            ("priority", json!("high")),
            ("assignee", json!("mikey")),
            ("labels", json!(["urgent", "backend"])),
            ("draft", json!(true)),
        ]),
    );
    let via_cli = cli::parse_invocation(&argv(&[
        "new",
        "Fix the thing",
        "--state",
        "todo",
        "--type",
        "bug",
        "--description",
        "It is broken",
        "--priority",
        "high",
        "--assignee",
        "mikey",
        "--label",
        "urgent",
        "--label",
        "backend",
        "--draft",
    ]))
    .unwrap();
    assert_eq!(via_tool, via_cli);
}

#[test]
fn story_move_matches_the_equivalent_cli_invocation() {
    let via_tool = call(
        "story_move",
        &map(&[
            ("project", json!("SH")),
            ("id", json!("SH-1")),
            ("state", json!("done")),
            ("if_state", json!("in-progress")),
            ("reason", json!("waiting on review")),
            ("comment", json!("shipped it")),
        ]),
    );
    let via_cli = cli::parse_invocation(&argv(&[
        "move",
        "SH-1",
        "done",
        "--if-state",
        "in-progress",
        "--reason",
        "waiting on review",
        "shipped it",
    ]))
    .unwrap();
    assert_eq!(via_tool, via_cli);
}

#[test]
fn story_comment_matches_the_equivalent_cli_invocation() {
    let via_tool = call(
        "story_comment",
        &map(&[
            ("project", json!("SH")),
            ("id", json!("SH-1")),
            ("text", json!("looks good")),
        ]),
    );
    let via_cli = cli::parse_invocation(&argv(&["comment", "SH-1", "looks good"])).unwrap();
    assert_eq!(via_tool, via_cli);
}

#[test]
fn story_assign_matches_the_equivalent_cli_invocation() {
    let via_tool = call(
        "story_assign",
        &map(&[
            ("project", json!("SH")),
            ("id", json!("SH-1")),
            ("member", json!("mikey")),
        ]),
    );
    let via_cli = cli::parse_invocation(&argv(&["assign", "SH-1", "mikey"])).unwrap();
    assert_eq!(via_tool, via_cli);
}

#[test]
fn story_prioritize_matches_the_equivalent_cli_invocation() {
    let via_tool = call(
        "story_prioritize",
        &map(&[
            ("project", json!("SH")),
            ("id", json!("SH-1")),
            ("priority", json!("critical")),
        ]),
    );
    let via_cli = cli::parse_invocation(&argv(&["prioritize", "SH-1", "critical"])).unwrap();
    assert_eq!(via_tool, via_cli);
}

#[test]
fn story_label_matches_the_equivalent_cli_invocation() {
    let via_tool = call(
        "story_label",
        &map(&[
            ("project", json!("SH")),
            ("id", json!("SH-1")),
            ("labels", json!(["bug", "p1"])),
        ]),
    );
    let via_cli = cli::parse_invocation(&argv(&["label", "SH-1", "bug,p1"])).unwrap();
    assert_eq!(via_tool, via_cli);
}

#[test]
fn story_relate_matches_the_equivalent_cli_invocation() {
    let via_tool = call(
        "story_relate",
        &map(&[
            ("project", json!("SH")),
            ("a", json!("SH-1")),
            ("relation", json!("blocks")),
            ("b", json!("SH-2")),
        ]),
    );
    let via_cli = cli::parse_invocation(&argv(&["relate", "SH-1", "blocks", "SH-2"])).unwrap();
    assert_eq!(via_tool, via_cli);
}

#[test]
fn story_block_matches_the_equivalent_cli_invocation() {
    let via_tool = call(
        "story_block",
        &map(&[
            ("project", json!("SH")),
            ("id", json!("SH-1")),
            ("reason", json!("waiting on design")),
        ]),
    );
    let via_cli = cli::parse_invocation(&argv(&["block", "SH-1", "waiting on design"])).unwrap();
    assert_eq!(via_tool, via_cli);
}

#[test]
fn story_unblock_matches_the_equivalent_cli_invocation() {
    let via_tool = call(
        "story_unblock",
        &map(&[("project", json!("SH")), ("id", json!("SH-1"))]),
    );
    let via_cli = cli::parse_invocation(&argv(&["unblock", "SH-1"])).unwrap();
    assert_eq!(via_tool, via_cli);
}

#[test]
fn story_set_matches_the_equivalent_cli_invocation() {
    let via_tool = call(
        "story_set",
        &map(&[
            ("project", json!("SH")),
            ("id", json!("SH-1")),
            ("title", json!("New title")),
            ("state", json!("todo")),
            ("priority", json!("low")),
            ("assignee", json!("mikey")),
            ("labels", json!("a,b")),
            ("blocked", json!("waiting")),
            ("unblocked", json!(false)),
            ("json", json!("{\"x\":1}")),
            ("story_type", json!("bug")),
            ("description", json!("new description")),
        ]),
    );
    let via_cli = cli::parse_invocation(&argv(&[
        "set",
        "SH-1",
        "--title",
        "New title",
        "--state",
        "todo",
        "--priority",
        "low",
        "--assignee",
        "mikey",
        "--labels",
        "a,b",
        "--blocked",
        "waiting",
        "--json",
        "{\"x\":1}",
        "--type",
        "bug",
        "--description",
        "new description",
    ]))
    .unwrap();
    assert_eq!(via_tool, via_cli);
}

#[test]
fn story_context_matches_the_equivalent_cli_invocation() {
    let via_tool = call(
        "story_context",
        &map(&[("project", json!("SH")), ("format", json!("json"))]),
    );
    let via_cli = cli::parse_invocation(&argv(&["load-context", "--format", "json"])).unwrap();
    assert_eq!(via_tool, via_cli);
}

// ---------------------------------------------------------------------------
// 3. No second schema: `json_schema` in `src/mcp/tools.rs` is the only site
//    under `src/mcp/` that constructs a `"properties"` key.
// ---------------------------------------------------------------------------

fn tracked_mcp_sources(root: &Path) -> BTreeMap<String, String> {
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z", "--", "src/mcp/*.rs"])
        .output()
        .expect("listing this repository's tracked src/mcp files");
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so this scan proved nothing: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|path| {
            let relative = std::str::from_utf8(path).expect("a UTF-8 path").to_string();
            let text = std::fs::read_to_string(root.join(&relative))
                .unwrap_or_else(|e| panic!("reading {relative}: {e}"));
            (relative, text)
        })
        .collect()
}

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn exactly_one_schema_object_is_built() {
    let sources = tracked_mcp_sources(repo_root());
    assert!(
        !sources.is_empty(),
        "found no tracked src/mcp/*.rs files — this scan proved nothing"
    );

    let mut sites: Vec<String> = Vec::new();
    for (path, text) in &sources {
        for (n, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            if line.contains("\"properties\"") {
                sites.push(format!("{path}:{}", n + 1));
            }
        }
    }
    assert_eq!(
        sites.len(),
        1,
        "expected exactly one site under src/mcp/ to build a schema's \"properties\" key (the \
         shared json_schema function in src/mcp/tools.rs), found {}: {sites:?} — a second site \
         is a second, hand-written schema for some tool, which is the exact shape of the \
         SH-9/SH-17 failure this scan exists to catch. Route the new tool's schema through \
         json_schema instead.",
        sites.len()
    );
}

// ---------------------------------------------------------------------------
// 4. No ambient state: this server reads neither the process environment
//    nor its own working directory. Project, actor, and (implicitly) cwd
//    travel as explicit arguments on every tool call — see this test's
//    sibling requirement in `docs/spec/mcp-server.md`.
// ---------------------------------------------------------------------------

#[test]
fn nothing_under_src_mcp_reads_the_process_environment_or_its_cwd() {
    let sources = tracked_mcp_sources(repo_root());
    assert!(
        !sources.is_empty(),
        "found no tracked src/mcp/*.rs files — this scan proved nothing"
    );

    let forbidden = ["std::env::var", "env::var", "current_dir"];
    let mut offenders: Vec<String> = Vec::new();
    for (path, text) in &sources {
        for (n, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            if forbidden.iter().any(|pat| line.contains(pat)) {
                offenders.push(format!("{path}:{}", n + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "{offenders:?} read ambient process state under src/mcp/ — this server is a stateless \
         per-call bridge by design (docs/spec/mcp-server.md): project, actor, and cwd must \
         travel as explicit tool arguments or values resolved once at process start in \
         src/main.rs, never read fresh, per call, from this long-lived process's own \
         environment or working directory."
    );
}
