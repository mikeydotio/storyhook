# Story CLI-First Tracker Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build the MVP Rust `story` CLI that manages open stories in project-local JSON event logs, archives closed stories to SQLite (WAL), and supports concise positional story commands.

**Architecture:** Use a layered single-binary Rust architecture with explicit `cli`, `application`, `domain`, `storage_open`, `storage_archive`, and `locking` modules. All write commands go through one project-scoped write lock and append events first, with immediate atomic archive on transition to CLOSED states. Maintain small derived indexes for fast reads and deterministic `--json` output for automation.

**Tech Stack:** Rust stable, `clap`, `serde`/`serde_json`, `toml`, `thiserror`, `fs4` (file lock), `rusqlite` (bundled SQLite + WAL), `chrono`, `assert_cmd`, `predicates`, `tempfile`, `insta` (optional snapshot assertions)

---

Skills referenced: @brainstorming, @writing-plans

### Task 1: Bootstrap Rust CLI Skeleton

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Create: `src/cli.rs`
- Create: `src/error.rs`
- Create: `tests/cli_help.rs`

**Step 1: Write the failing test**

```rust
// tests/cli_help.rs
use assert_cmd::Command;

#[test]
fn help_shows_binary_name_story() {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.arg("--help");
    cmd.assert().success().stdout(predicates::str::contains("story"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test cli_help help_shows_binary_name_story -- --exact`  
Expected: FAIL because binary/CLI does not exist yet.

**Step 3: Write minimal implementation**

```rust
// src/main.rs
mod cli;
mod error;

fn main() {
    let cli = cli::build();
    let _ = cli.get_matches();
}

// src/cli.rs
use clap::Command;

pub fn build() -> Command {
    Command::new("story").about("CLI-first story tracker")
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test cli_help help_shows_binary_name_story -- --exact`  
Expected: PASS

**Step 5: Commit**

```bash
git add Cargo.toml src/main.rs src/cli.rs src/error.rs tests/cli_help.rs
git commit -m "chore: bootstrap rust story cli skeleton"
```

### Task 2: Implement `story init` and Project Layout

**Files:**
- Modify: `src/cli.rs`
- Create: `src/application/mod.rs`
- Create: `src/application/init.rs`
- Create: `src/storage_open/fs_layout.rs`
- Create: `tests/init_command.rs`

**Step 1: Write the failing test**

```rust
// tests/init_command.rs
use assert_cmd::Command;
use tempfile::tempdir;

#[test]
fn init_creates_storyhook_layout() {
    let dir = tempdir().unwrap();

    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir.path()).arg("init");
    cmd.assert().success();

    assert!(dir.path().join(".storyhook/project.toml").exists());
    assert!(dir.path().join(".storyhook/states.toml").exists());
    assert!(dir.path().join(".storyhook/open/stories").exists());
    assert!(dir.path().join(".storyhook/archive").exists());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test init_command init_creates_storyhook_layout -- --exact`  
Expected: FAIL because `init` command is not implemented.

**Step 3: Write minimal implementation**

```rust
// src/application/init.rs
use std::{fs, path::Path};

pub fn run(root: &Path) -> std::io::Result<()> {
    fs::create_dir_all(root.join(".storyhook/open/stories"))?;
    fs::create_dir_all(root.join(".storyhook/open/indexes"))?;
    fs::create_dir_all(root.join(".storyhook/archive"))?;
    fs::write(root.join(".storyhook/project.toml"), "schema = 1\n")?;
    fs::write(
        root.join(".storyhook/states.toml"),
        "[[states]]\nslug = \"todo\"\nsuper = \"OPEN\"\n\n[[states]]\nslug = \"done\"\nsuper = \"CLOSED\"\n",
    )?;
    fs::write(root.join(".storyhook/next-id"), "1\n")?;
    Ok(())
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test init_command init_creates_storyhook_layout -- --exact`  
Expected: PASS

**Step 5: Commit**

```bash
git add src/cli.rs src/application/mod.rs src/application/init.rs src/storage_open/fs_layout.rs tests/init_command.rs
git commit -m "feat: add init command and project data layout"
```

