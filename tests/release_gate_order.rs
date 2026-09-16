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
        for file in [
            "branch-policy.sh",
            "github-access.sh",
            "release.sh",
            "release-targets.sh",
            "build-number.py",
        ] {
            fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("scripts")
                    .join(file),
                repo.join("scripts").join(file),
            )
            .unwrap();
        }
        fs::write(repo.join("VERSION"), "v9.9.9\n").unwrap();
        fs::write(repo.join("BUILD"), "0\n").unwrap();
        fs::write(repo.join(".gitignore"), "/.build-number.lock\n/.BUILD-*\n").unwrap();
        for file in [
            "bin/claude",
            "bin/codex",
            "bin/cargo",
            "scripts/build-release-assets.sh",
        ] {
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
[ "$GH_HOST" = github.example.com ] || exit 92
[ "$GH_REPO" = github.example.com/acme/storyhook ] || exit 93
case "$1 ${2:-}" in
  'api repos/acme/storyhook/releases?per_page=100') exit 0 ;;
  'pr create') echo pr:create >> "$GH_CONFIG_DIR/calls"; exit 91 ;;
  *) exit 90 ;;
esac
"#,
        );
        executable(
            &repo.join("bin/story"),
            r#"#!/bin/bash
set -eu
case "${1:-}" in
  --version) printf 'story %s\n' "$(cat VERSION)" ;;
  plugin)
    [ "${2:-}" = install ] || exit 90
    printf 'plugin:%s\n' "${3:-}" >> "$RELEASE_TEST_LOG"
    ;;
  *) exit 90 ;;
esac
"#,
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
        storyhook_test_support::install_git_endpoint(
            &fixture.repo.join("bin"),
            &[(
                "https://github.example.com/acme/storyhook.git",
                &fixture.scratch.path().join("origin"),
            )],
        );
        fixture.git(&["add", "bin/git"]);
        fixture.git(&["commit", "-qm", "fixture endpoint"]);
        fixture.git(&["push", "-q", "origin", "dev"]);
        fixture.git(&[
            "remote",
            "set-url",
            "origin",
            "git@github.example.com:acme/storyhook.git",
        ]);

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
            .env("STORY_BIN", env!("CARGO_BIN_EXE_story"))
            .env("GH_CONFIG_DIR", self.scratch.path())
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

    fn run_local_with_plugins(&self) -> Output {
        self.command("bash")
            .arg("scripts/release.sh")
            .args(["--yes", "--skip-daemon", "--local-only"])
            .env("RELEASE_GATE_FAIL", "0")
            .env("RELEASE_BUMP_FAIL", "0")
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
            self.git(&[
                "-C",
                "../origin",
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads/release/"
            ])
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
    let tree = fixture.git(&[
        "-C",
        "../origin",
        "rev-parse",
        "refs/heads/release/v9.9.10^{tree}",
    ]);
    assert_eq!(
        fixture.git(&[
            "-C",
            "../origin",
            "show",
            "refs/heads/release/v9.9.10:BUILD"
        ]),
        "1"
    );
    assert_eq!(
        fixture.calls(),
        format!("bump\ngate:v9.9.10:{tree}\npr:create\n")
    );
}

#[test]
fn public_release_preserves_local_build_advancement() {
    let fixture = ReleaseFixture::new();
    fs::write(fixture.repo.join("BUILD"), "272\n").unwrap();
    let output = fixture.run(false, true, false, false);
    assert_exit(&output, 1);
    assert_eq!(
        fixture.git(&[
            "-C",
            "../origin",
            "show",
            "refs/heads/release/v9.9.10:BUILD"
        ]),
        "273"
    );
    assert_eq!(fixture.git(&["status", "--porcelain"]), "");
}

