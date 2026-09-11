//! Production mirror behavior against real tmux on a private fixture socket.

use super::{checkout, shell_quote};
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};
use storyhook_test_support::scratch_dir;

struct Mirror {
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
        Self { root, tmux, socket }
    }

    fn tmux(&self, args: &[&str]) -> Output {
        Command::new(&self.tmux)
            .arg("-S")
            .arg(&self.socket)
            .args(["-f", "/dev/null"])
            .args(args)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .output()
            .unwrap()
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut path = self.root.path().join("bin").into_os_string();
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap_or_default());
        let mut command = Command::new("bash");
        command
            .arg(checkout().join("scripts/verify-window.sh"))
            .args(args)
            .current_dir(cwd)
            .env("PATH", path)
            .env("HOME", self.root.path().join("home"))
            .env("STORYHOOK_VERIFIER_MIRROR", "1")
            .env_remove("STORYHOOK_ACTIVITY_LOG_DIR")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        command
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
        let deadline = Instant::now() + Duration::from_secs(5);
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
        // The socket was created by this fixture; never address the user's server.
        let _ = self.tmux(&["kill-server"]);
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
        !mirror.socket.exists(),
        "failed identity must not even start tmux"
    );
}