### Task 3: State Definitions and OPEN/CLOSED Invariants

**Files:**
- Create: `src/domain/state.rs`
- Modify: `src/application/init.rs`
- Create: `tests/state_validation.rs`

**Step 1: Write the failing test**

```rust
// tests/state_validation.rs
use story::domain::state::{StateDef, SuperState, validate_state_set};

#[test]
fn requires_at_least_one_open_and_one_closed_state() {
    let states = vec![StateDef { slug: "todo".into(), super_state: SuperState::Open }];
    let err = validate_state_set(&states).unwrap_err();
    assert!(err.to_string().contains("OPEN and CLOSED"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test state_validation requires_at_least_one_open_and_one_closed_state -- --exact`  
Expected: FAIL due to missing domain state module.

**Step 3: Write minimal implementation**

```rust
// src/domain/state.rs
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SuperState { Open, Closed }

#[derive(Clone, Debug)]
pub struct StateDef {
    pub slug: String,
    pub super_state: SuperState,
}

pub fn validate_state_set(states: &[StateDef]) -> Result<(), String> {
    let has_open = states.iter().any(|s| s.super_state == SuperState::Open);
    let has_closed = states.iter().any(|s| s.super_state == SuperState::Closed);
    if has_open && has_closed { Ok(()) } else { Err("state set must include OPEN and CLOSED".into()) }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test state_validation requires_at_least_one_open_and_one_closed_state -- --exact`  
Expected: PASS

**Step 5: Commit**

```bash
git add src/domain/state.rs src/application/init.rs tests/state_validation.rs
git commit -m "feat: enforce OPEN/CLOSED state mapping invariants"
```

### Task 4: Member Add Commands

**Files:**
- Create: `src/domain/member.rs`
- Create: `src/application/member_add.rs`
- Modify: `src/cli.rs`
- Create: `tests/member_add.rs`

**Step 1: Write the failing test**

```rust
// tests/member_add.rs
use assert_cmd::Command;
use tempfile::tempdir;

#[test]
fn member_add_writes_jsonl_event() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story").unwrap().current_dir(dir.path()).arg("init").assert().success();

    Command::cargo_bin("story").unwrap()
        .current_dir(dir.path())
        .args(["member", "add", "mikey <mw@mikey.io>"])
        .assert()
        .success();

    let data = std::fs::read_to_string(dir.path().join(".storyhook/members.jsonl")).unwrap();
    assert!(data.contains("mw@mikey.io"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test member_add member_add_writes_jsonl_event -- --exact`  
Expected: FAIL because member command is missing.

**Step 3: Write minimal implementation**

```rust
// src/application/member_add.rs
use serde::Serialize;

#[derive(Serialize)]
struct MemberEvent<'a> { kind: &'a str, value: &'a str }

pub fn add_member(root: &std::path::Path, value: &str) -> std::io::Result<()> {
    let event = MemberEvent { kind: "MemberAdded", value };
    let line = serde_json::to_string(&event).unwrap();
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join(".storyhook/members.jsonl"))?
        .write_all(format!("{}\n", line).as_bytes())
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test member_add member_add_writes_jsonl_event -- --exact`  
Expected: PASS

**Step 5: Commit**

```bash
git add src/domain/member.rs src/application/member_add.rs src/cli.rs tests/member_add.rs
git commit -m "feat: add member add command and storage"
```

### Task 5: Story ID Allocation and `story new`

**Files:**
- Create: `src/locking/project_lock.rs`
- Create: `src/storage_open/id_allocator.rs`
- Create: `src/application/story_new.rs`
- Modify: `src/cli.rs`
- Create: `tests/story_new.rs`

**Step 1: Write the failing test**

