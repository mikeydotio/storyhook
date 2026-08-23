//! Provider plugin installation through real subprocess boundaries. Every
//! provider CLI is a fake placed on an isolated PATH and writes only beneath
//! the test's HOME, so these tests exercise command construction and failure
//! handling without touching a developer's Codex or Claude configuration.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use storyhook_test_support::scratch_dir;
use tempfile::TempDir;

const FAKE_CODEX: &str = r#"#!/bin/sh
set -u
printf '%s\n' "$*" >> "$HOME/codex-invocations"
mode=""
if [ -f "$HOME/codex-mode" ]; then IFS= read -r mode < "$HOME/codex-mode"; fi

if [ "${1:-}" = "--version" ]; then
  [ "$mode" = "version-fail" ] && exit 12
  echo 'codex-cli 1.2.3'
  exit 0
fi

if [ "${1:-}" = plugin ] && [ "${2:-}" = marketplace ] && [ "${3:-}" = add ]; then
  [ "$mode" = "marketplace-fail" ] && { echo 'marketplace exploded' >&2; exit 17; }
  already=false
  [ "$mode" = "already" ] && already=true
  printf '{"marketplaceName":"storyhook","installedRoot":"%s","alreadyAdded":%s}\n' "$4" "$already"
  exit 0
fi

if [ "${1:-}" = plugin ] && [ "${2:-}" = add ]; then
  [ "$mode" = "plugin-fail" ] && { echo 'plugin exploded' >&2; exit 18; }
  printf '{"pluginId":"story@storyhook","name":"story","marketplaceName":"storyhook","version":"0.6.0","installedPath":"%s/cache/storyhook/story/0.6.0","authPolicy":"ON_INSTALL"}\n' "$HOME"
  exit 0
fi

if [ "${1:-}" = plugin ] && [ "${2:-}" = remove ]; then
  [ "$mode" = "remove-fail" ] && { echo 'unrelated remove failure' >&2; exit 19; }
  printf '{"pluginId":"story@storyhook","name":"story","marketplaceName":"storyhook"}\n'
  exit 0
fi

if [ "${1:-}" = plugin ] && [ "${2:-}" = marketplace ] && [ "${3:-}" = remove ]; then
  [ "$mode" = "marketplace-remove-fail" ] && { echo 'unrelated marketplace failure' >&2; exit 20; }
  if [ "$mode" = "marketplace-absent" ]; then
    echo 'Error: marketplace `storyhook` is not configured or installed' >&2
    exit 1
  fi
  printf '{"marketplaceName":"storyhook","installedRoot":null}\n'
  exit 0
fi

echo "unexpected codex invocation: $*" >&2
exit 64
"#;

const FAKE_CLAUDE: &str = r#"#!/bin/sh
set -u
printf '%s\n' "$*" >> "$HOME/claude-invocations"
if [ "${1:-}" = "--version" ]; then echo 'Claude Code 1.2.3'; exit 0; fi
if [ "${1:-}" = plugin ]; then exit 0; fi
echo "unexpected claude invocation: $*" >&2
exit 64
"#;

struct Harness {
    _temp: TempDir,
    root: PathBuf,
    home: PathBuf,
    fake_bin: PathBuf,
    story: PathBuf,
}

impl Harness {
    fn new(packaged_binary: bool) -> Self {
        let temp = scratch_dir();
        let root = temp.path().join("project");
        let home = temp.path().join("home");
        let fake_bin = temp.path().join("bin");
        fs::create_dir_all(&root).expect("creating fixture project");
        fs::create_dir_all(&home).expect("creating fixture home");
        fs::create_dir_all(&fake_bin).expect("creating fixture bin");

        let built = PathBuf::from(env!("CARGO_BIN_EXE_story"));
        let story = if packaged_binary {
            let copied = temp.path().join("package/story");
            fs::create_dir_all(copied.parent().unwrap()).expect("creating package directory");
            fs::copy(&built, &copied).expect("copying packaged story binary");
            let mut permissions = fs::metadata(&copied).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&copied, permissions).unwrap();
            copied
        } else {
            built
        };

