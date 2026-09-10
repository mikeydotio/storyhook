//! Classify installer-produced launchers, then execute real readers and dispatch.

use super::*;
use std::io::Write;
use std::process::Stdio;
use storyhook_test_support::{ChildGuard, STORY_COMMAND_DEADLINE, run_bounded};

pub(super) fn fixture() -> Harness {
    let mut harness = Harness::new(false);
    harness.home = harness._temp.path().join("home with spaces");
    fs::create_dir_all(&harness.home).unwrap();
    harness.install_fake("codex", FAKE_CODEX);
    harness.install_story_on_path();
    let installed = harness.run(&["plugin", "install", "codex"]);
    assert!(installed.status.success(), "{}", combined(&installed));
    harness
}

pub(super) fn quoted(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

pub(super) fn shell(harness: &Harness) -> Command {
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

/// The tracked hook, never a copy.
pub(super) fn tracked_hook() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/plugins/story/hooks/protect-install.sh"
    ))
}

fn ask(harness: &Harness, text: &str, codex: bool) -> serde_json::Value {
    ask_hook(harness, &tracked_hook(), text, codex)
}

/// One hook decision, from the hook at `hook` — the tracked one for the
/// launcher door, an installed copy for the helper door, whose identity IS
/// where the hook was loaded from (SH-632).
pub(super) fn ask_hook(
    harness: &Harness,
    hook: &Path,
    text: &str,
    codex: bool,
) -> serde_json::Value {
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
        .arg(hook)
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
    assert_denied_by(harness, &tracked_hook(), text);
}

pub(super) fn assert_denied_by(harness: &Harness, hook: &Path, text: &str) {
    for codex in [false, true] {
        let response = ask_hook(harness, hook, text, codex);
        assert_eq!(
            response["hookSpecificOutput"]["permissionDecision"], "deny",
            "{text}: {response}"
        );
    }
}

/// Every spelling of the interpreter the router may put in front of an entry
/// point, including none: an installed helper is executable in its own right.
pub(super) const INTERPRETER_PREFIXES: [&str; 4] = ["", "bash ", "/bin/bash ", "/usr/bin/bash "];

/// The project selector forms the helper accepts ahead of its verb.
pub(super) const PROJECT_SELECTORS: [&str; 3] = ["", "--project test ", "--project=test "];

/// Reader verbs: nothing after them touches a story, a worktree, or a file.
pub(super) const ADMITTED_READER_ARGS: [&str; 9] = [
    "context",
    "context --full",
    "view TST-1",
    "view 1",
    "list",
    "capabilities",
    "capabilities --agent=claude",
    "capabilities --agent=codex",
    "ensure-cli",
];

/// Dispatch forms the argv contract admits (SH-588): one target, the helper's
/// own provider/model/effort/speed/mode flags, no managed-file operand.
pub(super) const ADMITTED_DISPATCH_ARGS: [&str; 8] = [
    "dispatch TST-1",
    "dispatch TST-1 --agent=codex",
    "dispatch --agent=claude TST-1",
    "dispatch TST-1 --auto --resume",
    "dispatch --next --auto --agent=codex",
    "dispatch TST-1 --force",
    "dispatch TST-1 --auto --full-auto",
    "dispatch TST-1 --agent=codex --model=gpt-6-astra --effort=high --speed=fast",
];

/// Argument lists no entry point may be admitted with: mutating verbs, unknown
/// verbs, and malformed selectors or reader options.
pub(super) const REJECTED_ARGS: [&str; 30] = [
    "",
    "create --title x",
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
];

/// Dispatch argument lists outside the contract.
pub(super) const REJECTED_DISPATCH_ARGS: [&str; 20] = [
    "dispatch",
    "dispatch --help",
    "dispatch ../TST-1",
    "dispatch TST-1 TST-2",
    "dispatch TST-1 --unknown",
    "dispatch TST-1 --agent other",
    "dispatch TST-1 --agent=other",
    "dispatch TST-1 --agent=codex --agent=claude",
    "dispatch TST-1 --model=",
    "dispatch TST-1 --effort=",
    "dispatch TST-1 --speed=slow",
    "dispatch TST-1 --auto --auto",
    "dispatch TST-1 --force --resume",
    "dispatch TST-1 --full-auto",
    "dispatch TST-1 --next",
    "dispatch --next --next",
    "dispatch --next --force",
    "dispatch --next --resume",
    "dispatch --next --auto --full-auto",
    "dispatch TST-1 --project other",
];