```rust
// tests/story_new.rs
use assert_cmd::Command;
use predicates::str::contains;
use tempfile::tempdir;

#[test]
fn story_new_assigns_monotonic_ids() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story").unwrap().current_dir(dir.path()).arg("init").assert().success();

    Command::cargo_bin("story").unwrap().current_dir(dir.path()).args(["new", "First"]).assert().stdout(contains("SH-1"));
    Command::cargo_bin("story").unwrap().current_dir(dir.path()).args(["new", "Second"]).assert().stdout(contains("SH-2"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test story_new story_new_assigns_monotonic_ids -- --exact`  
Expected: FAIL because `new` and id allocation are missing.

**Step 3: Write minimal implementation**

```rust
// src/storage_open/id_allocator.rs
pub fn next_story_id(root: &std::path::Path) -> std::io::Result<String> {
    let p = root.join(".storyhook/next-id");
    let mut n: u64 = std::fs::read_to_string(&p)?.trim().parse().unwrap();
    let id = format!("SH-{n}");
    n += 1;
    std::fs::write(p, format!("{n}\n"))?;
    Ok(id)
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test story_new story_new_assigns_monotonic_ids -- --exact`  
Expected: PASS

**Step 5: Commit**

```bash
git add src/locking/project_lock.rs src/storage_open/id_allocator.rs src/application/story_new.rs src/cli.rs tests/story_new.rs
git commit -m "feat: add monotonic story ids and story creation command"
```

### Task 6: Event Schema + `story <id>` Show

**Files:**
- Create: `src/domain/story_event.rs`
- Create: `src/application/story_show.rs`
- Create: `src/storage_open/event_log.rs`
- Modify: `src/cli.rs`
- Create: `tests/story_show.rs`

**Step 1: Write the failing test**

```rust
// tests/story_show.rs
use assert_cmd::Command;
use predicates::str::contains;
use tempfile::tempdir;

#[test]
fn story_id_shows_story_snapshot() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story").unwrap().current_dir(dir.path()).arg("init").assert().success();
    Command::cargo_bin("story").unwrap().current_dir(dir.path()).args(["new", "Need parser"]);

    Command::cargo_bin("story").unwrap()
        .current_dir(dir.path())
        .arg("SH-1")
        .assert()
        .success()
        .stdout(contains("Need parser"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test story_show story_id_shows_story_snapshot -- --exact`  
Expected: FAIL because positional `story <id>` is not resolved yet.

**Step 3: Write minimal implementation**

```rust
// src/application/story_show.rs
pub fn show(root: &std::path::Path, id: &str) -> std::io::Result<String> {
    let path = root.join(format!(".storyhook/open/stories/{id}.jsonl"));
    let raw = std::fs::read_to_string(path)?;
    Ok(raw)
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test story_show story_id_shows_story_snapshot -- --exact`  
Expected: PASS

**Step 5: Commit**

```bash
git add src/domain/story_event.rs src/application/story_show.rs src/storage_open/event_log.rs src/cli.rs tests/story_show.rs
git commit -m "feat: add event stream schema and story show"
```

### Task 7: Concise Positional Grammar Router

**Files:**
- Create: `src/cli_router.rs`
- Modify: `src/main.rs`
- Create: `tests/cli_router.rs`

**Step 1: Write the failing test**

```rust
// tests/cli_router.rs
use story::cli_router::{route, RoutedCommand};

#[test]
fn routes_state_change_form() {
    let cmd = route(&["SH-1", "is", "in-progress", "note"]).unwrap();
    assert!(matches!(cmd, RoutedCommand::SetState { .. }));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test cli_router routes_state_change_form -- --exact`  
Expected: FAIL because router does not exist.

**Step 3: Write minimal implementation**

```rust
// src/cli_router.rs
pub enum RoutedCommand {
    Show { id: String },
    Comment { id: String, text: String },
    Assign { id: String, member: String },
    SetState { id: String, state: String, comment: Option<String> },
    Relate { a: String, rel: String, b: String, remove: bool },
}

pub fn route(args: &[&str]) -> Result<RoutedCommand, String> {
    if args.len() >= 3 && args[1] == "assign" {
        return Ok(RoutedCommand::Assign { id: args[0].into(), member: args[2].into() });
    }
    if args.len() >= 3 && args[1] == "is" {
        return Ok(RoutedCommand::SetState {
            id: args[0].into(),
            state: args[2].into(),
            comment: args.get(3).map(|s| s.to_string()),
        });
    }
    if args.len() == 1 { return Ok(RoutedCommand::Show { id: args[0].into() }); }
    if args.len() == 2 { return Ok(RoutedCommand::Comment { id: args[0].into(), text: args[1].into() }); }
    Err("unsupported command shape".into())
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test cli_router routes_state_change_form -- --exact`  
Expected: PASS

