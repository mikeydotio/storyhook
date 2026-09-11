//! The browser harness's provider doubles work from the environment the
//! daemon actually gives its own tmux calls (SH-626).
//!
//! Every tmux the daemon runs, it runs through `apply_dispatch_allowlist`
//! (`src/env/spawn_env.rs`): `env_clear`, then only `PATH`, `HOME`, the XDG
//! homes, `TMPDIR`, the locale/terminal names and `STORY_*`/`STORYHOOK_*`.
//! `scripts/run-e2e.sh` had bridged the fake tmux's `FAKE_TMUX_*` knobs across
//! that boundary since SH-263 -- for the dispatch child only, through the
//! generated dispatch wrapper. The daemon crosses the same boundary a second
//! time on its own: the Full Auto reconciler's liveness probe
//! (`ShellDispatcher`, `tmux display-message -p -t <pane>
//! WINDOW_PROBE_FORMAT`) never passes through story.sh. It reached a `tmux`
//! double whose `set -u` died on an unset `FAKE_TMUX_IMPLEMENTATION`, and an
//! exit status of 1 was read as "the window is gone" on every steady pass --
//! the browser suite stayed green only when its stop-now beat that pass.
//!
//! This file is the merge-gate half of that regression (`make test` never
//! runs the browser tier, SH-418): it generates the doubles with the tracked
//! library, builds the daemon's tmux command through the **real** allowlist,
//! and asks the exact question `ShellDispatcher` asks, using the exact format
//! it uses. The browser-tier half is `engine.spec.ts`'s real-daemon case,
//! which waits for the daemon's own pass to observe its lane alive.
//!
//! The negative control is load-bearing: the same double with an empty knob
//! directory must fail, or the positive case could be passing because the
//! environment was never stripped at all (a developer with `FAKE_TMUX_STATE`
//! exported, say -- checked explicitly below, so that case fails by name
//! rather than vacuously passing).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::Duration;

use storyhook::env::spawn_env::apply_dispatch_allowlist;
use storyhook::service::engine::{TMUX_TIMEOUT, WINDOW_PROBE_FORMAT};
use storyhook_test_support::{run_bounded, scratch_dir};

/// One shell layer (the double) plus the fake itself, each of which the
/// engine gives `TMUX_TIMEOUT`; the whole exchange therefore has to finish
/// within twice that, or the probe was slower than the daemon would wait.
const SHELL_DEADLINE: Duration = Duration::from_secs(2 * TMUX_TIMEOUT.as_secs());

fn checkout() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// A generated provider directory, the knob snapshot it reads, and the fake
/// tmux state that snapshot names.
struct Doubles {
    _root: tempfile::TempDir,
    provider_bin: PathBuf,
    knob_dir: PathBuf,
    state: PathBuf,
    pane_cwd: PathBuf,
}

