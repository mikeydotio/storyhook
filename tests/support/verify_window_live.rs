//! Production mirror behavior against real tmux on a private fixture socket.

use super::{checkout, shell_quote};
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use storyhook_test_support::{ChildGuard, STORY_COMMAND_DEADLINE, scratch_dir};

#[test]
fn a_private_server_is_owned_before_any_mirror_command() {
    let mirror = Mirror::new();
    let out = mirror.tmux(&["show-options", "-s", "-v", "exit-empty"]);
    assert!(
        out.status.success(),
        "fixture must own a server before publishing commands: {out:?}"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "off");
}

#[test]
fn fixture_teardown_reaps_banner_and_activity_readers_even_while_unwinding() {
    use storyhook::daemon::lifecycle::{process_identity_is_live, process_start_time};
    use storyhook_test_support::STORY_COMMAND_DEADLINE;
    // A separate private server stands in for another owner's session.
    let control = Mirror::new();
    let project = control.project("control");
    assert!(
        control
            .command(&project, &["banner", "CONTROL"])
            .output()
            .unwrap()
            .status
            .success()
    );
    control.pane_with("CONTROL");
    for unwind in [false, true] {
        let mut identities = Vec::new();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mirror = Mirror::new();
            let project = mirror.project("owned");
            let store = mirror.root.path().join("store.db");
            fs::write(&store, "").unwrap();
            journal(&mirror.root.path().join("home"), &store, "OWNED_READER");
            let binary = storyhook_test_support::story_binary();
            for args in [
                vec!["banner", "OWNED_BANNER"],
                vec!["logs", binary.to_str().unwrap(), store.to_str().unwrap()],
            ] {
                assert!(
                    mirror
                        .command(&project, &args)
                        .output()
                        .unwrap()
                        .status
                        .success()
                );
            }
            mirror.pane_with("OWNED_BANNER");
            mirror.pane_with("OWNED_READER");
            let pids = mirror.tmux(&["list-panes", "-a", "-F", "#{pane_pid}"]);
            assert!(pids.status.success(), "{pids:?}");
            for pid in String::from_utf8(pids.stdout).unwrap().lines() {
                let pid: u32 = pid.parse().unwrap();
                let token = process_start_time(pid).expect("fixture reader has native identity");
                identities.push((pid, token));
            }
            assert_eq!(
                identities.len(),
                2,
                "banner sleeper and real activity reader"
            );
            if unwind {
                panic!("deliberate fixture assertion failure");
            }
        }));
        assert_eq!(outcome.is_err(), unwind);
        let deadline = Instant::now() + STORY_COMMAND_DEADLINE;
        while identities
            .iter()
            .any(|(pid, token)| process_identity_is_live(*pid, Some(token)))
        {
            assert!(
                Instant::now() < deadline,
                "fixture readers survived teardown: {identities:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(control.windows().len(), 1);
        control.pane_with("CONTROL");
    }
}

struct Mirror {
    // Dropped explicitly before root, including when startup or a test panics.
    server: Mutex<Option<ChildGuard>>,
    root: tempfile::TempDir,
    tmux: PathBuf,
    socket: PathBuf,
}