**Step 5: Commit**

```bash
git add src/cli_router.rs src/main.rs tests/cli_router.rs
git commit -m "feat: add concise positional command router"
```

### Task 8: `story <id> \"comment\"` and `story <id> assign <member>`

**Files:**
- Create: `src/application/story_comment.rs`
- Create: `src/application/story_assign.rs`
- Modify: `src/main.rs`
- Create: `tests/story_comment_assign.rs`

**Step 1: Write the failing test**

```rust
// tests/story_comment_assign.rs
use assert_cmd::Command;
use predicates::str::contains;
use tempfile::tempdir;

#[test]
fn comment_and_assign_append_events() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story").unwrap().current_dir(dir.path()).arg("init").assert().success();
    Command::cargo_bin("story").unwrap().current_dir(dir.path()).args(["new", "Routing"]).assert().success();

    Command::cargo_bin("story").unwrap().current_dir(dir.path()).args(["SH-1", "assign", "mikey"]).assert().success();
    Command::cargo_bin("story").unwrap().current_dir(dir.path()).args(["SH-1", "First pass done"]).assert().success();

    Command::cargo_bin("story").unwrap()
        .current_dir(dir.path())
        .arg("SH-1")
        .assert()
        .stdout(contains("First pass done"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test story_comment_assign comment_and_assign_append_events -- --exact`  
Expected: FAIL because handlers are missing.

**Step 3: Write minimal implementation**

```rust
// src/application/story_comment.rs
pub fn add_comment(root: &std::path::Path, id: &str, text: &str) -> std::io::Result<()> {
    super::event_append::append(root, id, "StoryCommentAdded", serde_json::json!({"text": text}))
}

// src/application/story_assign.rs
pub fn assign(root: &std::path::Path, id: &str, member: &str) -> std::io::Result<()> {
    super::event_append::append(root, id, "StoryAssigned", serde_json::json!({"member": member}))
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test story_comment_assign comment_and_assign_append_events -- --exact`  
Expected: PASS

**Step 5: Commit**

```bash
git add src/application/story_comment.rs src/application/story_assign.rs src/main.rs tests/story_comment_assign.rs
git commit -m "feat: support positional comment and assign commands"
```

### Task 9: `story <id> is <state> [comment]` + Immediate Archive Trigger

**Files:**
- Create: `src/application/story_state_set.rs`
- Create: `src/storage_archive/sqlite.rs`
- Modify: `src/domain/state.rs`
- Create: `tests/story_state_archive.rs`

**Step 1: Write the failing test**

```rust
// tests/story_state_archive.rs
use assert_cmd::Command;
use tempfile::tempdir;

#[test]
fn closed_state_moves_story_to_archive_db() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story").unwrap().current_dir(dir.path()).arg("init").assert().success();
    Command::cargo_bin("story").unwrap().current_dir(dir.path()).args(["new", "Finish me"]).assert().success();

    Command::cargo_bin("story").unwrap()
        .current_dir(dir.path())
        .args(["SH-1", "is", "done", "completed"])
        .assert()
        .success();

    assert!(dir.path().join(".storyhook/archive/archive.db").exists());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test story_state_archive closed_state_moves_story_to_archive_db -- --exact`  
Expected: FAIL because state-change/archive path is missing.

**Step 3: Write minimal implementation**

