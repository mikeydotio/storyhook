//! Launchd-shaped execution without registering anything with launchd (SH-819).

// Other Unix system directories can contain tools absent from macOS's default
// launchd PATH, so they cannot provide this negative control.
#![cfg(target_os = "macos")]

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use storyhook::daemon::{agent, lifecycle};
use storyhook_test_support::{ChildGuard, TestEnv, story_binary};

const MINIMAL_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

fn executable(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn tool_path(env: &TestEnv) -> String {
    format!(
        "{}:{}:{MINIMAL_PATH}",
        env.home().join("tools & links").display(),
        story_binary().parent().unwrap().display()
    )
}

fn generated_plist(env: &TestEnv, path: &str) -> String {
    agent::plist(
        story_binary(),
        &env.environment(),
        &agent::ExecutionPath::parse(Some(OsStr::new(path))).unwrap(),
    )
}

fn start(env: &TestEnv, plist: &str, apply_path: bool) -> ChildGuard {
    env.stop_daemon();
    let args = agent::registered_args(plist).unwrap();
    let log = fs::File::create(env.home().join("serve.log")).unwrap();
    let mut cmd = Command::new(&args[0]);
    cmd.env_clear();
    env.apply(&mut cmd);
    cmd.args(&args[1..])
        .env("PATH", MINIMAL_PATH)
        .env("STORYHOOK_ALLOW_UNINSTALLED_PLUGIN_INSTALL", "1")
        .current_dir(env.home())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    if apply_path {
        cmd.env(
            "PATH",
            agent::registered_path(plist).unwrap().unwrap().as_str(),
        );
    }
    let child = ChildGuard::spawn(&mut cmd).unwrap();
    storyhook_test_support::load_grace::wait_for(
        storyhook_test_support::load_grace::Patience::new(Duration::from_secs(10)),
        Duration::from_millis(25),
        || fs::read_to_string(env.home().join("serve.log")).unwrap(),
        || lifecycle::read_info(&env.environment()),
    );
    child
}

fn doctor(env: &TestEnv) -> String {
    doctor_on_path(env, &tool_path(env))
}

fn doctor_on_path(env: &TestEnv, caller_path: &str) -> String {
    let output = env
        .story(env.home())
        .env("PATH", caller_path)
        .args(["doctor", "install"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn provider(env: &TestEnv, name: &str) -> String {
    let output = env
        .story(env.home())
        .args(["plugin", "install", name])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "the fixture deliberately refuses the provider write"
    );
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn fake_tools(env: &TestEnv) {
    let bin = env.home().join("tools & links");
    fs::create_dir_all(env.home().join(".claude")).unwrap();
    for tool in ["claude", "codex"] {
        executable(
            &bin.join(tool),
            &format!(
                "#!/usr/bin/env node\nprintf '%s\\n' '{tool}' >> \"$HOME/providers-seen\"\nif [ \"$1\" = --version ]; then echo 'fixture 1.0'; exit 0; fi\necho 'SH819 provider reached {tool}' >&2\nexit 73\n"
            ),
        );
    }
    // The external interpreter is itself a double. Finding it still exercises
    // the kernel -> /usr/bin/env -> PATH boundary used by Node-based CLIs.
    executable(
        &bin.join("node"),
        "#!/bin/sh\nprintf 'node\\n' >> \"$HOME/tools-seen\"\nexec /bin/sh \"$@\"\n",
    );
    for tool in ["cargo", "gh", "tmux"] {
        executable(
            &bin.join(tool),
            &format!("#!/bin/sh\nprintf '{tool}\\n' >> \"$HOME/tools-seen\"\n"),
        );
    }
}

#[test]
fn launchd_path_reaches_providers_and_the_gate_spawn_boundary() {
    let env = TestEnv::isolated();
    fake_tools(&env);
    let project = env.project().build();
    let helper = std::env::current_exe().unwrap();
    let pointer = project.path().join(".storyhook.toml");
    let before = fs::read_to_string(&pointer).unwrap();
    let hook = format!(
        "'{}' --ignored --exact execution_path_gate_worker --nocapture",
        helper.display().to_string().replace('\'', "'\\''")
    );
    fs::write(
        &pointer,
        format!(
            "{before}\n[hooks.on_create]\ncommand = {}\ntimeout_seconds = 30\n",
            serde_json::to_string(&hook).unwrap()
        ),
    )
    .unwrap();
    let plist = generated_plist(&env, &tool_path(&env));
    let _daemon = start(&env, &plist, true);
    let info = serde_json::to_value(env.daemon().unwrap()).unwrap();
    assert_eq!(info["execution_path"], tool_path(&env));
    for name in ["claude", "codex"] {
        assert!(provider(&env, name).contains(&format!("SH819 provider reached {name}")));
    }
    env.story(project.path())
        .args(["new", "exercise gate environment"])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(env.home().join("gate-complete")).unwrap(),
        "gate tools executed"
    );
    let seen = fs::read_to_string(env.home().join("tools-seen")).unwrap();
    for name in ["node", "cargo", "gh", "tmux"] {
        assert!(seen.lines().any(|line| line == name), "{name}: {seen}");
    }
}

#[test]
fn a_minimal_daemon_cannot_use_tools_from_the_doctor_callers_path() {
    let env = TestEnv::isolated();
    fake_tools(&env);
    let plist = generated_plist(&env, &tool_path(&env));
    let _daemon = start(&env, &plist, false);
    for name in ["claude", "codex"] {
        assert!(provider(&env, name).contains("not found on PATH"));
    }
    assert!(!env.home().join("providers-seen").exists());
    let report = doctor(&env);
    assert!(report.contains("daemon PATH"), "{report}");
    assert!(report.contains(MINIMAL_PATH), "{report}");
    assert!(report.contains("cannot find `cargo`"), "{report}");
    assert!(report.contains("cannot find `codex`"), "{report}");
}

#[test]
fn doctor_detects_removed_tools_and_a_path_changed_since_start() {
    let env = TestEnv::isolated();
    fake_tools(&env);
    let path = tool_path(&env);
    let plist = generated_plist(&env, &path);
    let installed = agent::path(&env.environment());
    fs::create_dir_all(installed.parent().unwrap()).unwrap();
    fs::write(&installed, &plist).unwrap();
    let _daemon = start(&env, &plist, true);
    let seen = fs::read(env.home().join("tools-seen")).ok();
    let report = doctor(&env);
    assert!(!report.contains("cannot find `tmux`"), "{report}");
    assert_eq!(
        fs::read(env.home().join("tools-seen")).ok(),
        seen,
        "Doctor must not execute tools"
    );
    assert!(
        !env.home().join("providers-seen").exists(),
        "Doctor must not execute providers"
    );
    fs::remove_file(env.home().join("tools & links/tmux")).unwrap();
    let caller_bin = env.home().join("caller-only");
    executable(&caller_bin.join("tmux"), "#!/bin/sh\nexit 0\n");
    fs::write(
        &installed,
        generated_plist(&env, "/new/tool/path:/usr/bin:/bin"),
    )
    .unwrap();
    let report = doctor_on_path(&env, &format!("{}:{path}", caller_bin.display()));
    assert!(report.contains("cannot find `tmux`"), "{report}");
    assert!(
        report.contains("differs from the running daemon"),
        "{report}"
    );
    assert!(report.contains(&path), "{report}");
}

#[test]
fn doctor_reports_legacy_metadata_as_unknown_without_borrowing_the_callers_path() {
    let env = TestEnv::isolated();
    fake_tools(&env);
    let _daemon = start(&env, &generated_plist(&env, &tool_path(&env)), true);
    let file = env.environment().daemon_file();
    let mut info: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    info.as_object_mut().unwrap().remove("execution_path");
    fs::write(&file, serde_json::to_vec(&info).unwrap()).unwrap();
    let report = doctor(&env);
    assert!(report.contains("daemon did not publish PATH"), "{report}");
    assert!(!report.contains("daemon cargo"), "{report}");
    let parsed: lifecycle::DaemonInfo = serde_json::from_value(info).unwrap();
    assert_eq!(parsed.execution_path, None);
    assert!(
        serde_json::to_value(parsed)
            .unwrap()
            .get("execution_path")
            .is_none()
    );
}

#[test]
fn doctor_keeps_other_rows_when_daemon_observation_fails() {
    let env = TestEnv::isolated();
    let _lock = lifecycle::claim_pidfile(&env.environment()).unwrap();
    let report = doctor(&env);
    assert!(report.contains("daemon PATH"), "{report}");
    assert!(report.contains("no readable portfile"), "{report}");
    assert!(
        report.contains("store") && report.contains("edit guard"),
        "{report}"
    );
    assert!(!report.contains("daemon cargo"), "{report}");
}

/// Runs as an actual descendant of the fixture daemon, through its user hook.
/// The production actuator supplies the verifier environment and runs the gate
/// fixture. Only the external tools and verifier response are fixture data.
#[test]
#[ignore = "only invoked by the isolated daemon's event hook"]
fn execution_path_gate_worker() {
    use storyhook::daemon::verification::{
        ShellVerificationActuator, VerificationActuator, VerificationOutcome,
    };
    use storyhook::domain::Priority;
    use storyhook::service::verification::{VerificationCandidate, VerificationProblem};
    use storyhook::store::PrLink;
    use storyhook_test_support::{FIXTURE_NOW, ServiceFixture};

    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let checkout = home.join("gate-checkout");
    fs::create_dir_all(&checkout).unwrap();
    for args in [
        vec!["init", "-q"],
        vec![
            "config",
            "remote.origin.url",
            "https://github.com/acme/widgets.git",
        ],
    ] {
        assert!(
            storyhook::env::git_env::command(&checkout)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let gate = home.join("fixture-gate.sh");
    fs::write(&gate, "set -e\ncargo --version\ngh --version\ntmux -V\nprintf '{\"result\":\"certified\",\"gate\":\"cargo --version\",\"head\":\"abc\",\"tree\":\"def\",\"detail\":\"fixture gate\"}\\n'\n").unwrap();
    let fixture = ServiceFixture::new();
    let candidate = VerificationCandidate {
        blocked_by: Vec::new(),
        landing_pending: false,
        project: fixture.project(),
        project_slug: "fixture".into(),
        story_id: "SH-1".into(),
        title: "PATH".into(),
        priority: Priority::High,
        created_at: FIXTURE_NOW.into(),
        verifying_since: Some(FIXTURE_NOW.into()),
        verifying_generation: None,
        blocking_revision: None,
        human_only_revision: None,
        checkout,
        cleanup_lease: None,
        pull_request: Err(VerificationProblem::MissingPullRequest),
    };
    let link = PrLink {
        owner: "acme".into(),
        repo: "widgets".into(),
        number: 1,
        url: "https://github.com/acme/widgets/pull/1".into(),
        close_on_merge: true,
        status: "open".into(),
        linked_at: FIXTURE_NOW.into(),
        last_checked_at: None,
    };
    let actuator = ShellVerificationActuator::with_paths(
        fixture.env().clone(),
        home.join("unused-helper"),
        story_binary().to_path_buf(),
    )
    .with_verifier_script(gate);
    let result = actuator.verify(&candidate, &link);
    assert!(
        matches!(result, VerificationOutcome::Certified { .. }),
        "{result:?}"
    );
    fs::write(home.join("gate-complete"), "gate tools executed").unwrap();
}
