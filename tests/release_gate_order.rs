//! SH-588: the release gate must certify the versioned tree before it leaves.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use storyhook_test_support::{daemon_containment, scratch_dir_named};

struct ReleaseFixture {
    scratch: tempfile::TempDir,
    repo: PathBuf,
}

impl ReleaseFixture {
    fn new() -> Self {
        let scratch = scratch_dir_named("release-gate-order");
        let repo = scratch.path().join("repo");
        for directory in [
            repo.join("scripts"),
            repo.join("bin"),
            scratch.path().join("home"),
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        for file in ["release.sh", "release-targets.sh", "branch-policy.sh"] {
            fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("scripts")
                    .join(file),
                repo.join("scripts").join(file),
            )
            .unwrap();
        }
        fs::write(repo.join("VERSION"), "v9.9.9\n").unwrap();
        for file in ["bin/claude", "bin/cargo", "scripts/build-release-assets.sh"] {
            executable(&repo.join(file), "#!/bin/bash\nexit 0\n");
        }
        executable(
            &repo.join("scripts/render-release-body.sh"),
            "#!/bin/bash\necho notes\n",
        );
        executable(
            &repo.join("bin/semver"),
            r#"#!/bin/bash
set -eu
case "$1" in
  validate) echo '{"status":"PASS"}' ;;
  current) echo '{"version_prefix":"v"}' ;;
  bump)
    echo bump >> "$RELEASE_TEST_LOG"
    [ "${RELEASE_BUMP_FAIL:-0}" = 0 ] || exit 74
    printf 'v9.9.10\n' > VERSION
    git add VERSION
    git commit -qm 'chore: fixture version bump'
    ;;
  *) exit 90 ;;
esac
"#,
        );
        executable(
            &repo.join("bin/make"),
            r#"#!/bin/bash
set -eu
case "$1" in
  test-full)
    printf 'gate:%s:%s\n' "$(cat VERSION)" "$(git rev-parse 'HEAD^{tree}')" >> "$RELEASE_TEST_LOG"
    if [ "$(cat VERSION)" = v9.9.10 ] && [ "${RELEASE_GATE_FAIL:-0}" = 1 ]; then exit 73; fi
    ;;
  install) printf 'install:%s\n' "$(cat VERSION)" >> "$RELEASE_TEST_LOG" ;;
  *) exit 90 ;;
esac
"#,
        );
        executable(
            &repo.join("bin/gh"),
            r#"#!/bin/bash
set -eu
case "$1 ${2:-}" in
  'auth status'|'api --paginate') exit 0 ;;
  'pr create') echo pr:create >> "$RELEASE_TEST_LOG"; exit 91 ;;
  *) exit 90 ;;
esac
"#,
        );
        executable(
            &repo.join("bin/story"),
            "#!/bin/bash\nprintf 'story %s\n' \"$(cat VERSION)\"\n",
        );
        let fixture = Self { scratch, repo };
        fixture.git(&["init", "-q", "-b", "dev"]);
        fixture.git(&["config", "user.name", "Release Fixture"]);
        fixture.git(&["config", "user.email", "release@example.test"]);
        fixture.git(&["add", "-A"]);
        fixture.git(&["commit", "-qm", "fixture"]);
        fixture.git(&["init", "-q", "--bare", "../origin"]);
        fixture.git(&["remote", "add", "origin", "../origin"]);
        fixture.git(&["push", "-q", "-u", "origin", "dev"]);
        fixture
    }

    /// Every subprocess stays inside this fixture's Git, home and daemon scope.
    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(&self.repo)
            .env_clear()
            .envs(daemon_containment())
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.repo.join("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("HOME", self.scratch.path().join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("SEMVER_CLI", self.repo.join("bin/semver"))
            .env("RELEASE_TEST_LOG", self.scratch.path().join("calls"));
        command
    }

    fn git(&self, args: &[&str]) -> String {
        let output = self.command("git").args(args).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn run(&self, local: bool, bump: bool, gate_fail: bool, bump_fail: bool) -> Output {
        let mut command = self.command("bash");
        command
            .arg("scripts/release.sh")
            .args(["--yes", "--skip-plugin", "--skip-daemon"]);
        if local {
            command.arg("--local-only");
        }
        if bump {
            command.args(["--bump", "patch"]);
        }
        command
            .env("RELEASE_GATE_FAIL", if gate_fail { "1" } else { "0" })
            .env("RELEASE_BUMP_FAIL", if bump_fail { "1" } else { "0" })
            .output()
            .unwrap()
    }

    fn calls(&self) -> String {
        match fs::read_to_string(self.scratch.path().join("calls")) {
            Ok(calls) => calls,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => panic!("reading release calls: {error}"),
        }
    }

    fn assert_no_push(&self) {
        assert!(
            self.git(&["ls-remote", "--heads", "origin", "release/*"])
                .is_empty()
        );
    }
}

fn executable(path: &Path, source: &str) {
    fs::write(path, source).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn assert_exit(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn failed_gate_after_public_bump_prevents_push() {
    let fixture = ReleaseFixture::new();
    let output = fixture.run(false, true, true, false);
    assert_exit(&output, 73);
    let tree = fixture.git(&["rev-parse", "HEAD^{tree}"]);
    assert_eq!(fixture.calls(), format!("bump\ngate:v9.9.10:{tree}\n"));
    fixture.assert_no_push();
}

#[test]
fn failed_gate_after_local_bump_prevents_install() {
    let fixture = ReleaseFixture::new();
    assert_exit(&fixture.run(true, true, true, false), 73);
    let tree = fixture.git(&["rev-parse", "HEAD^{tree}"]);
    assert_eq!(fixture.calls(), format!("bump\ngate:v9.9.10:{tree}\n"));
    fixture.assert_no_push();
}

#[test]
fn successful_public_gate_certifies_the_tree_actually_pushed() {
    let fixture = ReleaseFixture::new();
    // The GitHub boundary deliberately stops after the real disposable push.
    let output = fixture.run(false, true, false, false);
    assert_exit(&output, 1);
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("could not open the stable-release pull request"),
        "translated GitHub failure must retain its release-stage context: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = fixture.git(&["rev-parse", "refs/remotes/origin/release/v9.9.10^{tree}"]);
    assert_eq!(
        fixture.calls(),
        format!("bump\ngate:v9.9.10:{tree}\npr:create\n")
    );
}

#[test]
fn local_install_gates_the_final_tree_with_or_without_a_bump() {
    for bump in [false, true] {
        let fixture = ReleaseFixture::new();
        assert_exit(&fixture.run(true, bump, false, false), 0);
        let tree = fixture.git(&["rev-parse", "HEAD^{tree}"]);
        let (prefix, version) = if bump {
            ("bump\n", "v9.9.10")
        } else {
            ("", "v9.9.9")
        };
        assert_eq!(
            fixture.calls(),
            format!("{prefix}gate:{version}:{tree}\ninstall:{version}\n")
        );
        fixture.assert_no_push();
    }
}

#[test]
fn a_failed_bump_never_reaches_the_gate_or_external_effects() {
    for local in [false, true] {
        let fixture = ReleaseFixture::new();
        assert_exit(&fixture.run(local, true, false, true), 74);
        assert_eq!(fixture.calls(), "bump\n");
        fixture.assert_no_push();
    }
}