        Self {
            _temp: temp,
            root,
            home,
            fake_bin,
            story,
        }
    }

    fn install_fake(&self, name: &str, body: &str) {
        let path = self.fake_bin.join(name);
        fs::write(&path, body).expect("writing fake provider CLI");
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn set_codex_mode(&self, mode: &str) {
        fs::write(self.home.join("codex-mode"), mode).expect("writing Codex fake mode");
    }

    fn run(&self, args: &[&str]) -> Output {
        let path = format!("{}:/usr/bin:/bin", self.fake_bin.display());
        let data = self.home.join("data");
        let config = self.home.join("config");
        let state = self.home.join("state");
        fs::create_dir_all(&data).unwrap();
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(&state).unwrap();
        Command::new(&self.story)
            .args(args)
            .current_dir(&self.root)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", path)
            .env("TMPDIR", self._temp.path())
            .env("XDG_DATA_HOME", &data)
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_STATE_HOME", &state)
            .env("STORYHOOK_DATA_DIR", data.join("storyhook"))
            .output()
            .expect("running story plugin command")
    }

    fn codex_log(&self) -> String {
        fs::read_to_string(self.home.join("codex-invocations")).unwrap_or_default()
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
fn codex_install_requires_only_an_invokable_cli_not_a_dot_codex_directory() {
    let harness = Harness::new(true);
    harness.install_fake("codex", FAKE_CODEX);
    assert!(!harness.home.join(".codex").exists());

    let output = harness.run(&["plugin", "install", "codex"]);
    assert!(output.status.success(), "{}", combined(&output));
    assert!(!harness.home.join(".codex").exists());
    let message = combined(&output);
    assert!(
        message.contains("Start a new Codex conversation"),
        "{message}"
    );
    assert!(message.contains("story-context"), "{message}");
}

#[test]
fn a_missing_codex_executable_is_actionable() {
    let harness = Harness::new(true);
    let output = harness.run(&["plugin", "install", "codex"]);
    assert!(!output.status.success());
    let message = combined(&output);
    assert!(
        message.contains("Codex CLI (`codex`) not found"),
        "{message}"
    );
    assert!(
        message.contains("codex plugin add story@storyhook"),
        "{message}"
    );
}

#[test]
fn marketplace_and_plugin_failures_stop_at_the_exact_failed_step() {
    let marketplace = Harness::new(true);
    marketplace.install_fake("codex", FAKE_CODEX);
    marketplace.set_codex_mode("marketplace-fail");
    let output = marketplace.run(&["plugin", "install", "codex"]);
    assert!(!output.status.success());
    assert!(combined(&output).contains("marketplace exploded"));
    assert!(!marketplace.codex_log().contains("plugin add"));

    let plugin = Harness::new(true);
    plugin.install_fake("codex", FAKE_CODEX);
    plugin.set_codex_mode("plugin-fail");
    let output = plugin.run(&["plugin", "install", "codex"]);
    assert!(!output.status.success());
    assert!(combined(&output).contains("plugin exploded"));
    assert!(plugin.codex_log().contains("plugin marketplace add"));
    assert!(
        plugin
            .codex_log()
            .contains("plugin add story@storyhook --json")
    );
}

#[test]
fn codex_install_is_idempotent_and_reports_the_marketplace_state() {
    let harness = Harness::new(true);
    harness.install_fake("codex", FAKE_CODEX);
    harness.set_codex_mode("already");
    for _ in 0..2 {
        let output = harness.run(&["plugin", "install", "codex"]);
        assert!(output.status.success(), "{}", combined(&output));
        assert!(combined(&output).contains("already registered"));
    }
    assert_eq!(
        harness
            .codex_log()
            .lines()
            .filter(|line| *line == "plugin add story@storyhook --json")
            .count(),
        2
    );
}

#[test]
fn source_selection_uses_the_checkout_locally_and_github_when_packaged() {
    let local = Harness::new(false);
    local.install_fake("codex", FAKE_CODEX);
    let output = local.run(&["plugin", "install", "codex"]);
    assert!(output.status.success(), "{}", combined(&output));
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(
        local
            .codex_log()
            .contains(&format!("plugin marketplace add {} --json", repo.display())),
        "{}",
        local.codex_log()
    );

    let packaged = Harness::new(true);
    packaged.install_fake("codex", FAKE_CODEX);
    let output = packaged.run(&["plugin", "install", "codex"]);
    assert!(output.status.success(), "{}", combined(&output));
    assert!(
        packaged
            .codex_log()
            .contains("plugin marketplace add mikeydotio/storyhook --json"),
        "{}",
        packaged.codex_log()
    );
}

#[test]
fn codex_uninstall_is_scoped_idempotent_and_preserves_project_files() {
    let harness = Harness::new(true);
    harness.install_fake("codex", FAKE_CODEX);
    harness.set_codex_mode("marketplace-absent");
    let claude_md = harness.root.join("CLAUDE.md");
    let original =
        "before\n<!-- storyhook:begin -->\nkeep for Claude\n<!-- storyhook:end -->\nafter\n";
    fs::write(&claude_md, original).unwrap();
    let agents_md = harness.root.join("AGENTS.md");
    fs::write(
        &agents_md,
        "user before\n<!-- BEGIN STORYHOOK -->\nplugin block\n<!-- END STORYHOOK -->\nuser after\n",
    )
    .unwrap();

    let output = harness.run(&["plugin", "uninstall", "codex"]);
    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(fs::read_to_string(claude_md).unwrap(), original);
    assert_eq!(
        fs::read_to_string(&agents_md).unwrap(),
        "user before\nuser after\n",
        "Codex uninstall removes only the complete sentinel block"
    );
    let log = harness.codex_log();
    assert!(
        log.contains("plugin remove story@storyhook --json"),
        "{log}"
    );
    assert!(
        log.contains("plugin marketplace remove storyhook --json"),
        "{log}"
    );
}

#[test]
fn codex_uninstall_preserves_project_generated_and_malformed_agents_files() {
    for agents in [
        "# canonical project-generated AGENTS.md\nno plugin sentinel\n",
        "user text\n<!-- BEGIN STORYHOOK -->\nunterminated user text\n",
    ] {
        let harness = Harness::new(true);
        harness.install_fake("codex", FAKE_CODEX);
        let path = harness.root.join("AGENTS.md");
        fs::write(&path, agents).unwrap();
        let output = harness.run(&["plugin", "uninstall", "codex"]);
        assert!(output.status.success(), "{}", combined(&output));
        assert_eq!(fs::read_to_string(path).unwrap(), agents);
    }
}

#[test]
fn unrelated_codex_remove_failures_are_not_treated_as_absence() {
    let harness = Harness::new(true);
    harness.install_fake("codex", FAKE_CODEX);
    harness.set_codex_mode("remove-fail");
    let output = harness.run(&["plugin", "uninstall", "codex"]);
    assert!(!output.status.success());
    assert!(combined(&output).contains("unrelated remove failure"));
    assert!(!harness.codex_log().contains("marketplace remove"));
}

#[test]
fn claude_command_sequence_and_success_guidance_use_the_canonical_target() {
    let harness = Harness::new(false);
    harness.install_fake("claude", FAKE_CLAUDE);
    fs::create_dir_all(harness.home.join(".claude")).unwrap();

    let output = harness.run(&["plugin", "install", "claude"]);
    assert!(output.status.success(), "{}", combined(&output));
    let log = fs::read_to_string(harness.home.join("claude-invocations")).unwrap();
    assert!(log.contains("plugin marketplace add"), "{log}");
    assert!(
        log.contains("plugin install story@storyhook --scope user"),
        "{log}"
    );
    let message = combined(&output);
    assert!(
        message.contains("Start a new Claude Code session"),
        "{message}"
    );
    assert!(message.contains("/story-context"), "{message}");
    assert!(!message.contains("deprecated"), "{message}");
}

#[test]
fn legacy_claude_code_target_still_works_and_warns() {
    let harness = Harness::new(false);
    harness.install_fake("claude", FAKE_CLAUDE);
    fs::create_dir_all(harness.home.join(".claude")).unwrap();

    let output = harness.run(&["plugin", "install", "claude-code"]);
    assert!(output.status.success(), "{}", combined(&output));
    let message = combined(&output);
    assert!(message.contains("deprecated"), "{message}");
    assert!(message.contains("use `claude`"), "{message}");
}

#[test]
fn legacy_claude_code_uninstall_target_still_works_and_warns() {
    let harness = Harness::new(false);
    harness.install_fake("claude", FAKE_CLAUDE);
    fs::create_dir_all(harness.home.join(".claude")).unwrap();

    let output = harness.run(&["plugin", "uninstall", "claude-code"]);
    assert!(output.status.success(), "{}", combined(&output));
    let message = combined(&output);
    assert!(message.contains("deprecated"), "{message}");
    assert!(message.contains("use `claude`"), "{message}");
    let log = fs::read_to_string(harness.home.join("claude-invocations")).unwrap();
    assert!(log.contains("plugin uninstall story@storyhook"), "{log}");
}
