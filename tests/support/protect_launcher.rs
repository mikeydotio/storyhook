//! SH-585: classify installer-produced launchers, then execute real reader verbs.

use super::*;
use std::io::Write;
use std::process::Stdio;
use storyhook_test_support::{ChildGuard, STORY_COMMAND_DEADLINE, run_bounded};

fn fixture() -> Harness {
    let mut harness = Harness::new(false);
    harness.home = harness._temp.path().join("home with spaces");
    fs::create_dir_all(&harness.home).unwrap();
    harness.install_fake("codex", FAKE_CODEX);
    harness.install_story_on_path();
    let installed = harness.run(&["plugin", "install", "codex"]);
    assert!(installed.status.success(), "{}", combined(&installed));
    harness
}

fn quoted(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn shell(harness: &Harness) -> Command {
    let mut command = Command::new("bash");
    command
        .current_dir(&harness.root)
        .env_clear()
        .env("HOME", &harness.home)
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", harness.fake_bin.display()),
        )
        .env("TMPDIR", harness._temp.path())
        .env("XDG_DATA_HOME", harness.home.join("data"))
        .env("XDG_CONFIG_HOME", harness.home.join("config"))
        .env("XDG_STATE_HOME", harness.home.join("state"))
        .env("STORYHOOK_DATA_DIR", harness.home.join("data/storyhook"))
        .envs(daemon_containment());
    command
}

fn ask(harness: &Harness, text: &str, codex: bool) -> serde_json::Value {
    // Both hosts normalize shell calls to Bash/command. Codex also supplies
    // permission_mode; it must not alter this hook's classification.
    let mut payload = serde_json::json!({
        "hook_event_name": "PreToolUse", "tool_name": "Bash",
        "session_id": "isolated-guard-test", "cwd": harness.root,
        "tool_input": {"command": text}
    });
    if codex {
        payload["permission_mode"] = "default".into();
    }
    let mut command = shell(harness);
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/plugins/story/hooks/protect-install.sh"
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = ChildGuard::spawn_with_output(&mut command).unwrap();
    write!(child.stdin().unwrap(), "{payload}").unwrap();
    let result = child.wait_with_output_within(STORY_COMMAND_DEADLINE, || format!("hook: {text}"));
    assert!(result.status.success(), "{}", combined(&result));
    assert!(result.stderr.is_empty(), "{}", combined(&result));
    serde_json::from_slice(&result.stdout).expect("valid hook response")
}

fn assert_denied(harness: &Harness, text: &str) {
    for codex in [false, true] {
        let response = ask(harness, text, codex);
        assert_eq!(
            response["hookSpecificOutput"]["permissionDecision"], "deny",
            "{text}: {response}"
        );
    }
}

#[test]
fn installed_launcher_reader_grammar() {
    let harness = fixture();
    let launcher = quoted(&harness.codex_launcher());
    let provider_calls = harness.codex_log();
    harness.install_fake(
        "story",
        "#!/bin/sh\nprintf invoked > \"$HOME/unexpected-story-call\"\nexit 99\n",
    );
    for prefix in ["", "bash ", "/bin/bash ", "/usr/bin/bash "] {
        for selector in ["", "--project test ", "--project=test "] {
            for args in [
                "context",
                "context --full",
                "view TST-1",
                "view 1",
                "list",
                "capabilities",
                "capabilities --agent=claude",
                "capabilities --agent=codex",
                "ensure-cli",
            ] {
                let text = format!("{prefix}{launcher} {selector}{args}");
                for codex in [false, true] {
                    assert_eq!(ask(&harness, &text, codex), serde_json::json!({}), "{text}");
                }
            }
        }
    }
    assert_eq!(
        harness.codex_log(),
        provider_calls,
        "classification must not run the launcher or resolve a provider"
    );
    assert!(!harness.home.join("unexpected-story-call").exists());
}

