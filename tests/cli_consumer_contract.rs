//! SH-901: real consumers of the compatible CLI, entirely inside private fixtures.
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Output, Stdio},
    time::Duration,
};
use storyhook_test_support::{
    ChildGuard, Pty, TestEnv,
    load_grace::{Patience, wait_for},
};

fn run(env: &TestEnv, dir: &Path, args: &[&str]) -> Output {
    let mut command = env.raw_story(dir);
    command.args(args);
    ChildGuard::spawn_with_output(&mut command)
        .unwrap()
        .wait_with_output_within(Duration::from_secs(45), || {
            format!("consumer command did not settle: {args:?}")
        })
}
fn ok(env: &TestEnv, dir: &Path, args: &[&str]) -> Vec<u8> {
    let out = run(env, dir, args);
    assert!(
        out.status.success(),
        "{args:?}: {} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}
fn value(env: &TestEnv, dir: &Path, args: &[&str]) -> Value {
    let mut args = args.to_vec();
    args.push("--json");
    serde_json::from_slice(&ok(env, dir, &args)).unwrap()
}
fn show(env: &TestEnv, dir: &Path, id: &str) -> Value {
    value(env, dir, &["show", id])
}

#[test]
fn input_migration_and_legacy_next_shapes_work_for_real_consumers() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    let dir = project.path();
    let empty = value(&env, dir, &["next"]);
    assert_eq!(empty["result"], "ok");
    assert_eq!(empty["message"], "no ready stories");
    assert!(empty.get("story").is_none());
    assert!(empty.get("stories").is_none());
    let id = project.new_story("consumer");
    let one = value(&env, dir, &["next", "--count", "1"]);
    assert_eq!(one["story"]["story"]["id"], id);
    assert!(one.get("stories").is_none());
    let requested_many = value(&env, dir, &["next", "--count", "2"]);
    assert_eq!(requested_many["stories"].as_array().unwrap().len(), 1);
    project.new_story("second");
    assert_eq!(
        value(&env, dir, &["next", "--count", "2"])["stories"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let explicit = value(
        &env,
        dir,
        &[
            "set",
            &id,
            "--input-json",
            r#"{"title":"migrated","complexity":"medium"}"#,
        ],
    );
    assert_eq!(explicit["story"]["story"]["title"], "migrated");
    let legacy = value(&env, dir, &["set", &id, "--json", r#"{"title":"legacy"}"#]);
    assert_eq!(legacy["story"]["story"]["title"], "legacy");
    let before = show(&env, dir, &id);
    let rejected = run(
        &env,
        dir,
        &["set", &id, "--input-json", "{}", "--json", "{}"],
    );
    assert_eq!(rejected.status.code(), Some(2));
    assert_eq!(show(&env, dir, &id), before);
}

#[test]
fn raw_export_round_trip_preserves_story_data_without_an_envelope() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    let dir = project.path();
    let id = project.new_story("export consumer");
    ok(&env, dir, &["label", &id, "audit"]);
    ok(&env, dir, &["prioritize", &id, "high"]);
    let before = show(&env, dir, &id);
    let export = ok(&env, dir, &["export"]);
    assert_eq!(export, ok(&env, dir, &["export", "--json", "--quiet"]));
    let document: Value = serde_json::from_slice(&export).unwrap();
    assert!(document.get("result").is_none());
    let restore = env.home().join("restore");
    fs::create_dir(&restore).unwrap();
    let input = restore.join("export.json");
    fs::write(&input, &export).unwrap();
    ok(&env, &restore, &["import-project", input.to_str().unwrap()]);
    let after = show(&env, &restore, &id);
    assert_eq!(after["story"]["story"], before["story"]["story"]);
    assert_eq!(after["story"]["labels"], before["story"]["labels"]);
    assert_eq!(ok(&env, &restore, &["export"]), export);
}

#[test]
fn quiet_errors_guarded_conflicts_and_unsupported_preview_leave_data_unchanged() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    let dir = project.path();
    let id = project.new_story("protected");
    let before = ok(&env, dir, &["export"]);
    assert!(ok(&env, dir, &["show", &id, "--json", "--quiet"]).is_empty());
    for (args, code) in [
        (
            vec!["move", &id, "done", "--if-state", "in-progress", "--json"],
            9,
        ),
        (vec!["delete", &id, "--json", "--quiet"], 2),
        (vec!["delete", &id, "--dry-run", "--force", "--json"], 2),
        (vec!["show", &id, "--dry-run", "--json"], 2),
        (vec!["show", "SH-99999", "--json", "--quiet"], 3),
    ] {
        let out = run(&env, dir, &args);
        assert_eq!(
            out.status.code(),
            Some(code),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        let error: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(error["result"], "error");
        assert_eq!(error["exit_code"], code);
        assert!(out.stderr.is_empty());
        assert_eq!(
            ok(&env, dir, &["export"]),
            before,
            "{args:?} mutated the export"
        );
    }
}

#[test]
fn lifecycle_verbs_and_real_previews_have_distinct_observable_effects() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    let dir = project.path();
    let id = project.new_story("lifecycle");
    let before = ok(&env, dir, &["export"]);
    ok(&env, dir, &["claim", &id, "--dry-run"]);
    ok(&env, dir, &["reset", &id, "--dry-run"]);
    assert_eq!(ok(&env, dir, &["export"]), before);
    let claimed = value(&env, dir, &["claim", &id, "--no-comment"]);
    assert_eq!(claimed["claimed_from"], "todo");
    let before = ok(&env, dir, &["export"]);
    ok(&env, dir, &["unclaim", &id, "--dry-run"]);
    assert_eq!(ok(&env, dir, &["export"]), before);
    let released = value(&env, dir, &["unclaim", &id, "--no-comment"]);
    assert_eq!(released["story"]["story"]["state"], "todo");
    ok(&env, dir, &["move", &id, "in-progress"]);
    ok(&env, dir, &["reset", &id]);
    assert_eq!(show(&env, dir, &id)["story"]["story"]["state"], "todo");
    ok(&env, dir, &["close", &id, "superseded in fixture"]);
    assert_eq!(show(&env, dir, &id)["story"]["story"]["state"], "dropped");
    ok(&env, dir, &["archive", &id]);
    assert_eq!(show(&env, dir, &id)["story"]["story"]["title"], "lifecycle");
    assert_eq!(
        value(&env, dir, &["list", "--include-closed"])["stories"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        value(&env, dir, &["list", "--all"])["stories"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    ok(&env, dir, &["delete", &id, "--force"]);
    assert_eq!(run(&env, dir, &["show", &id]).status.code(), Some(3));
}

#[test]
fn aliases_and_legacy_help_termination_remain_compatible() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    let dir = project.path();
    let one = project.new_story("one");
    let two = project.new_story("two");
    ok(&env, dir, &["link", &one, "blocks", &two]);
    let linked = show(&env, dir, &one);
    ok(&env, dir, &["unrelate", &one, "blocks", &two]);
    ok(&env, dir, &["relate", &one, "blocks", &two]);
    assert_eq!(
        show(&env, dir, &one)["story"]["relationships"],
        linked["story"]["relationships"]
    );
    ok(&env, dir, &["unlink", &one, "blocks", &two]);
    let before = ok(&env, dir, &["export"]);
    assert_eq!(
        ok(&env, dir, &["new", "--", "--help"]),
        ok(&env, dir, &["help", "new"])
    );
    assert_eq!(ok(&env, dir, &["export"]), before);
    for (alias, canonical) in [
        ("states", "state"),
        ("is", "move"),
        ("awaits", "block"),
        ("priority", "prioritize"),
    ] {
        assert_eq!(
            ok(&env, dir, &["help", alias]),
            ok(&env, dir, &["help", canonical])
        );
        assert_eq!(run(&env, dir, &[alias]).status.code(), Some(2));
    }
    assert_eq!(
        ok(&env, dir, &["context", "--format", "json"]),
        ok(&env, dir, &["load-context", "--format", "json"])
    );
}

fn record(message: &str) -> String {
    json!({"at":"2026-10-08T00:00:00Z","level":"INFO","source":"fixture","stream":"stderr","pid":1,"context":"project=fixture","message":message}).to_string()
}
fn await_records(path: &Path, count: usize) -> Vec<Value> {
    wait_for(
        Patience::new(Duration::from_secs(10)),
        Duration::from_millis(20),
        || format!("expected {count} complete records in {}", path.display()),
        || {
            let bytes = fs::read(path).unwrap();
            let lines: Vec<_> = bytes
                .split(|b| *b == b'\n')
                .filter(|s| !s.is_empty())
                .collect();
            if lines.len() != count || !bytes.ends_with(b"\n") {
                return None;
            }
            Some(
                lines
                    .into_iter()
                    .map(|line| serde_json::from_slice(line).unwrap())
                    .collect(),
            )
        },
    )
}

#[test]
fn jsonl_follow_emits_complete_records_and_preserves_stream_framing() {
    let env = TestEnv::isolated();
    let logs = env.home().join("logs");
    fs::create_dir(&logs).unwrap();
    let log = logs.join(format!("{}.jsonl", chrono::Utc::now().format("%Y-%m-%d")));
    let first = record("first");
    let second = record("second");
    fs::write(&log, format!("{first}\n{second}")).unwrap();
    let output = env.home().join("follow.out");
    let errors = env.home().join("follow.err");
    let mut command = env.raw_story(env.home());
    command
        .args(["daemon", "logs", "--directory"])
        .arg(&logs)
        .args(["--json", "--quiet", "--follow"])
        .stdout(Stdio::from(fs::File::create(&output).unwrap()))
        .stderr(Stdio::from(fs::File::create(&errors).unwrap()));
    let mut child = ChildGuard::spawn(&mut command).unwrap();
    assert_eq!(await_records(&output, 1)[0]["message"], "first");
    // The follower has flushed its first drain, but must not publish an unterminated second row.
    assert_eq!(fs::read_to_string(&output).unwrap(), format!("{first}\n"));
    let mut writer = fs::OpenOptions::new().append(true).open(&log).unwrap();
    writer.write_all(b"\n").unwrap();
    writer.flush().unwrap();
    assert_eq!(await_records(&output, 2)[1]["message"], "second");
    child.kill_and_reap();
    assert!(fs::read(&errors).unwrap().is_empty());
    assert!(!env.daemon_is_live());
    assert!(!env.store_path().exists());
}

#[test]
fn delegated_helper_keeps_both_streams_and_exit_status_even_in_json_quiet_mode() {
    let env = TestEnv::isolated();
    let bin = env.home().join("fake-bin");
    fs::create_dir(&bin).unwrap();
    let codex = bin.join("codex");
    fs::write(&codex, "#!/bin/sh\nif [ \"$1 $2 $3\" = 'plugin list --json' ]; then\n printf '%s\\n' '{\"installed\":[{\"pluginId\":\"story@storyhook\",\"name\":\"story\",\"marketplaceName\":\"storyhook\",\"version\":\"0.6.0\",\"installed\":true,\"enabled\":true}]}'\nelse exit 29; fi\n").unwrap();
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o755)).unwrap();
    let root = env
        .home()
        .join(".codex/plugins/cache/storyhook/story/0.6.0");
    fs::create_dir_all(root.join(".codex-plugin")).unwrap();
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::write(root.join(".codex-plugin/plugin.json"), "{}\n").unwrap();
    fs::write(
        root.join("bin/story.sh"),
        "printf 'helper-out:%s\\n' \"$*\"\nprintf 'helper-error\\n' >&2\nexit 17\n",
    )
    .unwrap();
    let mut command = env.raw_story(env.home());
    let paths = std::iter::once(bin)
        .chain(std::env::split_paths(&env.path_with_binary()))
        .collect::<Vec<_>>();
    command
        .env("PATH", std::env::join_paths(paths).unwrap())
        .args([
            "--json", "--quiet", "plugin", "run", "codex", "--", "context",
        ]);
    let out = ChildGuard::spawn_with_output(&mut command)
        .unwrap()
        .wait_with_output_within(Duration::from_secs(30), || {
            "fake helper did not settle".into()
        });
    assert_eq!(
        out.status.code(),
        Some(17),
        "{} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"helper-out:context\n");
    assert_eq!(out.stderr, b"helper-error\n");
    assert!(!env.store_path().exists());
    assert!(!env.daemon_is_live());
}

#[test]
fn terminal_questionnaire_can_cancel_without_creating_a_project() {
    let env = TestEnv::isolated();
    let dir = env.home().join("questionnaire");
    fs::create_dir(&dir).unwrap();
    assert_eq!(run(&env, &dir, &["project", "new"]).status.code(), Some(2));
    let mut command = env.raw_story(&dir);
    command.args(["project", "new"]);
    let mut pty = Pty::spawn("SH-901 terminal cancellation", command);
    for prompt in [
        "Project name",
        "Story-id prefix",
        "Attach this checkout",
        "Generate AGENTS.md?",
    ] {
        pty.expect(prompt);
        pty.send_line("");
    }
    pty.expect("Create this project?");
    pty.send_line("n");
    pty.expect("cancelled; nothing was created");
    assert_eq!(pty.wait().code(), Some(0), "{}", pty.transcript());
    assert!(!dir.join(".storyhook.toml").exists());
    assert!(!dir.join("AGENTS.md").exists());
    assert!(
        String::from_utf8_lossy(&ok(&env, &dir, &["project", "list"])).contains("No projects yet")
    );
}