impl Mirror {
    fn new() -> Self {
        let found = Command::new("sh")
            .args(["-c", "command -v tmux"])
            .output()
            .unwrap();
        assert!(
            found.status.success(),
            "real tmux is required for mirror tests"
        );
        let tmux = PathBuf::from(String::from_utf8(found.stdout).unwrap().trim());
        let root = scratch_dir();
        let socket = root.path().join("tmux.sock");
        fs::create_dir(root.path().join("bin")).unwrap();
        fs::create_dir(root.path().join("home")).unwrap();
        let wrapper = root.path().join("bin/tmux");
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nexec {} -S {} -f /dev/null \"$@\"\n",
                shell_quote(tmux.to_str().unwrap()),
                shell_quote(socket.to_str().unwrap())
            ),
        )
        .unwrap();
        fs::set_permissions(wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        let mut command = Command::new(&tmux);
        command
            .args(["-D", "-f", "/dev/null", "-S"])
            .arg(&socket)
            .env("HOME", root.path().join("home"))
            .env_remove("XDG_STATE_HOME")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .envs(storyhook_test_support::daemon_containment())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        let server = ChildGuard::spawn(&mut command).expect("start owned private tmux server");
        let mut fixture = Self {
            server: Mutex::new(Some(server)),
            root,
            tmux,
            socket,
        };
        let deadline = Instant::now() + STORY_COMMAND_DEADLINE;
        while !fixture.socket.exists() {
            assert!(
                fixture
                    .server
                    .get_mut()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .try_wait()
                    .is_none(),
                "private tmux exited before publishing its socket"
            );
            assert!(
                Instant::now() < deadline,
                "private tmux socket was never published"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        fixture
    }

    fn tmux_result(&self, args: &[&str]) -> Result<Output, String> {
        let mut command = Command::new(&self.tmux);
        command
            .arg("-S")
            .arg(&self.socket)
            .args(["-f", "/dev/null"])
            .args(args)
            .env("HOME", self.root.path().join("home"))
            .env_remove("XDG_STATE_HOME")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        let mut child = ChildGuard::spawn_with_output(&mut command)
            .map_err(|error| format!("start private tmux {args:?}: {error}"))?;
        // ChildGuard reports deadline/pipe failures by panicking. Convert only
        // that bounded wait into a diagnostic so Drop can still reap the server
        // when an assertion is already unwinding.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            child.wait_with_output_within(STORY_COMMAND_DEADLINE, || {
                format!("private tmux {args:?} did not finish")
            })
        }))
        .map_err(|failure| {
            let message = failure
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| failure.downcast_ref::<&str>().copied())
                .unwrap_or("non-text panic from bounded child wait");
            format!("private tmux {args:?}: {message}")
        })
    }

    fn tmux(&self, args: &[&str]) -> Output {
        self.tmux_result(args)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut command = Command::new("bash");
        command
            .arg(checkout().join("scripts/verify-window.sh"))
            .args(args)
            .current_dir(cwd);
        self.apply_mirror(&mut command);
        command
    }

    fn apply_mirror(&self, command: &mut Command) {
        let mut path = self.root.path().join("bin").into_os_string();
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        command
            .env("PATH", path)
            .env("HOME", self.root.path().join("home"))
            .env("STORYHOOK_VERIFIER_MIRROR", "1")
            .env_remove("XDG_STATE_HOME")
            .env_remove("STORYHOOK_ACTIVITY_LOG_DIR")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
    }

    fn project(&self, relative: &str) -> PathBuf {
        let path = self.root.path().join(relative);
        fs::create_dir_all(&path).unwrap();
        git(&path, &["init", "-q", "-b", "main"]);
        git(&path, &["commit", "--allow-empty", "-qm", "seed"]);
        path
    }

    fn windows(&self) -> Vec<String> {
        let out = self.tmux(&[
            "list-windows",
            "-t",
            "=storyhook-verifier",
            "-F",
            "#{window_name}",
        ]);
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn pane_with(&self, needle: &str) -> String {
        let deadline = Instant::now() + STORY_COMMAND_DEADLINE;
        loop {
            for window in self.windows() {
                let target = format!("=storyhook-verifier:={window}");
                let out = self.tmux(&["capture-pane", "-p", "-t", &target]);
                assert!(out.status.success(), "{out:?}");
                if String::from_utf8_lossy(&out.stdout).contains(needle) {
                    return window;
                }
            }
            assert!(Instant::now() < deadline, "no pane displayed {needle:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Mirror {
    fn drop(&mut self) {
        use storyhook::daemon::lifecycle::{process_identity_is_live, process_start_time};
        let mut errors = Vec::new();
        let mut readers = Vec::new();
        match self.tmux_result(&["list-panes", "-a", "-F", "#{pane_pid}"]) {
            Ok(output) if output.status.success() => {
                for raw in String::from_utf8_lossy(&output.stdout).lines() {
                    match raw.parse::<u32>() {
                        Ok(pid) => readers.push((pid, process_start_time(pid))),
                        Err(error) => {
                            errors.push(format!("invalid private reader pid {raw:?}: {error}"))
                        }
                    }
                }
            }
            Ok(output)
                if output.status.code() == Some(1)
                    && output.stdout.is_empty()
                    && String::from_utf8_lossy(&output.stderr).trim() == "no current target" =>
            {
                // An owned -D server with no sessions has no pane target yet.
            }
            other => errors.push(format!("could not census private readers: {other:?}")),
        }
        match self.tmux_result(&["kill-server"]) {
            Ok(output) if output.status.success() => {}
            other => errors.push(format!("could not close private tmux: {other:?}")),
        }
        let deadline = Instant::now() + STORY_COMMAND_DEADLINE;
        let server = self
            .server
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(mut server) = server.take() {
            while server.try_wait().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            if server.try_wait().is_none() {
                errors.push("private tmux did not stop within its command deadline".into());
            }
            // Also covers a failed control client; ownership never depends on the socket.
            server.kill_and_reap();
        }
        while readers
            .iter()
            .any(|(pid, token)| process_identity_is_live(*pid, token.as_deref()))
        {
            if Instant::now() >= deadline {
                errors.push(format!(
                    "private terminal readers survived shutdown: {readers:?}"
                ));
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if !errors.is_empty() {
            let diagnostic = format!(
                "tmux fixture {} teardown: {}",
                self.socket.display(),
                errors.join("; ")
            );
            if std::thread::panicking() {
                eprintln!("{diagnostic}");
            } else {
                panic!("{diagnostic}");
            }
        }
    }
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
        ])
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

#[test]
fn concurrent_projects_keep_independent_live_logs_and_reuse_worktree_identity() {
    let mirror = Mirror::new();
    let first = mirror.project("one/same name's $(inert)");
    let second = mirror.project("two/same name's $(inert)");
    let log_a = first.join("attempt's $(inert).log");
    let log_b = second.join("attempt.log");
    fs::write(&log_a, "PROJECT_A_BEGIN\n").unwrap();
    fs::write(&log_b, "PROJECT_B_BEGIN\n").unwrap();
    std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            mirror
                .command(&first, &["tail", log_a.to_str().unwrap()])
                .env("STORYHOOK_ACTIVITY_LOG_DIR", mirror.root.path())
                .output()
                .unwrap()
        });
        let b = scope.spawn(|| {
            mirror
                .command(&second, &["tail", log_b.to_str().unwrap()])
                .env("STORYHOOK_ACTIVITY_LOG_DIR", mirror.root.path())
                .output()
                .unwrap()
        });
        for out in [a.join().unwrap(), b.join().unwrap()] {
            assert!(out.status.success(), "{out:?}");
        }
    });
    assert_eq!(mirror.windows().len(), 2);
    let a = mirror.pane_with("PROJECT_A_BEGIN");
    let b = mirror.pane_with("PROJECT_B_BEGIN");
    assert_ne!(a, b);
    writeln!(
        fs::OpenOptions::new().append(true).open(&log_a).unwrap(),
        "PROJECT_A_LIVE"
    )
    .unwrap();
    writeln!(
        fs::OpenOptions::new().append(true).open(&log_b).unwrap(),
        "PROJECT_B_LIVE"
    )
    .unwrap();
    assert_eq!(mirror.pane_with("PROJECT_A_LIVE"), a);
    assert_eq!(mirror.pane_with("PROJECT_B_LIVE"), b);

    let linked = mirror.root.path().join("linked");
    git(
        &first,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.to_str().unwrap(),
        ],
    );
    let alias = mirror.root.path().join("alias");
    symlink(&linked, &alias).unwrap();
    let out = mirror
        .command(&alias, &["banner", "it's literal; $(inert) `inert` SH-999"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(mirror.pane_with("it's literal; $(inert) `inert` SH-999"), a);
    assert_eq!(mirror.pane_with("PROJECT_B_LIVE"), b);
    assert_eq!(mirror.windows().len(), 2);

    let common = fs::canonicalize(first.join(".git")).unwrap();
    let identity = mirror.root.path().join("identity");
    fs::write(&identity, common.as_os_str().as_encoded_bytes()).unwrap();
    let hash = git(&first, &["hash-object", identity.to_str().unwrap()]);
    assert!(
        a.ends_with(&hash),
        "project window must use the full gate-lock digest: {a}"
    );
}

#[test]
fn simultaneous_starts_for_one_project_create_exactly_one_window() {
    let mirror = Mirror::new();
    let project = mirror.project("project");
    std::thread::scope(|scope| {
        let runs: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    mirror
                        .command(&project, &["banner", "same project"])
                        .output()
                        .unwrap()
                })
            })
            .collect();
        for run in runs {
            let out = run.join().unwrap();
            assert!(out.status.success(), "{out:?}");
        }
    });
    assert_eq!(mirror.windows().len(), 1);
    mirror.pane_with("same project");
}