#[test]
fn launcher_exception_rejects_mutation_and_ambiguous_shell_forms() {
    let harness = fixture();
    let launcher = quoted(&harness.codex_launcher());
    for args in [
        "",
        "create --title x",
        "dispatch TST-1",
        "sync",
        "handoff",
        "triage",
        "doctor",
        "capture TST-1",
        "reset TST-1",
        "reap TST-1",
        "notify TST-1 x",
        "complete execute TST-1",
        "unclaim TST-1",
        "scaffold-agents-md",
        "unknown",
        "context --unknown",
        "context --full --full",
        "context extra",
        "view",
        "view --help",
        "view TST-1 extra",
        "view ../TST-1",
        "list --ready",
        "capabilities --agent=other",
        "capabilities --agent=codex --agent=claude",
        "capabilities --agent codex",
        "ensure-cli --install",
        "--project",
        "--project= context",
        "--project a --project b context",
        "context --project a",
    ] {
        assert_denied(&harness, &format!("bash {launcher} {args}"));
    }
    for text in [
        format!("bash {launcher} context; rm {launcher}"),
        format!("bash {launcher} context && story new x"),
        format!("story new x && bash {launcher} context"),
        format!("bash {launcher} context | cat"),
        format!("bash {launcher} context > /tmp/output"),
        format!("bash {launcher} context 2>&1"),
        format!("bash {launcher} context < /dev/null"),
        format!("bash {launcher} context\nrm {launcher}"),
        format!("bash {launcher} context\r\n"),
        format!("bash {launcher} context # ignored"),
        format!("bash {launcher} context $(touch /tmp/unwanted)"),
        format!("bash {launcher} context `touch /tmp/unwanted`"),
        format!("bash {launcher} context <(cat /dev/null)"),
        format!("bash -c {launcher} context"),
        format!("bash -- {launcher} context"),
        format!("env bash {launcher} context"),
        format!("STORY_BIN=bad bash {launcher} context"),
        format!("python3 {launcher} context"),
        format!("source {launcher} context"),
        format!("bash {launcher} --project '$PROJECT' context"),
        format!("bash {launcher} --project '*' context"),
        format!("bash {launcher} --project '{{a,b}}' context"),
        format!("bash {launcher} --project '~' context"),
        format!("bash '{}.bak' context", harness.codex_launcher().display()),
        format!(
            "bash '{}/../storyhook/story.sh' context",
            harness.codex_launcher().parent().unwrap().display()
        ),
    ] {
        assert_denied(&harness, &text);
    }
}

#[test]
fn launcher_identity_requires_the_installer_bytes_and_no_symlink() {
    let harness = fixture();
    let path = harness.codex_launcher();
    let text = format!("bash {} context", quoted(&path));
    let original = fs::read(&path).unwrap();
    fs::write(
        &path,
        b"# storyhook-managed: codex-launcher-v1\ntouch /tmp/unwanted\n",
    )
    .unwrap();
    assert_denied(&harness, &text);
    fs::remove_file(&path).unwrap();
    assert_denied(&harness, &text);
    let other = harness.home.join("other.sh");
    fs::write(&other, original).unwrap();
    std::os::unix::fs::symlink(&other, &path).unwrap();
    assert_denied(&harness, &text);
    fs::remove_file(&path).unwrap();
    let mut fifo = shell(&harness);
    fifo.arg("-c").arg(format!("mkfifo {}", quoted(&path)));
    let output = run_bounded(fifo, "create isolated FIFO", STORY_COMMAND_DEADLINE);
    assert!(output.status.success(), "{}", combined(&output));
    assert_denied(&harness, &text);
    fs::remove_file(&path).unwrap();
    fs::copy(&other, &path).unwrap();
    let managed = path.parent().unwrap();
    let moved = harness.home.join("redirected-managed-directory");
    fs::rename(managed, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, managed).unwrap();
    assert_denied(&harness, &text);
}