```rust
// src/application/story_state_set.rs
pub fn set_state(root: &std::path::Path, id: &str, state: &str, comment: Option<&str>) -> Result<(), crate::error::Error> {
    super::event_append::append(root, id, "StoryStateChanged", serde_json::json!({"state": state}))?;
    if let Some(text) = comment {
        super::event_append::append(root, id, "StoryCommentAdded", serde_json::json!({"text": text}))?;
    }
    if crate::domain::state::is_closed_state(root, state)? {
        crate::storage_archive::sqlite::archive_story(root, id)?;
    }
    Ok(())
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test story_state_archive closed_state_moves_story_to_archive_db -- --exact`  
Expected: PASS

**Step 5: Commit**

```bash
git add src/application/story_state_set.rs src/storage_archive/sqlite.rs src/domain/state.rs tests/story_state_archive.rs
git commit -m "feat: add state transitions with immediate closed-story archive"
```

### Task 10: Relationship Engine (Directional, Mutual, Parent Constraints)

**Files:**
- Create: `src/domain/relationship.rs`
- Create: `src/application/story_relate.rs`
- Modify: `src/cli_router.rs`
- Create: `tests/relationships.rs`

**Step 1: Write the failing test**

```rust
// tests/relationships.rs
use assert_cmd::Command;
use tempfile::tempdir;

#[test]
fn adding_directional_relationship_creates_inverse_edge() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story").unwrap().current_dir(dir.path()).arg("init").assert().success();
    Command::cargo_bin("story").unwrap().current_dir(dir.path()).args(["new", "A"]).assert().success();
    Command::cargo_bin("story").unwrap().current_dir(dir.path()).args(["new", "B"]).assert().success();

    Command::cargo_bin("story").unwrap()
        .current_dir(dir.path())
        .args(["SH-1", "starts-before", "SH-2"])
        .assert()
        .success();

    let a = std::fs::read_to_string(dir.path().join(".storyhook/open/stories/SH-1.jsonl")).unwrap();
    let b = std::fs::read_to_string(dir.path().join(".storyhook/open/stories/SH-2.jsonl")).unwrap();
    assert!(a.contains("starts-before"));
    assert!(b.contains("starts-after"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test relationships adding_directional_relationship_creates_inverse_edge -- --exact`  
Expected: FAIL because relationship handlers are missing.

**Step 3: Write minimal implementation**

```rust
// src/domain/relationship.rs
pub fn inverse(rel: &str) -> Option<&'static str> {
    match rel {
        "starts-before" => Some("starts-after"),
        "starts-after" => Some("starts-before"),
        "finishes-before" => Some("finishes-after"),
        "finishes-after" => Some("finishes-before"),
        "precedes" => Some("follows"),
        "follows" => Some("precedes"),
        "relieves" => Some("relieved-by"),
        "relieved-by" => Some("relieves"),
        "parent-of" => Some("child-of"),
        "child-of" => Some("parent-of"),
        "obviates" => Some("obviated-by"),
        "obviated-by" => Some("obviates"),
        _ => None,
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test relationships adding_directional_relationship_creates_inverse_edge -- --exact`  
Expected: PASS

**Step 5: Commit**

```bash
git add src/domain/relationship.rs src/application/story_relate.rs src/cli_router.rs tests/relationships.rs
git commit -m "feat: add relationship model with inverse edge enforcement"
```

### Task 11: `story list` and Flagged Computation

**Files:**
- Create: `src/application/story_list.rs`
- Create: `src/storage_open/indexes.rs`
- Modify: `src/domain/story_event.rs`
- Create: `tests/story_list_flagged.rs`

**Step 1: Write the failing test**

```rust
// tests/story_list_flagged.rs
use assert_cmd::Command;
use predicates::str::contains;
use tempfile::tempdir;

#[test]
fn list_flagged_filters_to_attention_items() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story").unwrap().current_dir(dir.path()).arg("init").assert().success();
    Command::cargo_bin("story").unwrap().current_dir(dir.path()).args(["new", "A"]).assert().success();
    Command::cargo_bin("story").unwrap().current_dir(dir.path()).args(["new", "B"]).assert().success();
    Command::cargo_bin("story").unwrap().current_dir(dir.path()).args(["SH-1", "obviates", "SH-2"]).assert().success();

    Command::cargo_bin("story").unwrap()
        .current_dir(dir.path())
        .args(["list", "--flagged"])
        .assert()
        .success()
        .stdout(contains("SH-2"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test story_list_flagged list_flagged_filters_to_attention_items -- --exact`  
