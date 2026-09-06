//! A long-lived daemon must not retain the directory of the request that
//! started it when later plugin provider commands run.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use storyhook_test_support::{TestEnv, scratch_dir, story_binary};
use tempfile::TempDir;

const FAKE_CLAUDE: &str = r#"#!/bin/sh
set -eu

if [ "${1:-}" = "--version" ]; then
  echo 'Claude Code 2.1.263'
  exit 0
fi

if [ "$*" = "plugin uninstall story@storyhook" ]; then
  if ! cwd=$(/bin/pwd -P 2>/dev/null); then
    echo "error: The current working directory was deleted, so that command didn't work. Please cd into a different directory and try again." >&2
    exit 1
  fi
  printf '%s\n' "$cwd" > "$HOME/claude-provider-cwd"
  exit 0
fi

if [ "${1:-}" = "plugin" ]; then exit 0; fi

echo "unexpected claude invocation: $*" >&2
exit 64
"#;

const FAKE_CODEX: &str = r#"#!/bin/sh
set -eu

if [ "${1:-}" = "--version" ]; then
  echo 'codex-cli 0.153.4'
  exit 0
fi

if [ "$*" = "plugin remove story@storyhook --json" ]; then
  if ! cwd=$(/bin/pwd -P 2>/dev/null); then
    printf 'Error: failed to load configuration\n\nCaused by:\n    No such file or directory (os error 2)\n' >&2
    exit 1
  fi
  printf '%s\n' "$cwd" > "$HOME/codex-provider-cwd"
  printf '{"pluginId":"story@storyhook","name":"story","marketplaceName":"storyhook"}\n'
  exit 0
fi

if [ "$*" = "plugin marketplace remove storyhook --json" ]; then
  printf '{"marketplaceName":"storyhook","installedRoot":null}\n'
  exit 0
fi

if [ "${1:-}" = "plugin" ] && [ "${2:-}" = "marketplace" ] && [ "${3:-}" = "add" ]; then
  printf '%s\n' "$4" > "$HOME/codex-marketplace-source"
  printf '{"marketplaceName":"storyhook","installedRoot":"%s","alreadyAdded":false}\n' "$4"
  exit 0
fi

if [ "$*" = "plugin add story@storyhook --json" ]; then
  IFS= read -r source < "$HOME/codex-marketplace-source"
  version=$(basename "$source")
  installed="$HOME/.codex/plugins/cache/storyhook/story/$version"
  mkdir -p "$installed"
  cp -Rp "$source/plugins/story/." "$installed/"
  printf '%s\n' "$version" > "$HOME/codex-installed-version"
  printf '{"pluginId":"story@storyhook","name":"story","marketplaceName":"storyhook","version":"%s","installedPath":"%s","authPolicy":"ON_INSTALL"}\n' "$version" "$installed"
  exit 0
fi

if [ "$*" = "plugin list --json" ]; then
  IFS= read -r version < "$HOME/codex-installed-version"
  printf '{"installed":[{"pluginId":"story@storyhook","name":"story","marketplaceName":"storyhook","version":"%s","installed":true,"enabled":true}]}\n' "$version"
  exit 0
fi

if [ "${1:-}" = "execpolicy" ] && [ "${2:-}" = "check" ]; then
  printf '{"matchedRules":[{"prefixRuleMatch":{"decision":"allow"}}],"decision":"allow"}\n'
  exit 0
fi

echo "unexpected codex invocation: $*" >&2
exit 64
"#;

struct ReinstallFixture {
    env: TestEnv,
    _scratch: TempDir,
    startup: PathBuf,
    stable: PathBuf,
    bin: PathBuf,
}

impl ReinstallFixture {
    fn new(provider: &str, fake: &str) -> Self {
        let env = TestEnv::isolated();
        let scratch = scratch_dir();
        let startup = scratch.path().join("startup");
        let stable = scratch.path().join("stable");
        let bin = scratch.path().join("bin");
        let installed = env.home().join(format!(
            ".{provider}/plugins/cache/storyhook/story/existing"
        ));
        fs::create_dir_all(&installed).expect("seeding the existing plugin directory");
        fs::create_dir_all(&startup).expect("creating the daemon startup directory");
        fs::create_dir_all(&stable).expect("creating the stable client directory");
        fs::create_dir_all(&bin).expect("creating the fake provider PATH");

        let executable = bin.join(provider);
        fs::write(&executable, fake).expect("writing the fake provider CLI");
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).unwrap();

        Self {
            env,
            _scratch: scratch,
            startup,
            stable,
            bin,
        }
    }

    fn story(&self, cwd: &Path) -> Command {
        let mut command = Command::new(story_binary());
        command.current_dir(cwd).env_clear();
        self.env.apply(&mut command);
        command.env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()));
        command
    }

    fn start_daemon_then_remove_startup_directory(&self) -> u32 {
        let output = self
            .story(&self.startup)
            .args(["project", "list"])
            .output()
            .expect("running the harmless auto-spawn request");
        assert!(output.status.success(), "{}", combined(&output));

        let info = self
            .env
            .daemon()
            .expect("the auto-spawn request must publish a daemon portfile");
        assert!(
            self.env.daemon_is_live(),
            "the auto-spawned daemon must still hold its pidfile"
        );
        fs::remove_dir(&self.startup).expect("removing the daemon startup directory");
        info.pid
    }

    fn reinstall(&self, provider: &str) -> Output {
        self.story(&self.stable)
            .args(["plugin", "install", provider])
            .output()
            .expect("running the subsequent plugin install request")
    }

    fn assert_same_daemon(&self, original_pid: u32) {
        let current = self
            .env
            .daemon()
            .expect("the original daemon must retain its portfile");
        assert_eq!(current.pid, original_pid, "the daemon must not be replaced");
        assert!(
            self.env.daemon_is_live(),
            "the original daemon must remain live"
        );
    }

    fn assert_provider_cwd_is_home(&self, provider: &str) {
        let observed = fs::read_to_string(self.env.home().join(format!("{provider}-provider-cwd")))
            .expect("the fake provider must record its actual working directory");
        let expected = fs::canonicalize(self.env.home()).expect("canonicalizing the test home");
        assert_eq!(Path::new(observed.trim()), expected);
    }
}

impl Drop for ReinstallFixture {
    fn drop(&mut self) {
        self.env.stop_daemon();
    }
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn claude_reinstall_uses_stable_cwd_after_daemon_startup_directory_is_deleted() {
    let fixture = ReinstallFixture::new("claude", FAKE_CLAUDE);
    let daemon_pid = fixture.start_daemon_then_remove_startup_directory();

    let output = fixture.reinstall("claude");

    assert!(output.status.success(), "{}", combined(&output));
    fixture.assert_same_daemon(daemon_pid);
    fixture.assert_provider_cwd_is_home("claude");
}

#[test]
fn codex_reinstall_uses_stable_cwd_after_daemon_startup_directory_is_deleted() {
    let fixture = ReinstallFixture::new("codex", FAKE_CODEX);
    let daemon_pid = fixture.start_daemon_then_remove_startup_directory();

    let output = fixture.reinstall("codex");

    assert!(output.status.success(), "{}", combined(&output));
    fixture.assert_same_daemon(daemon_pid);
    fixture.assert_provider_cwd_is_home("codex");
}