#[test]
fn unknown_operation_is_not_misreported_as_an_artifact_edit() {
    let harness = fixture();
    let text = format!(
        "bash {} create --title x",
        quoted(&harness.codex_launcher())
    );
    let response = ask(&harness, &text, true);
    let reason = response["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap();
    assert!(reason.contains("cannot establish"), "{reason}");
    assert!(!reason.contains("refusing to edit"), "{reason}");
    assert!(reason.contains("CHECKOUT"), "{reason}");
}

#[test]
fn admitted_reads_execute_real_helpers_without_domain_or_artifact_writes() {
    let harness = fixture();
    // Only the provider installation boundary is simulated. Use the complete
    // shipped helper tree, real launcher, CLI and daemon for every read.
    let cache = harness
        .home
        .join(".codex/plugins/cache/storyhook/story")
        .join(env!("CARGO_PKG_VERSION"));
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/story");
    for (relative, (bytes, executable)) in regular_files(&source) {
        let path = cache.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }),
        )
        .unwrap();
    }
    let created = harness.run(&[
        "project",
        "new",
        "--name",
        "Guard fixture",
        "--prefix",
        "TST",
    ]);
    assert!(created.status.success(), "{}", combined(&created));
    let created = harness.run(&["new", "Guard reader sentinel"]);
    assert!(created.status.success(), "{}", combined(&created));
    let snapshot = || {
        let result = harness.run(&["show", "TST-1", "--json"]);
        assert!(result.status.success(), "{}", combined(&result));
        let view = serde_json::from_slice::<serde_json::Value>(&result.stdout).unwrap();
        let store = rusqlite::Connection::open_with_flags(
            harness.home.join("data/storyhook/store.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let events: (i64, i64) = store
            .query_row(
                "SELECT COUNT(*), COALESCE(MAX(global_seq), 0) FROM events",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        (view, events)
    };
    let before = snapshot();
    assert!(
        before.0["story"]["head_global_seq"].is_number() && before.1.0 > 0,
        "snapshot must include story and store-wide event positions"
    );
    let selected = harness.run(&["project", "show", "--json"]);
    assert!(selected.status.success(), "{}", combined(&selected));
    let selected: serde_json::Value = serde_json::from_slice(&selected.stdout).unwrap();
    let slug = selected["project"]["slug"].as_str().unwrap();
    let artifacts = regular_files(&harness.home.join(".codex"));
    let launcher = quoted(&harness.codex_launcher());
    // Linux may provide /usr/bin/bash; macOS provides only /bin/bash.
    // Grammar coverage above still checks every supported spelling.
    let prefixes: Vec<_> = ["", "bash ", "/bin/bash ", "/usr/bin/bash "]
        .into_iter()
        .filter(|prefix| !prefix.starts_with('/') || Path::new(prefix.trim()).is_file())
        .collect();
    for (index, args) in [
        "context",
        "context --full",
        "view TST-1",
        "list",
        "capabilities",
        "capabilities --agent=claude",
        "capabilities --agent=codex",
        "ensure-cli",
    ]
    .into_iter()
    .enumerate()
    {
        let prefix = prefixes[index % prefixes.len()];
        let selector = match index % 3 {
            0 => String::new(),
            1 => format!("--project {slug} "),
            _ => format!("--project={slug} "),
        };
        let text = format!("{prefix}{launcher} {selector}{args}");
        assert_eq!(ask(&harness, &text, true), serde_json::json!({}), "{text}");
        let mut command = shell(&harness);
        command.args(["-c", &text]);
        let output = run_bounded(command, "real launcher reader", STORY_COMMAND_DEADLINE);
        assert!(output.status.success(), "{text}: {}", combined(&output));
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["ok"], true, "{text}: {value}");
        if args.starts_with("capabilities") {
            assert!(value["models"].as_array().unwrap().len() > 1);
        } else if args == "ensure-cli" {
            assert_eq!(value["installed"], true);
        } else {
            assert!(
                value["display"]
                    .as_str()
                    .unwrap()
                    .contains("Guard reader sentinel"),
                "{value}"
            );
        }
        assert_eq!(
            snapshot(),
            before,
            "reader changed domain state/events: {text}"
        );
        assert_eq!(
            regular_files(&harness.home.join(".codex")),
            artifacts,
            "reader changed installed artifacts: {text}"
        );
    }
}
