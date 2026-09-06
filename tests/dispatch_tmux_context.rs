//! SH-584: a daemon's inherited tmux server must not select web dispatch's server.
//!
//! A shell probe observes the production dispatch boundary: the real daemon,
//! spawn allowlist, HTTP lifecycle, and two real tmux servers remain in play.
//! Worktree creation and provider handoff belong to the full helper tests.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use storyhook::daemon::lifecycle::SPAWN_DEADLINE;
use storyhook_test_support::{ChildGuard, STORY_COMMAND_DEADLINE, TestEnv, scratch_dir};

// Let production startup report its own timeout before the harness does.
const STARTUP_DEADLINE: Duration = SPAWN_DEADLINE.saturating_mul(2);

fn tmux(socket: &Path, args: &[&str]) -> String {
    let mut command = Command::new("tmux");
    command.env_remove("TMUX").env_remove("TMUX_PANE");
    command.arg("-S").arg(socket).args(args);
    let output = ChildGuard::spawn_with_output(&mut command)
        .expect("start fixture tmux client")
        .wait_with_output_within(STORY_COMMAND_DEADLINE, || {
            "fixture tmux client did not finish".into()
        });
    assert!(
        output.status.success(),
        "tmux failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("tmux returns UTF-8")
}

fn server(socket: &Path) -> ChildGuard {
    let mut command = Command::new("tmux");
    command
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .args(["-D", "-f", "/dev/null", "-S"])
        .arg(socket)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    let mut guard = ChildGuard::spawn(&mut command).expect("start foreground fixture server");
    let deadline = Instant::now() + STARTUP_DEADLINE;
    while !socket.exists() {
        assert!(
            guard.try_wait().is_none(),
            "fixture tmux server exited before creating its socket"
        );
        assert!(
            Instant::now() < deadline,
            "fixture tmux socket did not appear"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    tmux(socket, &["new-session", "-d", "-s", "fixture", "/bin/sh"]);
    guard
}

#[test]
fn web_dispatch_uses_default_server_despite_daemons_unrelated_tmux_context() {
    let env = TestEnv::isolated();
    let scratch = scratch_dir();
    let tmux_root = scratch.path().join("sockets");
    let uid = scratch
        .path()
        .metadata()
        .expect("fixture directory exists")
        .uid();
    let socket_dir = tmux_root.join(format!("tmux-{uid}"));
    std::fs::create_dir_all(&socket_dir).expect("create isolated tmux socket directory");
    std::fs::set_permissions(&socket_dir, std::fs::Permissions::from_mode(0o700))
        .expect("tmux requires a private socket directory for implicit targeting");
    let default_socket = socket_dir.join("default");
    let unrelated_socket = socket_dir.join("unrelated");
    let _default_server = server(&default_socket);
    let _unrelated_server = server(&unrelated_socket);
    let inherited_tmux = tmux(
        &unrelated_socket,
        &["display-message", "-p", "#{socket_path},#{pid},0"],
    );
    let inherited_pane = tmux(&unrelated_socket, &["display-message", "-p", "#{pane_id}"]);
    let helper = scratch.path().join("probe.sh");
    // Set the fixture namespace inside the helper because TMUX_TMPDIR is not
    // part of the production allowlist. Every implicit tmux target stays isolated.
    std::fs::write(
        &helper,
        format!(
            r#"#!/usr/bin/env bash
DISPATCH_PROTOCOL=4
set -eu
export TMUX_TMPDIR='{}'
socket=$(tmux display-message -p '#{{socket_path}}')
printf '{{"ok":true,"socket":"%s","argv":"%s"}}\n' "$socket" "$*"
"#,
            tmux_root.display()
        ),
    )
    .expect("write the boundary probe");

    let mut interactive = Command::new("bash");
    interactive
        .arg(&helper)
        .env("TMUX", inherited_tmux.trim())
        .env("TMUX_PANE", inherited_pane.trim());
    let output = ChildGuard::spawn_with_output(&mut interactive)
        .expect("invoke the helper directly with interactive context")
        .wait_with_output_within(STORY_COMMAND_DEADLINE, || {
            "interactive probe did not finish".into()
        });
    assert!(
        output.status.success(),
        "interactive probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let direct: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("interactive probe JSON");
    assert_eq!(
        direct["socket"],
        unrelated_socket.to_string_lossy().as_ref(),
        "direct helper invocation must retain the interactive caller's server"
    );

    let mut serve = env.raw_story(scratch.path());
    serve
        .args(["daemon", "--serve", "--port", "0"])
        .env("TMUX", inherited_tmux.trim())
        .env("TMUX_PANE", inherited_pane.trim())
        .env("STORYHOOK_DISPATCH_SCRIPT", &helper)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut daemon =
        ChildGuard::spawn(&mut serve).expect("start daemon with unrelated tmux context");
    let deadline = Instant::now() + STARTUP_DEADLINE;
    let info = loop {
        if let Some(info) = env.daemon() {
            break info;
        }
        assert!(
            daemon.try_wait().is_none(),
            "fixture daemon exited before publishing its port"
        );
        assert!(
            Instant::now() < deadline,
            "fixture daemon did not publish its port"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    let url = format!(
        "http://127.0.0.1:{}/api/repos/fixture/story/SH-1/dispatch",
        info.port
    );
    for (agent, auto) in [
        ("claude", false),
        ("claude", true),
        ("codex", false),
        ("codex", true),
    ] {
        let query = format!("agent={agent}{}", if auto { "&auto=1" } else { "" });
        let accepted: serde_json::Value = ureq::post(format!("{url}?{query}"))
            .header("X-Storyhook", "1")
            .header("Host", "127.0.0.1")
            .header("X-Storyhook-Token", &info.token)
            .send_empty()
            .expect("dispatch accepted")
            .into_body()
            .read_json()
            .expect("dispatch acceptance JSON");
        let handle = accepted["dispatch"]["handle"]
            .as_str()
            .expect("dispatch handle");
        let deadline = Instant::now() + STORY_COMMAND_DEADLINE.saturating_mul(2);
        let record = loop {
            let response: serde_json::Value = ureq::get(format!("{url}/{handle}"))
                .header("X-Storyhook", "1")
                .header("Host", "127.0.0.1")
                .header("X-Storyhook-Token", &info.token)
                .call()
                .expect("read dispatch outcome")
                .into_body()
                .read_json()
                .expect("dispatch outcome JSON");
            if response["dispatch"]["state"] != "running" {
                break response["dispatch"].clone();
            }
            assert!(
                Instant::now() < deadline,
                "dispatch never finished: {response}"
            );
            std::thread::sleep(Duration::from_millis(25));
        };
        assert_eq!(
            record["state"], "ok",
            "probe must execute successfully: {record}"
        );
        assert_eq!(
            record["payload"]["socket"],
            default_socket.to_string_lossy().as_ref(),
            "web dispatch must use the default server, independent of the daemon's inherited TMUX; unrelated server was {}",
            unrelated_socket.display()
        );
        let argv = record["payload"]["argv"].as_str().expect("probe argv");
        assert!(
            argv.contains(&format!("--agent={agent}")),
            "provider option must reach helper: {argv}"
        );
        assert_eq!(
            argv.split_whitespace().any(|arg| arg == "--auto"),
            auto,
            "auto option must reach helper: {argv}"
        );
    }
}

#[test]
fn engine_monitoring_and_stop_use_default_server_with_overlapping_window_ids() {
    use storyhook::service::engine::{Dispatcher, ShellDispatcher};

    const RESULT_ENV: &str = "STORY_ENGINE_TMUX_RESULT";
    if let Some(result_path) = std::env::var_os(RESULT_ENV) {
        // Process-local environment poisoning cannot race sibling Rust tests.
        let env = TestEnv::isolated();
        let dispatcher = ShellDispatcher::new("unused-helper", env.environment());
        let alive = dispatcher.window_alive("@1");
        let stopped = dispatcher.kill_window("@1");
        std::fs::write(
            result_path,
            serde_json::json!({
                "alive": alive,
                "stop_error": stopped.err().map(|error| error.to_string()),
            })
            .to_string(),
        )
        .expect("record real engine observations");
        return;
    }

    let scratch = scratch_dir();
    let tmux_root = scratch.path().join("sockets");
    let uid = scratch
        .path()
        .metadata()
        .expect("fixture directory exists")
        .uid();
    let socket_dir = tmux_root.join(format!("tmux-{uid}"));
    std::fs::create_dir_all(&socket_dir).expect("create isolated socket directory");
    std::fs::set_permissions(&socket_dir, std::fs::Permissions::from_mode(0o700))
        .expect("private socket directory");
    let default_socket = socket_dir.join("default");
    let unrelated_socket = socket_dir.join("unrelated");
    let _default_server = server(&default_socket);
    let _unrelated_server = server(&unrelated_socket);
    let target = tmux(
        &default_socket,
        &["new-window", "-d", "-P", "-F", "#{window_id}", "/bin/sh"],
    );
    let other_target = tmux(
        &unrelated_socket,
        &["new-window", "-d", "-P", "-F", "#{window_id}", "/bin/cat"],
    );
    assert_eq!(
        target.trim(),
        "@1",
        "fixture needs deterministic overlapping IDs"
    );
    assert_eq!(
        target, other_target,
        "two servers must carry the same window ID"
    );
    let inherited_tmux = tmux(
        &unrelated_socket,
        &["display-message", "-p", "#{socket_path},#{pid},0"],
    );
    let inherited_pane = tmux(&unrelated_socket, &["display-message", "-p", "#{pane_id}"]);

    let default_process = tmux(
        &default_socket,
        &[
            "display-message",
            "-p",
            "-t",
            "@1",
            "#{pane_current_command}",
        ],
    );
    assert!(
        !default_process.trim().is_empty(),
        "default fixture process identity"
    );
    assert_ne!(
        default_process.trim(),
        "cat",
        "servers need distinguishable occupants"
    );

    // The adapter changes only tmux's fixture namespace. Real tmux executes
    // every command, including the potentially destructive kill-window call.
    let real_tmux = std::env::split_paths(&std::env::var_os("PATH").expect("PATH"))
        .map(|directory| directory.join("tmux"))
        .find(|path| path.is_file())
        .expect("tmux installed");
    let bin = scratch.path().join("bin");
    std::fs::create_dir(&bin).expect("fixture executable directory");
    let adapter = bin.join("tmux");
    std::fs::write(
        &adapter,
        format!(
            "#!/bin/sh\nexport TMUX_TMPDIR='{}'\nexec '{}' \"$@\"\n",
            tmux_root.display(),
            real_tmux.display()
        ),
    )
    .expect("write isolated tmux namespace adapter");
    std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o755))
        .expect("executable adapter");
    let result_path = scratch.path().join("observed.json");
    let mut command = Command::new(std::env::current_exe().expect("this test executable"));
    command
        .args([
            "--exact",
            "engine_monitoring_and_stop_use_default_server_with_overlapping_window_ids",
            "--nocapture",
        ])
        .env(RESULT_ENV, &result_path)
        .env("TMUX", inherited_tmux.trim())
        .env("TMUX_PANE", inherited_pane.trim())
        .env(
            "STORY_READY_PROCESS_PATTERN",
            format!("^{}$", regex::escape(default_process.trim())),
        )
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").expect("PATH")),
        );
    let output = ChildGuard::spawn_with_output(&mut command)
        .expect("isolated engine subprocess")
        .wait_with_output_within(STORY_COMMAND_DEADLINE, || {
            "engine probe did not finish".into()
        });
    assert!(
        output.status.success(),
        "engine probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let observed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(result_path).expect("engine result file"))
            .expect("engine result JSON");
    let default_windows = tmux(
        &default_socket,
        &["list-windows", "-a", "-F", "#{window_id}"],
    );
    let unrelated_windows = tmux(
        &unrelated_socket,
        &["list-windows", "-a", "-F", "#{window_id}"],
    );
    assert!(
        observed["alive"] == true
            && observed["stop_error"].is_null()
            && !default_windows.lines().any(|window| window == "@1")
            && unrelated_windows.lines().any(|window| window == "@1"),
        "engine must monitor and kill default @1 while preserving unrelated @1: observed={observed}, default={default_windows:?}, unrelated={unrelated_windows:?}"
    );
}

#[test]
fn verification_callback_delivers_only_to_the_default_server_agent() {
    use storyhook::daemon::verification::{ShellVerificationActuator, VerificationActuator};
    use storyhook::domain::Priority;
    use storyhook::service::{VerificationCandidate, VerificationProblem};
    use storyhook::store::ProjectId;

    const RESULT_ENV: &str = "STORY_CALLBACK_RESULT";
    if let Some(result_path) = std::env::var_os(RESULT_ENV) {
        let env = TestEnv::isolated();
        let candidate = VerificationCandidate {
            project: ProjectId::new(1),
            project_slug: "fixture".into(),
            story_id: "SH-CALLBACK-1".into(),
            title: "isolated callback".into(),
            priority: Priority::High,
            created_at: "2026-01-01T00:00:00Z".into(),
            verifying_since: None,
            verifying_generation: None,
            checkout: env.home().to_path_buf(),
            cleanup_lease: None,
            pull_request: Err(VerificationProblem::MissingPullRequest),
        };
        let actuator = ShellVerificationActuator::with_paths(
            env.environment(),
            std::env::var_os("STORY_CALLBACK_HELPER")
                .expect("callback helper")
                .into(),
            storyhook_test_support::story_binary().to_path_buf(),
        );
        let result = actuator.notify(
            &candidate,
            &std::env::var("STORY_CALLBACK_MESSAGE").expect("fixture message"),
        );
        std::fs::write(
            result_path,
            serde_json::json!({"error": result.err().map(|error| error.to_string())}).to_string(),
        )
        .expect("callback result");
        return;
    }

    let scratch = scratch_dir();
    let tmux_root = scratch.path().join("sockets");
    let uid = scratch.path().metadata().expect("fixture directory").uid();
    let socket_dir = tmux_root.join(format!("tmux-{uid}"));
    std::fs::create_dir_all(&socket_dir).expect("socket directory");
    std::fs::set_permissions(&socket_dir, std::fs::Permissions::from_mode(0o700))
        .expect("private socket directory");
    let default_socket = socket_dir.join("default");
    let unrelated_socket = socket_dir.join("unrelated");
    let _default_server = server(&default_socket);
    let _unrelated_server = server(&unrelated_socket);
    for socket in [&default_socket, &unrelated_socket] {
        tmux(
            socket,
            &["new-window", "-d", "-n", "SH-CALLBACK-1", "/bin/cat"],
        );
        tmux(
            socket,
            &["set-option", "-w", "-t", "@1", "@storyhook-agent", "codex"],
        );
        tmux(
            socket,
            &["set-option", "-w", "-t", "@1", "automatic-rename", "off"],
        );
    }
    let inherited_tmux = tmux(
        &unrelated_socket,
        &["display-message", "-p", "#{socket_path},#{pid},0"],
    );
    let inherited_pane = tmux(&unrelated_socket, &["display-message", "-p", "#{pane_id}"]);
    let real_tmux = std::env::split_paths(&std::env::var_os("PATH").expect("PATH"))
        .map(|directory| directory.join("tmux"))
        .find(|path| path.is_file())
        .expect("tmux installed");
    let bin = scratch.path().join("bin");
    std::fs::create_dir(&bin).expect("adapter directory");
    let adapter = bin.join("tmux");
    std::fs::write(
        &adapter,
        format!(
            "#!/bin/sh\nexport TMUX_TMPDIR='{}'\nexec '{}' \"$@\"\n",
            tmux_root.display(),
            real_tmux.display()
        ),
    )
    .expect("namespace adapter");
    std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o755))
        .expect("executable adapter");
    let helper = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/story/bin/story.sh");
    // Reintroduce the old inherited environment only in a fixture wrapper;
    // both cases still execute production notify and real tmux operations.
    let legacy_helper = scratch.path().join("legacy-environment.sh");
    std::fs::write(&legacy_helper, format!("export TMUX=\"$STORY_CALLBACK_INHERITED_TMUX\"\nexport TMUX_PANE=\"$STORY_CALLBACK_INHERITED_PANE\"\nexec bash '{}' \"$@\"\n", helper.display())).expect("legacy environment control");

    for (mode, selected_helper, expected_socket, other_socket) in [
        ("current", &helper, &default_socket, &unrelated_socket),
        ("legacy", &legacy_helper, &unrelated_socket, &default_socket),
    ] {
        let marker = format!("CALLBACK_FIXTURE_ONLY_584_{mode}");
        let result_path = scratch.path().join(format!("{mode}.json"));
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .args([
                "--exact",
                "verification_callback_delivers_only_to_the_default_server_agent",
                "--nocapture",
            ])
            .env(RESULT_ENV, &result_path)
            .env("STORY_CALLBACK_HELPER", selected_helper)
            .env("STORY_CALLBACK_MESSAGE", &marker)
            .env("STORY_CALLBACK_INHERITED_TMUX", inherited_tmux.trim())
            .env("STORY_CALLBACK_INHERITED_PANE", inherited_pane.trim())
            .env("TMUX", inherited_tmux.trim())
            .env("TMUX_PANE", inherited_pane.trim())
            .env("STORY_READY_PROCESS_PATTERN", "^cat$")
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").expect("PATH")),
            );
        let output = ChildGuard::spawn_with_output(&mut command)
            .expect("callback subprocess")
            .wait_with_output_within(STORY_COMMAND_DEADLINE.saturating_mul(2), || {
                "callback probe did not finish".into()
            });
        assert!(
            output.status.success(),
            "callback subprocess: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: serde_json::Value =
            serde_json::from_slice(&std::fs::read(result_path).expect("callback result"))
                .expect("callback JSON");
        assert!(
            result["error"].is_null(),
            "{mode} production notification failed: {result}"
        );
        let deadline = Instant::now() + STARTUP_DEADLINE;
        loop {
            let transcript = tmux(expected_socket, &["capture-pane", "-p", "-t", "@1"]);
            if transcript.contains(&marker) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{mode} callback never arrived: {transcript}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        let other = tmux(other_socket, &["capture-pane", "-p", "-t", "@1"]);
        assert!(
            !other.contains(&marker),
            "{mode} callback reached the wrong server: {other}"
        );
    }
}