#[test]
fn non_repository_never_falls_back_to_a_shared_project_window() {
    let mirror = Mirror::new();
    let out = mirror
        .command(mirror.root.path(), &["banner", "no project"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        !mirror
            .tmux(&["has-session", "-t", "=storyhook-verifier"])
            .status
            .success(),
        "failed identity must not create a session on the fixture's owned server"
    );
}

/// Populate the same daily JSONL input the production log reader consumes.
fn journal(home: &Path, store: &Path, message: &str) {
    let location = storyhook::env::StoreLocation::resolve(
        Some(store),
        &storyhook::env::StoreVars::default(),
        home,
    )
    .unwrap();
    let env = storyhook::env::Environment::at(home).with_store(location);
    let directory = env.daemon_state_dir().join("activity");
    fs::create_dir_all(&directory).unwrap();
    let now = chrono::Utc::now();
    let row = serde_json::json!({"at": now.to_rfc3339(), "level": "INFO",
        "source": "fixture", "stream": "event", "pid": 1, "context": "", "message": message});
    writeln!(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(directory.join(format!("{}.jsonl", now.format("%Y-%m-%d"))))
            .unwrap(),
        "{row}"
    )
    .unwrap();
}

#[test]
fn stores_keep_continuous_readers_alongside_project_and_legacy_windows() {
    let mirror = Mirror::new();
    let project = mirror.project("project");
    let first = mirror.root.path().join("one/store's $(inert).db");
    let second = mirror.root.path().join("two/store's $(inert).db");
    for store in [&first, &second] {
        fs::create_dir_all(store.parent().unwrap()).unwrap();
        fs::write(store, "").unwrap();
    }
    journal(&mirror.root.path().join("home"), &first, "STORE_A_BEGIN");
    journal(&mirror.root.path().join("home"), &second, "STORE_B_BEGIN");
    let legacy = mirror.tmux(&[
        "new-session",
        "-d",
        "-s",
        "storyhook-verifier",
        "-n",
        "verification",
        "sleep",
        "2147483647",
    ]);
    assert!(legacy.status.success(), "{legacy:?}");
    let legacy_pid = mirror
        .tmux(&[
            "display-message",
            "-p",
            "-t",
            "=storyhook-verifier:=verification",
            "#{pane_pid}",
        ])
        .stdout;
    let binary = storyhook_test_support::story_binary();
    let mirror = &mirror;
    std::thread::scope(|scope| {
        let runs: Vec<_> = [&first, &second]
            .into_iter()
            .map(|store| {
                scope.spawn(move || {
                    mirror
                        .command(
                            mirror.root.path(),
                            &["logs", binary.to_str().unwrap(), store.to_str().unwrap()],
                        )
                        .output()
                        .unwrap()
                })
            })
            .collect();
        for run in runs {
            let out = run.join().unwrap();
            assert!(out.status.success(), "{out:?}");
        }
    });
    assert_eq!(mirror.windows().len(), 3);
    let a = mirror.pane_with("STORE_A_BEGIN");
    let b = mirror.pane_with("STORE_B_BEGIN");
    assert_ne!(a, b);
    assert!(a.starts_with("activity-") && b.starts_with("activity-"));
    let out = mirror
        .command(&project, &["banner", "PROJECT_PHASE"])
        .env("STORYHOOK_ACTIVITY_LOG_DIR", mirror.root.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("PROJECT_PHASE"));
    assert!(
        mirror
            .pane_with("PROJECT_PHASE")
            .starts_with("verification-")
    );
    journal(&mirror.root.path().join("home"), &first, "STORE_A_LIVE");
    journal(&mirror.root.path().join("home"), &second, "STORE_B_LIVE");
    assert_eq!(mirror.pane_with("STORE_A_LIVE"), a);
    assert_eq!(mirror.pane_with("STORE_B_LIVE"), b);
    let alias = mirror.root.path().join("store-alias.db");
    symlink(&first, &alias).unwrap();
    let out = mirror
        .command(
            mirror.root.path(),
            &["logs", binary.to_str().unwrap(), "store-alias.db"],
        )
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(mirror.windows().len(), 4);
    assert_eq!(mirror.pane_with("STORE_A_LIVE"), a);
    let out = mirror.tmux(&[
        "display-message",
        "-p",
        "-t",
        "=storyhook-verifier:=verification",
        "#{pane_pid}",
    ]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(out.stdout, legacy_pid, "legacy reader must not be replaced");
}