/// Shell compositions around an otherwise-admitted reader call on `entry`:
/// chaining, redirection, substitution, a second interpreter, an environment
/// prefix, a lookalike path. None may be admitted, whichever door `entry` is.
pub(super) fn rejected_compositions(entry: &Path) -> Vec<String> {
    let e = quoted(entry);
    vec![
        format!("bash {e} context; rm {e}"),
        format!("bash {e} context && story new x"),
        format!("story new x && bash {e} context"),
        format!("bash {e} context | cat"),
        format!("bash {e} context > /tmp/output"),
        format!("bash {e} context 2>&1"),
        format!("bash {e} context < /dev/null"),
        format!("bash {e} context\nrm {e}"),
        format!("bash {e} context\r\n"),
        format!("bash {e} context # ignored"),
        format!("bash {e} context $(touch /tmp/unwanted)"),
        format!("bash {e} context `touch /tmp/unwanted`"),
        format!("bash {e} context <(cat /dev/null)"),
        format!("bash -c {e} context"),
        format!("bash -- {e} context"),
        format!("env bash {e} context"),
        format!("STORY_BIN=bad bash {e} context"),
        format!("python3 {e} context"),
        format!("source {e} context"),
        format!("bash {e} --project '$PROJECT' context"),
        format!("bash {e} --project '*' context"),
        format!("bash {e} --project '{{a,b}}' context"),
        format!("bash {e} --project '~' context"),
        format!("bash '{}.bak' context", entry.display()),
    ]
}

/// The same for a dispatch call: composition, a managed path smuggled in as an
/// operand, a second interpreter, an environment prefix.
pub(super) fn rejected_dispatch_compositions(entry: &Path) -> Vec<String> {
    let e = quoted(entry);
    vec![
        format!("bash {e} dispatch TST-1; rm {e}"),
        format!("bash {e} dispatch TST-1 && story new x"),
        format!("bash {e} dispatch TST-1\nrm {e}"),
        format!("bash {e} dispatch TST-1 > {e}"),
        format!("bash {e} dispatch TST-1 | cat"),
        format!("bash {e} dispatch '$(touch /tmp/unwanted)'"),
        format!("bash {e} --project {e} dispatch TST-1"),
        format!("bash {e} dispatch TST-1 --model={e}"),
        format!("bash -c {e} dispatch TST-1"),
        format!("STORY_LAUNCH_CMD=bad bash {e} dispatch TST-1"),
    ]
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
    for prefix in INTERPRETER_PREFIXES {
        for selector in PROJECT_SELECTORS {
            for args in ADMITTED_READER_ARGS {
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
    for args in REJECTED_ARGS {
        assert_denied(&harness, &format!("bash {launcher} {args}"));
    }
    for text in rejected_compositions(&harness.codex_launcher()) {
        assert_denied(&harness, &text);
    }
    assert_denied(
        &harness,
        &format!(
            "bash '{}/../storyhook/story.sh' context",
            harness.codex_launcher().parent().unwrap().display()
        ),
    );
}

#[test]
fn installed_launcher_dispatch_is_not_an_installed_artifact_edit() {
    let harness = fixture();
    let launcher = quoted(&harness.codex_launcher());
    let provider_calls = harness.codex_log();
    harness.install_fake(
        "story",
        "#!/bin/sh\nprintf invoked > \"$HOME/unexpected-story-call\"\nexit 99\n",
    );
    for prefix in INTERPRETER_PREFIXES {
        for selector in PROJECT_SELECTORS {
            for args in ADMITTED_DISPATCH_ARGS {
                let text = format!("{prefix}{launcher} {selector}{args}");
                for codex in [false, true] {
                    assert_eq!(ask(&harness, &text, codex), serde_json::json!({}), "{text}");
                }
            }
        }
    }
    assert_eq!(harness.codex_log(), provider_calls);
    assert!(!harness.home.join("unexpected-story-call").exists());
}

#[test]
fn dispatch_exception_preserves_argument_shell_and_identity_guards() {
    let harness = fixture();
    let launcher = quoted(&harness.codex_launcher());
    for args in REJECTED_DISPATCH_ARGS {
        assert_denied(&harness, &format!("bash {launcher} {args}"));
    }
    for text in rejected_dispatch_compositions(&harness.codex_launcher()) {
        assert_denied(&harness, &text);
    }
    fs::write(harness.codex_launcher(), "exec arbitrary-program\n").unwrap();
    assert_denied(
        &harness,
        &format!("bash {launcher} dispatch TST-1 --agent=codex"),
    );
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
    assert!(
        reason.contains("preserves installed release artifacts"),
        "{reason}"
    );
    assert!(!reason.contains("refusing to edit"), "{reason}");
    assert!(reason.contains("CHECKOUT"), "{reason}");
}

#[test]
fn admitted_reads_execute_real_helpers_without_domain_or_artifact_writes() {
    let harness = fixture();
    // Only the provider installation boundary is simulated. Use the complete
    // shipped helper tree, real launcher, CLI and daemon for every read.
    install_checkout_helpers(&harness);
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
    let prefixes: Vec<_> = INTERPRETER_PREFIXES
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

/// Replace the fixture release payload with this checkout's actual helper tree.
fn install_checkout_helpers(harness: &Harness) -> PathBuf {
    let cache = harness
        .home
        .join(".codex/plugins/cache/storyhook/story")
        .join(env!("CARGO_PKG_VERSION"));
    install_checkout_helpers_at(&cache);
    cache
}

/// Write this checkout's actual plugin tree at `root`, byte for byte and mode
/// for mode — what a provider's plugin install leaves behind, minus the
/// provider.
pub(super) fn install_checkout_helpers_at(root: &Path) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/story");
    for (relative, (bytes, executable)) in regular_files(&source) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }),
        )
        .unwrap();
    }
}