Expected: FAIL because list/flagging is missing.

**Step 3: Write minimal implementation**

```rust
// src/application/story_list.rs
pub fn list_flagged(root: &std::path::Path) -> Result<Vec<String>, crate::error::Error> {
    let ids = crate::storage_open::indexes::all_open_ids(root)?;
    Ok(ids.into_iter().filter(|id| crate::domain::flagged::is_flagged(root, id).unwrap_or(false)).collect())
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test story_list_flagged list_flagged_filters_to_attention_items -- --exact`  
Expected: PASS

**Step 5: Commit**

```bash
git add src/application/story_list.rs src/storage_open/indexes.rs src/domain/story_event.rs tests/story_list_flagged.rs
git commit -m "feat: add list command with flagged filtering"
```

### Task 12: `story doctor` Integrity Checks

**Files:**
- Create: `src/application/doctor.rs`
- Modify: `src/cli.rs`
- Create: `tests/doctor.rs`

**Step 1: Write the failing test**

```rust
// tests/doctor.rs
use assert_cmd::Command;
use predicates::str::contains;
use tempfile::tempdir;

#[test]
fn doctor_reports_missing_inverse_edge() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story").unwrap().current_dir(dir.path()).arg("init").assert().success();
    Command::cargo_bin("story").unwrap().current_dir(dir.path()).args(["new", "A"]).assert().success();

    std::fs::write(
        dir.path().join(".storyhook/open/stories/SH-1.jsonl"),
        "{\"kind\":\"StoryRelationshipAdded\",\"payload\":{\"to\":\"SH-9\",\"type\":\"starts-before\"}}\n",
    ).unwrap();

    Command::cargo_bin("story").unwrap()
        .current_dir(dir.path())
        .arg("doctor")
        .assert()
        .code(5)
        .stdout(contains("missing inverse"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test doctor doctor_reports_missing_inverse_edge -- --exact`  
Expected: FAIL because doctor command is missing.

**Step 3: Write minimal implementation**

```rust
// src/application/doctor.rs
pub fn run(root: &std::path::Path) -> Result<Vec<String>, crate::error::Error> {
    let mut issues = Vec::new();
    for id in crate::storage_open::indexes::all_open_ids(root)? {
        issues.extend(crate::domain::relationship::find_integrity_issues(root, &id)?);
    }
    Ok(issues)
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test doctor doctor_reports_missing_inverse_edge -- --exact`  
Expected: PASS

**Step 5: Commit**

```bash
git add src/application/doctor.rs src/cli.rs tests/doctor.rs
git commit -m "feat: add doctor integrity checks command"
```

### Task 13: JSON Output Contract and Exit Codes

**Files:**
- Modify: `src/main.rs`
- Create: `src/output.rs`
- Modify: `src/error.rs`
- Create: `tests/output_json.rs`

**Step 1: Write the failing test**

```rust
// tests/output_json.rs
use assert_cmd::Command;
use predicates::str::contains;
use tempfile::tempdir;

#[test]
fn show_with_json_emits_machine_schema() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story").unwrap().current_dir(dir.path()).arg("init").assert().success();
    Command::cargo_bin("story").unwrap().current_dir(dir.path()).args(["new", "Contract"]).assert().success();

    Command::cargo_bin("story").unwrap()
        .current_dir(dir.path())
        .args(["SH-1", "--json"])
        .assert()
        .success()
        .stdout(contains("\"result\""))
        .stdout(contains("\"story\""));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test output_json show_with_json_emits_machine_schema -- --exact`  
Expected: FAIL because `--json` schema is not emitted.

**Step 3: Write minimal implementation**