impl Doubles {
    fn generate(snapshot_knobs: bool) -> Self {
        let root = scratch_dir();
        let provider_bin = root.path().join("provider-bin");
        let knob_dir = root.path().join("faketmux-env");
        let state = root.path().join("faketmux");
        let pane_cwd = root.path().join("worktree");
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&knob_dir).unwrap();
        fs::create_dir_all(&pane_cwd).unwrap();
        let fake = checkout().join("plugins/story/tests/fakes/tmux");
        if snapshot_knobs {
            // The runner's own snapshot shape: one file per knob, the value
            // verbatim, no trailing newline.
            fs::write(
                knob_dir.join("FAKE_TMUX_STATE"),
                state.to_string_lossy().as_bytes(),
            )
            .unwrap();
            fs::write(
                knob_dir.join("FAKE_TMUX_IMPLEMENTATION"),
                fake.to_string_lossy().as_bytes(),
            )
            .unwrap();
        }
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(". \"$1\"; shift; write_e2e_provider_doubles \"$@\"")
            .arg("e2e-provider-doubles-under-test")
            .arg(checkout().join("scripts/e2e-provider-doubles.sh"))
            .arg(&provider_bin)
            .arg(&knob_dir)
            .arg(&fake)
            .current_dir(root.path());
        let output = run_bounded(cmd, "write_e2e_provider_doubles", SHELL_DEADLINE);
        assert!(
            output.status.success(),
            "generating the doubles: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        for name in ["claude", "codex", "tmux"] {
            let mode = fs::metadata(provider_bin.join(name))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o111,
                0o100,
                "{name} must be executable by its owner"
            );
        }
        Self {
            _root: root,
            provider_bin,
            knob_dir,
            state,
            pane_cwd,
        }
    }

    /// Exactly the command the daemon builds for its own tmux calls: the
    /// production allowlist over this process's environment, then `PATH`
    /// with the doubles first -- the order `apply_dispatch_allowlist`'s own
    /// doc requires, since `env_clear` removes anything set before it.
    fn daemon_tmux(&self) -> Command {
        let mut command = Command::new("tmux");
        apply_dispatch_allowlist(&mut command);
        let system_path = std::env::var("PATH").unwrap_or_default();
        command.env(
            "PATH",
            format!("{}:{system_path}", self.provider_bin.display()),
        );
        command.current_dir(&self.pane_cwd);
        command
    }

    fn new_window(&self) -> Output {
        let mut command = self.daemon_tmux();
        command.args([
            "new-window",
            "-d",
            "-c",
            &self.pane_cwd.to_string_lossy(),
            "-n",
            "EE-1",
            "-P",
            "-F",
            "#{pane_id}",
            "claude --permission-mode plan",
            ";",
            "set-window-option",
            "-t",
            "@1",
            "remain-on-exit",
            "on",
        ]);
        run_bounded(
            command,
            "tmux new-window through the double",
            SHELL_DEADLINE,
        )
    }

    fn probe(&self, pane: &str) -> Output {
        let mut command = self.daemon_tmux();
        command.args(["display-message", "-p", "-t", pane, WINDOW_PROBE_FORMAT]);
        run_bounded(
            command,
            "the engine's liveness probe through the double",
            SHELL_DEADLINE,
        )
    }

    fn reap_placeholder(&self) {
        if let Ok(pid) = fs::read_to_string(self.state.join("pane_pid"))
            && let Ok(pid) = pid.trim().parse::<i32>()
        {
            // SAFETY: the pid was written by the fake this test drove, into a
            // state directory only this test names.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A `FAKE_TMUX_*` name exported into this test's own process cannot cross
/// the allowlist today, but the negative control below would stop proving
/// that the moment the allowlist widened, so the case is refused by name
/// rather than left to pass vacuously (SH-306).
fn assert_no_ambient_knobs() {
    let ambient: Vec<String> = std::env::vars()
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("FAKE_TMUX_"))
        .collect();
    assert!(
        ambient.is_empty(),
        "this test process carries {ambient:?}; unset them, or the negative control cannot \
         prove the daemon's environment is stripped"
    );
}

#[test]
fn the_daemons_own_liveness_probe_reaches_the_fake_through_the_double() {
    assert_no_ambient_knobs();
    let doubles = Doubles::generate(true);

    let opened = doubles.new_window();
    assert!(
        opened.status.success(),
        "new-window through the double: {}",
        stderr_of(&opened)
    );
    let pane = stdout_of(&opened).trim().to_string();
    assert_eq!(
        pane, "%1",
        "the fake answers new-window with its one pane id"
    );

    let answer = doubles.probe(&pane);
    doubles.reap_placeholder();
    assert!(
        answer.status.success(),
        "the daemon's probe must reach the fake with only the allowlisted environment: {}",
        stderr_of(&answer)
    );
    let stdout = stdout_of(&answer);
    let fields: Vec<&str> = stdout.trim_end().split('\t').collect();
    assert_eq!(
        fields.len(),
        4,
        "the composite probe answers four tab-separated fields, got {stdout:?}"
    );
    let pid: i32 = fields[0]
        .parse()
        .unwrap_or_else(|_| panic!("the first field is the placeholder's pid, got {stdout:?}"));
    assert!(pid > 0);
    assert_eq!(
        fields[1], "claude",
        "the occupant is the launch's own binary"
    );
    assert_eq!(fields[2], "0", "a freshly opened pane is not dead");
    let activity: i64 = fields[3].parse().unwrap_or_else(|_| {
        panic!("the fourth field is the window's last-output unix time (SH-657), got {stdout:?}")
    });
    assert!(
        activity > 0,
        "a freshly opened window has written its prompt, so its activity stamp is set"
    );
    assert!(
        doubles.knob_dir.join("FAKE_TMUX_STATE").exists(),
        "the double read its knobs from the snapshot directory it was handed"
    );
}

#[test]
fn a_double_with_no_snapshot_fails_rather_than_answering() {
    assert_no_ambient_knobs();
    let doubles = Doubles::generate(false);
    let answer = doubles.probe("%1");
    assert!(
        !answer.status.success(),
        "with nothing to bridge, the stripped environment must reach the fake's own refusal \
         rather than an answer -- otherwise the positive case above proves nothing: stdout {:?}",
        stdout_of(&answer)
    );
    let stderr = stderr_of(&answer);
    assert!(
        stderr.contains("FAKE_TMUX_STATE") || stderr.contains("FAKE_TMUX_IMPLEMENTATION"),
        "the refusal names the missing knob: {stderr}"
    );
}

#[test]
fn a_knob_the_caller_already_carries_wins_over_the_snapshot() {
    assert_no_ambient_knobs();
    let doubles = Doubles::generate(true);
    let other_state = doubles._root.path().join("other-state");
    fs::create_dir_all(&other_state).unwrap();
    let mut command = doubles.daemon_tmux();
    command.env("FAKE_TMUX_STATE", &other_state);
    command.args(["display-message", "-p", "-t", "%1", "#{session_name}"]);
    fs::write(doubles.state.join("session_name"), "snapshot-session").unwrap();
    fs::write(other_state.join("session_name"), "caller-session").unwrap();
    let answer = run_bounded(
        command,
        "display-message with a caller-set knob",
        SHELL_DEADLINE,
    );
    assert!(answer.status.success(), "{}", stderr_of(&answer));
    assert_eq!(
        stdout_of(&answer).trim(),
        "caller-session",
        "the dispatch child's bridged environment and the double's own bridge must agree \
         rather than compete: the caller's value stands"
    );
}

#[test]
fn the_runner_hands_the_library_the_snapshot_directory_it_fills() {
    let runner = fs::read_to_string(checkout().join("scripts/run-e2e.sh")).unwrap();
    let body = runner
        .split_once("run_one_project() {")
        .expect("scripts/run-e2e.sh must define run_one_project")
        .1;
    let sourced = runner
        .lines()
        .any(|line| line.trim() == ". \"$repo_root/scripts/e2e-provider-doubles.sh\"");
    assert!(
        sourced,
        "scripts/run-e2e.sh must source scripts/e2e-provider-doubles.sh"
    );
    let named = body
        .find("faketmux_env=\"$data_root/faketmux-env\"")
        .expect("the runner names its knob snapshot directory");
    let written = body
        .find("write_e2e_provider_doubles \"$provider_bin\" \"$faketmux_env\" \"$FAKE_TMUX_IMPLEMENTATION\"")
        .expect("the runner generates the doubles through the library, handing it the snapshot directory");
    let filled = body
        .find("mkdir -p \"$faketmux_env\"")
        .expect("the runner fills the snapshot directory");
    let started = body
        .find("start_output=\"$(\"$story_bin\" daemon start 2>&1)\"")
        .expect("run_one_project must start its daemon");
    assert!(
        named < written && written < filled && filled < started,
        "the double is handed the directory the runner later snapshots into, before the daemon \
         that will read through it starts"
    );
}