#[test]
fn admitted_dispatch_claims_and_creates_worktree_without_editing_installed_artifacts() {
    let harness = fixture();
    let cache = install_checkout_helpers(&harness);
    let artifacts = regular_files(&harness.home.join(".codex"));
    let text = format!(
        "bash {} dispatch TST-1 --agent=codex",
        quoted(&harness.codex_launcher())
    );
    for codex in [false, true] {
        assert_eq!(ask(&harness, &text, codex), serde_json::json!({}), "{text}");
    }
    // The existing terminal double models only the provider boundary. The
    // launcher, helper, CLI, daemon, Git worktree and story claim are real.
    let mut command = shell(&harness);
    command
        .args([
            "-c",
            r#"
source "$2/plugins/story/tests/lib.sh"
repo=$(mk_story_repo)
id=$(new_story "$repo" "Installed launcher dispatch")
cd "$repo" || exit 1
export PATH="$TESTS_DIR/fakes:$PATH" TMUX=fake TMUX_PANE=%0
export STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0
export STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0
export FAKE_TMUX_CAPTURE=marker FAKE_TMUX_CODEX_SENTINEL_MODE=identity
out=$(bash "$1" dispatch "$id" --agent=codex)
tmux kill-window -t "$id"
assert_eq "$(jqf "$out" .ok)" true "installed dispatch succeeds"
assert_eq "$(jqf "$out" .claimed)" true "installed dispatch claims"
assert_eq "$(jqf "$out" .prompt_confirmed)" true "prompt was delivered"
assert_eq "$(jqf "$out" .plan_mode_confirmed)" true "provider entered Plan mode"
[ -d "$repo/.codex/worktrees/$id" ] || fail_test "missing worktree"
git show-ref --verify --quiet "refs/heads/worktree-$id" || fail_test "missing branch"
assert_eq "$(story show "$id" --json | jq -r '.story.story.state')" in-progress "persisted claim"
printf '%s\n' "$out"
"#,
            "installed-dispatch-test",
        ])
        .arg(harness.codex_launcher())
        .arg(env!("CARGO_MANIFEST_DIR"))
        .env("STORYHOOK_TEST_HOME", &harness.home)
        .env("FAKE_TMUX_CODEX_PLUGIN_ROOT", cache);
    let output = run_bounded(command, "real installed dispatch", STORY_COMMAND_DEADLINE);
    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(
        regular_files(&harness.home.join(".codex")),
        artifacts,
        "dispatch changed installed artifacts"
    );
}