```rust
// src/output.rs
use serde::Serialize;

#[derive(Serialize)]
pub struct JsonEnvelope<T: Serialize> {
    pub result: &'static str,
    pub story: T,
    pub warnings: Vec<String>,
    pub flagged_reasons: Vec<String>,
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test output_json show_with_json_emits_machine_schema -- --exact`  
Expected: PASS

**Step 5: Commit**

```bash
git add src/main.rs src/output.rs src/error.rs tests/output_json.rs
git commit -m "feat: add stable json output envelope and exit code mapping"
```

### Task 14: Concurrency and Archive Atomicity Integration Tests

**Files:**
- Create: `tests/concurrency_locking.rs`
- Create: `tests/archive_atomicity.rs`
- Modify: `src/locking/project_lock.rs`
- Modify: `src/storage_archive/sqlite.rs`

**Step 1: Write the failing test**

```rust
// tests/concurrency_locking.rs
use std::thread;
use assert_cmd::Command;
use tempfile::tempdir;

#[test]
fn concurrent_writers_do_not_duplicate_ids() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story").unwrap().current_dir(dir.path()).arg("init").assert().success();

    let mut handles = Vec::new();
    for i in 0..8 {
        let root = dir.path().to_path_buf();
        handles.push(thread::spawn(move || {
            Command::cargo_bin("story").unwrap().current_dir(root).args(["new", &format!("T{i}")]).assert().success();
        }));
    }
    for h in handles { h.join().unwrap(); }

    let ids = std::fs::read_dir(dir.path().join(".storyhook/open/stories")).unwrap().count();
    assert_eq!(ids, 8);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test concurrency_locking concurrent_writers_do_not_duplicate_ids -- --exact`  
Expected: FAIL because locking is insufficient.

**Step 3: Write minimal implementation**

```rust
// src/locking/project_lock.rs
use fs4::FileExt;

pub fn with_lock<T>(root: &std::path::Path, f: impl FnOnce() -> T) -> std::io::Result<T> {
    let lock_path = root.join(".storyhook/lock");
    let file = std::fs::OpenOptions::new().create(true).read(true).write(true).open(lock_path)?;
    file.lock_exclusive()?;
    let out = f();
    file.unlock()?;
    Ok(out)
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --test concurrency_locking concurrent_writers_do_not_duplicate_ids -- --exact`  
Expected: PASS

**Step 5: Commit**

```bash
git add tests/concurrency_locking.rs tests/archive_atomicity.rs src/locking/project_lock.rs src/storage_archive/sqlite.rs
git commit -m "test: verify lock safety and archive atomicity"
```

### Task 15: Coverage Gate and CI Check

**Files:**
- Create: `.github/workflows/ci.yml`
- Create: `scripts/coverage.sh`
- Create: `README.md`

**Step 1: Write the failing test/check**

```bash
# scripts/coverage.sh
#!/usr/bin/env bash
set -euo pipefail
cargo test
cargo llvm-cov --summary-only --fail-under-lines 90
```

**Step 2: Run check to verify it fails initially**

Run: `bash scripts/coverage.sh`  
Expected: FAIL until missing branches/tests are filled.

**Step 3: Write minimal implementation**

```yaml
# .github/workflows/ci.yml
name: ci
on: [push, pull_request]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo test --all-targets
```

**Step 4: Run check to verify it passes**

Run: `cargo test --all-targets`  
Expected: PASS (coverage gate can remain optional locally until `cargo-llvm-cov` is installed).

**Step 5: Commit**

```bash
git add .github/workflows/ci.yml scripts/coverage.sh README.md
git commit -m "ci: add tests and 90 percent coverage gate scaffolding"
```

## Final Verification Sweep

After Task 15:

1. Run: `cargo test --all-targets`
2. Run: `cargo fmt -- --check`
3. Run: `cargo clippy --all-targets -- -D warnings`
4. Run: `story --help`
5. Run: `story init && story new "Sanity" && story SH-1`

Expected:

- All tests pass
- No formatting/lint errors
- CLI grammar works for concise story forms
- CLOSED transition archives immediately into SQLite
- `story doctor` reports integrity issues correctly