#[test]
fn release_refuses_unrelated_changes_and_counter_rollback() {
    let fixture = ReleaseFixture::new();
    fs::write(fixture.repo.join("BUILD"), "272\n").unwrap();
    fs::write(fixture.repo.join("unrelated"), "keep\n").unwrap();
    assert_exit(&fixture.run(false, true, false, false), 1);
    assert!(fixture.calls().is_empty());
    assert_eq!(
        fs::read_to_string(fixture.repo.join("BUILD")).unwrap(),
        "272\n"
    );
    fs::remove_file(fixture.repo.join("unrelated")).unwrap();
    fixture.git(&["add", "BUILD"]);
    fixture.git(&["commit", "-qm", "advance build"]);
    fs::write(fixture.repo.join("BUILD"), "271\n").unwrap();
    let output = fixture.run(false, true, false, false);
    assert_exit(&output, 1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("BUILD decreased"));
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
fn local_install_refreshes_both_provider_plugins_after_the_binary() {
    let fixture = ReleaseFixture::new();
    assert_exit(&fixture.run_local_with_plugins(), 0);
    let tree = fixture.git(&["rev-parse", "HEAD^{tree}"]);
    assert_eq!(
        fixture.calls(),
        format!("gate:v9.9.9:{tree}\ninstall:v9.9.9\nplugin:claude\nplugin:codex\n")
    );
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

#[test]
fn installed_daemon_comparison_uses_version_and_optional_build_number() {
    for (installed, running, mismatch) in [
        ("9.9.9", "9.9.9", false),
        ("9.9.9 (100)", "9.9.9 (100)", false),
        ("9.9.9 (100) (build abc123)", "9.9.9 (100)", false),
        (
            "9.9.9-beta.4 (100) (build custom-stamp)",
            "9.9.9-beta.4 (100)",
            false,
        ),
        ("9.9.9 (100) (build abc123)", "9.9.9 (99)", true),
        ("9.9.9 (100)", "9.9.9", true),
        ("9.9.9", "9.9.8", true),
    ] {
        let fixture = ReleaseFixture::new();
        executable(
            &fixture.repo.join("bin/story"),
            r#"#!/bin/bash
set -eu
case "$1 ${2:-}" in
  '--version ') printf 'story %s\n' "$RELEASE_TEST_INSTALLED" ;;
  'daemon status') printf 'storyhook daemon %s running at http://127.0.0.1:12345 (PID 123)\n' "$RELEASE_TEST_RUNNING" ;;
  'daemon stop'|'daemon start') exit 0 ;;
  *) exit 90 ;;
esac
"#,
        );
        fixture.git(&["add", "bin/story"]);
        fixture.git(&["commit", "-qm", "fixture daemon identity"]);
        let output = fixture
            .command("bash")
            .args([
                "scripts/release.sh",
                "--yes",
                "--skip-plugin",
                "--local-only",
            ])
            .env("RELEASE_TEST_INSTALLED", installed)
            .env("RELEASE_TEST_RUNNING", running)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.success(),
            !mismatch,
            "{installed} / {running}: {output:?}"
        );
        assert_eq!(
            stderr.contains("version skew"),
            mismatch,
            "{installed} / {running}: {output:?}"
        );
    }
}

#[test]
fn publish_uses_enterprise_authority_for_api_write_readback_and_web_link() {
    let fixture = ReleaseFixture::new();
    executable(
        &fixture.repo.join("bin/gh"),
        r#"#!/bin/bash
set -eu
[ "$GH_HOST" = github.example.com ] || exit 92
[ "$GH_REPO" = github.example.com/acme/storyhook ] || exit 93
case "$1" in
  api) [[ " $* " = *' --hostname github.example.com '* ]] || exit 94 ;;
  release) [[ " $* " = *' --repo github.example.com/acme/storyhook '* ]] || exit 95 ;;
  *) exit 96 ;;
esac
case "$1 $2" in
  'api repos/acme/storyhook/releases?per_page=100') printf '7\ttrue\n' ;;
  'api repos/acme/storyhook/releases/7/assets?per_page=100')
    source scripts/release-targets.sh
    for artifact in "${RELEASE_ARTIFACTS[@]}"; do printf '%s\tsha256:%064d\n' "$artifact" 0; done ;;
  'release edit') printf '%s\n' "$*" > "$GH_CONFIG_DIR/published" ;;
  'release view') echo v9.9.9 ;;
  *) exit 97 ;;
esac
"#,
    );
    fixture.git(&["add", "bin/gh"]);
    fixture.git(&["commit", "-qm", "explicit publication endpoint fixture"]);
    let output = fixture
        .command("bash")
        .args(["scripts/release.sh", "--publish", "--yes"])
        .env("GH_HOST", "wrong.example")
        .env("GH_REPO", "wrong.example/other/repo")
        .output()
        .unwrap();
    assert_exit(&output, 0);
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("https://github.example.com/acme/storyhook/releases/tag/v9.9.9")
    );
    let write = fs::read_to_string(fixture.scratch.path().join("published")).unwrap();
    assert!(
        write.contains(
            "release edit v9.9.9 --draft=false --repo github.example.com/acme/storyhook"
        )
    );
}
