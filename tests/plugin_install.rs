//! Provider plugin installation through real subprocess boundaries. Every
//! provider CLI is a fake placed on an isolated PATH and writes only beneath
//! the test's HOME, so these tests exercise command construction and failure
//! handling without touching a developer's Codex or Claude configuration.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use storyhook_test_support::{daemon_containment, scratch_dir};
use tempfile::TempDir;

#[path = "support/protect_domain.rs"]
mod protect_domain;
#[path = "support/protect_helper.rs"]
mod protect_helper;
#[path = "support/protect_launcher.rs"]
mod protect_launcher;

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
  if [ "$mode" = "marketplace-fail-once" ] && [ ! -f "$HOME/codex-marketplace-add-failed" ]; then
    : > "$HOME/codex-marketplace-add-failed"
    echo 'marketplace exploded' >&2; exit 17
  fi
  already=false
  if [ -f "$HOME/codex-marketplace-version" ]; then
    already=true
  else
    basename "$4" > "$HOME/codex-marketplace-version"
    printf '%s\n' "$4" > "$HOME/codex-marketplace-source"
  fi
  IFS= read -r root < "$HOME/codex-marketplace-source"
  # The registration in Codex's own config shape, which the installer reads
  # back to learn what it is about to replace.
  mkdir -p "$HOME/.codex"
  printf '[marketplaces.storyhook]\nsource_type = "local"\nsource = "%s"\n' "$root" > "$HOME/.codex/config.toml"
  printf '{"marketplaceName":"storyhook","installedRoot":"%s","alreadyAdded":%s}\n' "$root" "$already"
  exit 0
fi

if [ "${1:-}" = plugin ] && [ "${2:-}" = add ]; then
  [ "$mode" = "plugin-fail" ] && { echo 'plugin exploded' >&2; exit 18; }
  if [ "$mode" = "plugin-fail-once" ] && [ ! -f "$HOME/codex-plugin-add-failed" ]; then
    : > "$HOME/codex-plugin-add-failed"
    echo 'plugin exploded' >&2; exit 18
  fi
  IFS= read -r version < "$HOME/codex-marketplace-version"
  printf '%s\n' "$version" > "$HOME/codex-installed-version"
  IFS= read -r source < "$HOME/codex-marketplace-source"
  installed="$HOME/.codex/plugins/cache/storyhook/story/$version"
  mkdir -p "$installed"
  cp -Rp "$source/plugins/story/." "$installed/" || exit 22
  case "$mode" in
    payload-missing) rm "$installed/bin/story.sh" ;;
    payload-stale) printf '%s\n' 'stale helper' > "$installed/bin/story.sh" ;;
    payload-mode) chmod 644 "$installed/bin/story.sh" ;;
    payload-extra) printf '%s\n' 'unexpected' > "$installed/extra.sh" ;;
    payload-link) mv "$installed/bin/story.sh" "$HOME/original-helper"; ln -s "$HOME/original-helper" "$installed/bin/story.sh" ;;
    divergent-path) installed="$HOME/unrelated-plugin" ;;
  esac
  printf '{"pluginId":"story@storyhook","name":"story","marketplaceName":"storyhook","version":"%s","installedPath":"%s","authPolicy":"ON_INSTALL"}\n' "$version" "$installed"
  exit 0
fi

if [ "${1:-}" = plugin ] && [ "${2:-}" = list ]; then
  version=0.6.0
  if [ -f "$HOME/codex-installed-version" ]; then IFS= read -r version < "$HOME/codex-installed-version"; fi
  if [ -f "$HOME/codex-plugin-version" ]; then IFS= read -r version < "$HOME/codex-plugin-version"; fi
  [ "$mode" = "list-fail" ] && { echo 'list exploded' >&2; exit 23; }
  enabled=true
  [ "$mode" = "disabled" ] && enabled=false
  printf '{"installed":[{"pluginId":"story@storyhook","name":"story","marketplaceName":"storyhook","version":"%s","installed":true,"enabled":%s}]}\n' "$version" "$enabled"
  exit 0
fi

if [ "${1:-}" = execpolicy ] && [ "${2:-}" = check ]; then
  [ "$mode" = "execpolicy-fail" ] && { echo 'rule verification exploded' >&2; exit 21; }
  printf '{"matchedRules":[{"prefixRuleMatch":{"decision":"allow"}}],"decision":"allow"}\n'
  exit 0
fi

if [ "${1:-}" = plugin ] && [ "${2:-}" = remove ]; then
  [ "$mode" = "remove-fail" ] && { echo 'unrelated remove failure' >&2; exit 19; }
  rm -f "$HOME/codex-installed-version"
  printf '{"pluginId":"story@storyhook","name":"story","marketplaceName":"storyhook"}\n'
  exit 0
fi

if [ "${1:-}" = plugin ] && [ "${2:-}" = marketplace ] && [ "${3:-}" = remove ]; then
  [ "$mode" = "marketplace-remove-fail" ] && { echo 'unrelated marketplace failure' >&2; exit 20; }
  if [ "$mode" = "marketplace-absent" ]; then
    echo 'Error: marketplace `storyhook` is not configured or installed' >&2
    exit 1
  fi
  rm -f "$HOME/codex-marketplace-version" "$HOME/codex-marketplace-source" "$HOME/.codex/config.toml"
  printf '{"marketplaceName":"storyhook","installedRoot":null}\n'
  exit 0
fi

echo "unexpected codex invocation: $*" >&2
exit 64
"#;

const FAKE_CLAUDE: &str = r#"#!/bin/sh
set -u
printf '%s\n' "$*" >> "$HOME/claude-invocations"
mode=""
if [ -f "$HOME/claude-mode" ]; then IFS= read -r mode < "$HOME/claude-mode"; fi
if [ "${1:-}" = "--version" ]; then echo 'Claude Code 1.2.3'; exit 0; fi
if [ "${1:-}" = plugin ] && [ "${2:-}" = uninstall ]; then
  if [ "$mode" = "plugin-absent" ]; then
    echo 'Failed to uninstall plugin "story@storyhook": Plugin "story@storyhook" not found in installed plugins' >&2
    exit 1
  fi
  [ "$mode" = "uninstall-fail" ] && { echo 'unrelated uninstall failure' >&2; exit 19; }
  rm -f "$HOME/claude-installed"
  exit 0
fi
if [ "${1:-}" = plugin ] && [ "${2:-}" = marketplace ] && [ "${3:-}" = add ]; then
  [ "$mode" = "marketplace-add-fail" ] && { echo 'marketplace add exploded' >&2; exit 17; }
  if [ "$mode" = "marketplace-add-fail-once" ] && [ ! -f "$HOME/claude-marketplace-add-failed" ]; then
    : > "$HOME/claude-marketplace-add-failed"
    echo 'marketplace add exploded' >&2; exit 17
  fi
  # The registration in Claude Code's own config shape (the one this
  # machine's real known_marketplaces.json uses for a directory source),
  # which the installer reads back to learn what it is about to replace.
  mkdir -p "$HOME/.claude/plugins"
  printf '{"storyhook":{"source":{"source":"directory","path":"%s"},"installLocation":"%s"}}\n' "$4" "$HOME/.claude/plugins/marketplaces/storyhook" > "$HOME/.claude/plugins/known_marketplaces.json"
  exit 0
fi
if [ "${1:-}" = plugin ] && [ "${2:-}" = marketplace ] && [ "${3:-}" = remove ]; then
  mkdir -p "$HOME/.claude/plugins"
  printf '{}\n' > "$HOME/.claude/plugins/known_marketplaces.json"
  exit 0
fi
if [ "${1:-}" = plugin ] && [ "${2:-}" = install ]; then
  [ "$mode" = "plugin-install-fail" ] && { echo 'plugin install exploded' >&2; exit 18; }
  if [ "$mode" = "plugin-install-fail-once" ] && [ ! -f "$HOME/claude-plugin-install-failed" ]; then
    : > "$HOME/claude-plugin-install-failed"
    echo 'plugin install exploded' >&2; exit 18
  fi
  : > "$HOME/claude-installed"
  exit 0
fi
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

    fn set_claude_mode(&self, mode: &str) {
        fs::write(self.home.join("claude-mode"), mode).expect("writing Claude fake mode");
    }

    /// One `story` invocation against this fixture's isolated home.
    ///
    /// `env_clear` is the point of this harness — a provider CLI must be found
    /// on the fixture's own `PATH` and nowhere else — but it also clears the
    /// containment `scripts/run-tests.sh` exports for the whole run, and since
    /// SH-114 every `story` starts a daemon. Without
    /// [`daemon_containment`] reinstated the child asked for port 3456, the
    /// port a developer's own dashboard uses, and had no parent to die with:
    /// this one file leaked 20 daemons per run (one per `Harness`), and the
    /// 16 built from a packaged copy were invisible to
    /// `scripts/check-no-orphan-servers.sh` as well, so they accumulated —
    /// 672 alive across three days when SH-493 was measured.
    fn run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(&self.story);
        command.args(args);
        self.run_with(command)
    }

    /// [`Harness::run`] for a caller that has already built the command — to
    /// add an environment variable on top of the isolated set, never instead
    /// of it.
    fn run_with(&self, mut command: Command) -> Output {
        let path = format!("{}:/usr/bin:/bin", self.fake_bin.display());
        let data = self.home.join("data");
        let config = self.home.join("config");
        let state = self.home.join("state");
        fs::create_dir_all(&data).unwrap();
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(&state).unwrap();
        let preset: Vec<(String, String)> = command
            .get_envs()
            .filter_map(|(key, value)| {
                Some((key.to_str()?.to_owned(), value?.to_str()?.to_owned()))
            })
            .collect();
        command
            .current_dir(&self.root)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", path)
            .env("TMPDIR", self._temp.path())
            .env("XDG_DATA_HOME", &data)
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_STATE_HOME", &state)
            .env("STORYHOOK_DATA_DIR", data.join("storyhook"))
            .envs(daemon_containment())
            .envs(preset);
        command.output().expect("running story plugin command")
    }

    fn codex_log(&self) -> String {
        fs::read_to_string(self.home.join("codex-invocations")).unwrap_or_default()
    }

    fn claude_log(&self) -> String {
        fs::read_to_string(self.home.join("claude-invocations")).unwrap_or_default()
    }

    fn codex_launcher(&self) -> PathBuf {
        self.home.join(".codex/storyhook/story.sh")
    }

    fn codex_rule(&self) -> PathBuf {
        self.home.join(".codex/rules/storyhook.rules")
    }

    fn release_marketplace(&self) -> PathBuf {
        self.home
            .join("data/storyhook/plugins")
            .join(env!("CARGO_PKG_VERSION"))
    }

    fn set_codex_plugin_version(&self, version: &str) {
        fs::write(self.home.join("codex-plugin-version"), version)
            .expect("writing fake Codex plugin version");
    }

    fn seed_codex_install(&self, version: &str, marketplace_source: &str) {
        fs::write(self.home.join("codex-marketplace-version"), version)
            .expect("seeding fake Codex marketplace version");
        fs::write(
            self.home.join("codex-marketplace-source"),
            marketplace_source,
        )
        .expect("seeding fake Codex marketplace source");
        fs::write(self.home.join("codex-installed-version"), version)
            .expect("seeding fake Codex plugin version");
    }

    fn codex_marketplace_source(&self) -> String {
        fs::read_to_string(self.home.join("codex-marketplace-source"))
            .expect("reading fake Codex marketplace source")
    }

    fn codex_installed_version(&self) -> String {
        fs::read_to_string(self.home.join("codex-installed-version"))
            .expect("reading fake Codex plugin version")
    }

    fn install_fake_plugin_helper(&self, version: &str, body: &str) -> PathBuf {
        let root = self
            .home
            .join(".codex/plugins/cache/storyhook/story")
            .join(version);
        fs::create_dir_all(root.join(".codex-plugin")).unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(root.join(".codex-plugin/plugin.json"), "{}\n").unwrap();
        fs::write(root.join("bin/story.sh"), body).unwrap();
        self.set_codex_plugin_version(version);
        root
    }

    /// Puts `story` on the fixture's `PATH`, and from then on runs it from
    /// there.
    ///
    /// The launcher this fixture installs execs the `story` on its `PATH`,
    /// so after this call the fixture models an installed machine — and on
    /// an installed machine the `story` a person types and the one the
    /// launcher resolves are one file. This used to leave [`Harness::run`]
    /// on the original binary, so a test that alternated the two ran two
    /// copies of one build against one store, each call standing the
    /// other's daemon down and seating its own: the SH-634 ping-pong, paid
    /// silently per call. The seat guard now refuses that from the
    /// uninstalled side (`Harness::new(false)`'s `target/debug/story`), which
    /// is how the fixture was found to be doing it.
    fn install_story_on_path(&mut self) {
        let path = self.fake_bin.join("story");
        fs::copy(&self.story, &path).expect("copying story onto fixture PATH");
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        self.story = path;
    }

    /// The installed launcher, run the way Codex runs it.
    ///
    /// Carries [`daemon_containment`] for the same reason [`Harness::run`]
    /// does, and needs it just as much: the launcher's whole job is to exec
    /// the `story` this fixture put on its `PATH`.
    fn run_launcher(&self, args: &[&str]) -> Output {
        let path = format!("{}:/usr/bin:/bin", self.fake_bin.display());
        let mut command = Command::new("bash");
        command
            .arg(self.codex_launcher())
            .args(args)
            .current_dir(&self.root)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", path)
            .env("TMPDIR", self._temp.path())
            .envs(daemon_containment());
        command.output().expect("running the stable Codex launcher")
    }

    /// A marketplace registered before this install ran, at a source that is
    /// not this binary's release: a real directory, because the fake Codex
    /// `plugin add` copies the payload out of whatever source it was handed.
    fn seed_previous_registration(&self, provider: &str) -> PathBuf {
        let previous = self.home.join("previous/1.0.0");
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
        for relative in [
            ".agents/plugins/marketplace.json",
            ".claude-plugin/marketplace.json",
        ] {
            let target = previous.join(relative);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::copy(repository.join(relative), target).unwrap();
        }
        copy_tree(
            &repository.join("plugins/story"),
            &previous.join("plugins/story"),
        );
        let source = previous.display().to_string();
        match provider {
            "claude" => {
                let plugins = self.home.join(".claude/plugins");
                fs::create_dir_all(&plugins).unwrap();
                fs::write(
                    plugins.join("known_marketplaces.json"),
                    serde_json::to_vec_pretty(&serde_json::json!({
                        "storyhook": {
                            "source": { "source": "directory", "path": source },
                            "installLocation": plugins.join("marketplaces/storyhook"),
                        }
                    }))
                    .unwrap(),
                )
                .unwrap();
                fs::write(self.home.join("claude-installed"), "").unwrap();
            }
            "codex" => {
                self.seed_codex_install("1.0.0", &source);
                fs::create_dir_all(self.home.join(".codex")).unwrap();
                fs::write(
                    self.home.join(".codex/config.toml"),
                    format!(
                        "[marketplaces.storyhook]\nsource_type = \"local\"\nsource = \"{source}\"\n"
                    ),
                )
                .unwrap();
            }
            other => panic!("unknown provider {other}"),
        }
        previous
    }

    /// The storyhook marketplace source a provider's config names now, read
    /// the way the installer and `story doctor install` read it.
    fn registered_source(&self, provider: &str) -> Option<String> {
        match provider {
            "claude" => {
                let path = self.home.join(".claude/plugins/known_marketplaces.json");
                let body = fs::read(&path).ok()?;
                let value: serde_json::Value = serde_json::from_slice(&body)
                    .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                value
                    .get("storyhook")?
                    .get("source")?
                    .get("path")?
                    .as_str()
                    .map(str::to_string)
            }
            "codex" => {
                let body = fs::read_to_string(self.home.join(".codex/config.toml")).ok()?;
                body.lines()
                    .find_map(|line| line.strip_prefix("source = \""))
                    .map(|rest| rest.trim_end_matches('"').to_string())
            }
            other => panic!("unknown provider {other}"),
        }
    }

    fn provider_log(&self, provider: &str) -> String {
        match provider {
            "claude" => self.claude_log(),
            "codex" => self.codex_log(),
            other => panic!("unknown provider {other}"),
        }
    }

    fn set_mode(&self, provider: &str, mode: &str) {
        match provider {
            "claude" => self.set_claude_mode(mode),
            "codex" => self.set_codex_mode(mode),
            other => panic!("unknown provider {other}"),
        }
    }

    /// A provider fixture ready to install: the fake CLI on PATH and, for
    /// Claude, the `~/.claude` its preflight requires.
    fn for_provider(provider: &str) -> Self {
        let harness = Harness::new(provider == "codex");
        match provider {
            "claude" => {
                harness.install_fake("claude", FAKE_CLAUDE);
                fs::create_dir_all(harness.home.join(".claude")).unwrap();
            }
            "codex" => harness.install_fake("codex", FAKE_CODEX),
            other => panic!("unknown provider {other}"),
        }
        harness
    }
}

fn copy_tree(from: &Path, to: &Path) {
    for (relative, (bytes, executable)) in regular_files(from) {
        let target = to.join(relative);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, bytes).unwrap();
        fs::set_permissions(
            &target,
            fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }),
        )
        .unwrap();
    }
}

/// Byte offsets of each expected provider invocation, in the order the log
/// records them; panics naming the first one missing or out of order.
fn assert_log_order(log: &str, expected: &[String]) {
    let mut cursor = 0;
    for line in expected {
        let Some(at) = log[cursor..].find(line.as_str()) else {
            panic!("expected `{line}` after byte {cursor} of the provider log:\n{log}");
        };
        cursor += at + line.len();
    }
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn regular_files(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, bool)> {
    fn visit(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, (Vec<u8>, bool)>) {
        let mut entries: Vec<_> = fs::read_dir(directory)
            .unwrap_or_else(|error| panic!("reading {}: {error}", directory.display()))
            .map(|entry| entry.expect("reading marketplace entry"))
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).expect("reading marketplace metadata");
            if metadata.is_dir() {
                visit(root, &path, files);
            } else {
                assert!(
                    metadata.is_file(),
                    "unexpected payload entry: {}",
                    path.display()
                );
                files.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    (
                        fs::read(&path).expect("reading marketplace file"),
                        metadata.permissions().mode() & 0o111 != 0,
                    ),
                );
            }
        }
    }

    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

fn expected_marketplace() -> BTreeMap<PathBuf, (Vec<u8>, bool)> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut expected = BTreeMap::new();
    for relative in [
        Path::new(".agents/plugins/marketplace.json"),
        Path::new(".claude-plugin/marketplace.json"),
    ] {
        expected.insert(
            relative.to_path_buf(),
            (fs::read(repository.join(relative)).unwrap(), false),
        );
    }
    for (relative, value) in regular_files(&repository.join("plugins/story")) {
        expected.insert(Path::new("plugins/story").join(relative), value);
    }
    expected
}

/// The provider's own spelling of each registration verb, as the fakes log it.
fn verb(provider: &str, step: &str, source: &Path) -> String {
    let source = source.display();
    match (provider, step) {
        ("claude", "remove-plugin") => "plugin uninstall story@storyhook".into(),
        ("claude", "remove-marketplace") => "plugin marketplace remove storyhook".into(),
        ("claude", "add-marketplace") => format!("plugin marketplace add {source} --scope user"),
        ("claude", "add-plugin") => "plugin install story@storyhook --scope user".into(),
        ("codex", "remove-plugin") => "plugin remove story@storyhook --json".into(),
        ("codex", "remove-marketplace") => "plugin marketplace remove storyhook --json".into(),
        ("codex", "add-marketplace") => format!("plugin marketplace add {source} --json"),
        ("codex", "add-plugin") => "plugin add story@storyhook --json".into(),
        other => panic!("unknown verb {other:?}"),
    }
}

/// The failure modes that break one step AFTER the removes, once, for each
/// provider, paired with the text the fake prints when it does.
fn failures_after_the_removes(provider: &str) -> Vec<(&'static str, &'static str)> {
    match provider {
        "claude" => vec![
            ("marketplace-add-fail-once", "marketplace add exploded"),
            ("plugin-install-fail-once", "plugin install exploded"),
        ],
        "codex" => vec![
            ("marketplace-fail-once", "marketplace exploded"),
            ("plugin-fail-once", "plugin exploded"),
            ("payload-stale", "verify"),
            ("execpolicy-fail", "rule verification exploded"),
        ],
        other => panic!("unknown provider {other}"),
    }
}

#[test]
fn a_failure_after_the_removes_re_registers_the_previous_marketplace() {
    for provider in ["claude", "codex"] {
        for (mode, error) in failures_after_the_removes(provider) {
            let harness = Harness::for_provider(provider);
            let previous = harness.seed_previous_registration(provider);
            harness.set_mode(provider, mode);

            let output = harness.run(&["plugin", "install", provider]);
            let message = combined(&output);
            assert!(!output.status.success(), "{provider}/{mode}: {message}");
            assert!(message.contains(error), "{provider}/{mode}: {message}");
            assert!(
                message.contains(&format!(
                    "re-registered the previous marketplace at {}",
                    previous.display()
                )),
                "{provider}/{mode}: {message}"
            );
            assert!(
                !message.contains("not verified"),
                "{provider}/{mode}: a different source was put back, not the same one: {message}"
            );
            assert_eq!(
                harness.registered_source(provider).as_deref(),
                Some(previous.display().to_string().as_str()),
                "{provider}/{mode}: the provider must be registered at the previous source again"
            );

            let release = harness.release_marketplace();
            let mut expected = vec![
                verb(provider, "remove-plugin", &release),
                verb(provider, "remove-marketplace", &release),
                verb(provider, "add-marketplace", &release),
            ];
            if !mode.starts_with("marketplace") {
                expected.push(verb(provider, "add-plugin", &release));
            }
            expected.extend([
                verb(provider, "remove-plugin", &previous),
                verb(provider, "remove-marketplace", &previous),
                verb(provider, "add-marketplace", &previous),
                verb(provider, "add-plugin", &previous),
            ]);
            assert_log_order(&harness.provider_log(provider), &expected);
            if provider == "codex" {
                assert!(!harness.codex_launcher().exists(), "{mode}");
                assert!(!harness.codex_rule().exists(), "{mode}");
            }
        }
    }
}

#[test]
fn a_fresh_install_that_fails_removes_its_partial_registration() {
    for (provider, mode, error) in [
        ("claude", "plugin-install-fail", "plugin install exploded"),
        ("codex", "plugin-fail", "plugin exploded"),
    ] {
        let harness = Harness::for_provider(provider);
        harness.set_mode(provider, mode);
        assert_eq!(harness.registered_source(provider), None);

        let output = harness.run(&["plugin", "install", provider]);
        let message = combined(&output);
        assert!(!output.status.success(), "{provider}: {message}");
        assert!(message.contains(error), "{provider}: {message}");
        assert!(
            message.contains("removed the partial registration"),
            "{provider}: {message}"
        );
        assert!(!message.contains("re-registered"), "{provider}: {message}");
        assert_eq!(
            harness.registered_source(provider),
            None,
            "{provider}: a marketplace the failed install added must not survive it"
        );
        let release = harness.release_marketplace();
        assert_log_order(
            &harness.provider_log(provider),
            &[
                verb(provider, "add-marketplace", &release),
                verb(provider, "add-plugin", &release),
                verb(provider, "remove-plugin", &release),
                verb(provider, "remove-marketplace", &release),
            ],
        );
        let adds = harness
            .provider_log(provider)
            .matches("marketplace add")
            .count();
        assert_eq!(
            adds, 1,
            "{provider}: nothing to re-register, so no second add"
        );
    }
}

#[test]
fn a_failed_restore_reports_both_errors_and_names_the_previous_source() {
    for (provider, mode, error) in [
        ("claude", "marketplace-add-fail", "marketplace add exploded"),
        ("codex", "marketplace-fail", "marketplace exploded"),
    ] {
        let harness = Harness::for_provider(provider);
        let previous = harness.seed_previous_registration(provider);
        harness.set_mode(provider, mode);

        let output = harness.run(&["plugin", "install", provider]);
        let message = combined(&output);
        assert!(!output.status.success(), "{provider}: {message}");
        assert_eq!(
            message.matches(error).count(),
            2,
            "{provider}: the original failure and the restore's own must both be reported: {message}"
        );
        assert!(
            message.contains(&format!(
                "AND failed to re-register the previous marketplace at {}",
                previous.display()
            )),
            "{provider}: {message}"
        );
        assert!(
            !message.contains("re-registered the previous"),
            "{provider}: a failed restore must not read as a successful one: {message}"
        );
        assert_eq!(
            harness.registered_source(provider),
            None,
            "{provider}: the fake's marketplace add never succeeded"
        );
    }
}

#[test]
fn a_same_source_reinstall_failure_says_what_it_put_back_is_unverified() {
    for (provider, mode) in [
        ("claude", "plugin-install-fail-once"),
        ("codex", "plugin-fail-once"),
    ] {
        let harness = Harness::for_provider(provider);
        let first = harness.run(&["plugin", "install", provider]);
        assert!(first.status.success(), "{provider}: {}", combined(&first));
        let release = harness.release_marketplace();
        assert_eq!(
            harness.registered_source(provider).as_deref(),
            Some(release.display().to_string().as_str())
        );

        harness.set_mode(provider, mode);
        let output = harness.run(&["plugin", "install", provider]);
        let message = combined(&output);
        assert!(!output.status.success(), "{provider}: {message}");
        assert!(
            message.contains(&format!(
                "re-registered the same release source {}",
                release.display()
            )),
            "{provider}: {message}"
        );
        assert!(message.contains("not verified"), "{provider}: {message}");
        assert!(
            message.contains(&format!("story plugin install {provider}")),
            "{provider}: {message}"
        );
        assert_eq!(
            harness.registered_source(provider).as_deref(),
            Some(release.display().to_string().as_str()),
            "{provider}: the registration is back even though this run failed"
        );
    }
}

#[test]
fn an_unreadable_provider_config_never_blocks_the_install_and_is_named_on_failure() {
    for (provider, config, body, mode, reason) in [
        (
            "claude",
            ".claude/plugins/known_marketplaces.json",
            "{ not json",
            "plugin-install-fail",
            "nothing restored: its configuration is invalid JSON",
        ),
        (
            "codex",
            ".codex/config.toml",
            "[marketplaces.storyhook\nsource = 1",
            "plugin-fail",
            "nothing restored: its configuration is invalid TOML",
        ),
    ] {
        let unblocked = Harness::for_provider(provider);
        let path = unblocked.home.join(config);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
        let output = unblocked.run(&["plugin", "install", provider]);
        assert!(
            output.status.success(),
            "{provider}: a config the parser cannot read must not make install a dead end: {}",
            combined(&output)
        );

        let failed = Harness::for_provider(provider);
        let path = failed.home.join(config);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
        failed.set_mode(provider, mode);
        let output = failed.run(&["plugin", "install", provider]);
        let message = combined(&output);
        assert!(!output.status.success(), "{provider}: {message}");
        assert!(message.contains(reason), "{provider}: {message}");
        assert!(!message.contains("re-registered"), "{provider}: {message}");
        // The partial registration is still undone: there was something to
        // remove even though there was nothing to put back.
        let log = failed.provider_log(provider);
        let release = failed.release_marketplace();
        assert_log_order(
            &log,
            &[
                verb(provider, "add-plugin", &release),
                verb(provider, "remove-plugin", &release),
                verb(provider, "remove-marketplace", &release),
            ],
        );
    }
}

#[test]
fn a_successful_reinstall_over_a_previous_registration_runs_no_restore() {
    for provider in ["claude", "codex"] {
        let harness = Harness::for_provider(provider);
        harness.seed_previous_registration(provider);
        let output = harness.run(&["plugin", "install", provider]);
        assert!(output.status.success(), "{provider}: {}", combined(&output));
        let release = harness.release_marketplace();
        assert_eq!(
            harness.registered_source(provider).as_deref(),
            Some(release.display().to_string().as_str())
        );
        let log = harness.provider_log(provider);
        for step in [
            "remove-plugin",
            "remove-marketplace",
            "add-marketplace",
            "add-plugin",
        ] {
            let line = verb(provider, step, &release);
            assert_eq!(
                log.lines().filter(|l| *l == line).count(),
                1,
                "{provider}: `{line}` must run exactly once on success:\n{log}"
            );
        }
        assert!(!combined(&output).contains("re-registered"));
    }
}

#[test]
fn codex_install_creates_the_stable_launcher_and_verified_rule() {
    let harness = Harness::new(true);
    harness.install_fake("codex", FAKE_CODEX);
    assert!(!harness.home.join(".codex").exists());

    let output = harness.run(&["plugin", "install", "codex"]);
    assert!(output.status.success(), "{}", combined(&output));
    let launcher = fs::read_to_string(harness.codex_launcher()).unwrap();
    assert!(launcher.starts_with("# storyhook-managed: codex-launcher-v1"));
    assert!(
        launcher.contains("plugin run codex -- \"$@\""),
        "{launcher}"
    );
    assert!(!launcher.contains("plugins/cache"), "{launcher}");
    #[cfg(unix)]
    assert_ne!(
        fs::metadata(harness.codex_launcher())
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0,
        "the stable launcher is executable even though skills invoke it via bash"
    );
    let rule = fs::read_to_string(harness.codex_rule()).unwrap();
    assert!(rule.starts_with("# storyhook-managed: codex-rules-v1"));
    assert!(
        rule.contains(&format!(
            "pattern = [\"bash\", \"{}\"]",
            harness.codex_launcher().display()
        )),
        "{rule}"
    );
    assert!(!rule.contains("plugins/cache"), "{rule}");
    assert!(
        harness
            .codex_log()
            .contains("execpolicy check --pretty --rules"),
        "{}",
        harness.codex_log()
    );
    let message = combined(&output);
    assert!(message.contains("Restart Codex"), "{message}");
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
        message.contains("retry `story plugin install codex`"),
        "{message}"
    );
    assert!(
        !harness.release_marketplace().exists(),
        "provider preflight must happen before materialization"
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
    assert!(
        combined(&output).contains("removed the partial registration"),
        "{}",
        combined(&output)
    );

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
fn registration_cleanup_failures_stop_before_the_release_source_is_added() {
    let harness = Harness::new(true);
    harness.install_fake("codex", FAKE_CODEX);
    harness.set_codex_mode("remove-fail");

    let output = harness.run(&["plugin", "install", "codex"]);

    assert!(!output.status.success());
    assert!(combined(&output).contains("unrelated remove failure"));
    let log = harness.codex_log();
    assert!(!log.contains("plugin marketplace remove"), "{log}");
    assert!(!log.contains("plugin marketplace add"), "{log}");
}

#[test]
fn codex_install_is_idempotent_and_replaces_the_owned_registration() {
    let harness = Harness::new(true);
    harness.install_fake("codex", FAKE_CODEX);
    for _ in 0..2 {
        let output = harness.run(&["plugin", "install", "codex"]);
        assert!(output.status.success(), "{}", combined(&output));
        assert!(combined(&output).contains("registered"));
    }
    for invocation in [
        "plugin remove story@storyhook --json",
        "plugin marketplace remove storyhook --json",
        "plugin add story@storyhook --json",
    ] {
        assert_eq!(
            harness
                .codex_log()
                .lines()
                .filter(|line| *line == invocation)
                .count(),
            2,
            "each install must repeat `{invocation}`"
        );
    }
    let launcher = fs::read_to_string(harness.codex_launcher()).unwrap();
    assert_eq!(
        launcher
            .matches("storyhook-managed: codex-launcher-v1")
            .count(),
        1
    );
    let rule = fs::read_to_string(harness.codex_rule()).unwrap();
    assert_eq!(rule.matches("prefix_rule(").count(), 1);
}

#[test]
fn codex_install_preserves_unmanaged_launcher_and_rule_files() {
    for (relative, original) in [
        (".codex/storyhook/story.sh", "user launcher\n"),
        (".codex/rules/storyhook.rules", "user rule\n"),
    ] {
        let harness = Harness::new(true);
        harness.install_fake("codex", FAKE_CODEX);
        let path = harness.home.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, original).unwrap();

        let output = harness.run(&["plugin", "install", "codex"]);
        assert!(!output.status.success());
        assert!(combined(&output).contains("refusing to overwrite unmanaged file"));
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }
}

#[test]
fn codex_rule_verification_failure_rolls_back_managed_files() {
    let harness = Harness::new(true);
    harness.install_fake("codex", FAKE_CODEX);
    harness.set_codex_mode("execpolicy-fail");

    let output = harness.run(&["plugin", "install", "codex"]);
    assert!(!output.status.success());
    assert!(combined(&output).contains("rule verification exploded"));
    assert!(!harness.codex_launcher().exists());
    assert!(!harness.codex_rule().exists());
}

#[test]
fn failed_codex_upgrade_restores_the_previous_managed_files() {
    let harness = Harness::new(true);
    harness.install_fake("codex", FAKE_CODEX);
    let installed = harness.run(&["plugin", "install", "codex"]);
    assert!(installed.status.success(), "{}", combined(&installed));
    let launcher = fs::read(harness.codex_launcher()).unwrap();
    let rule = fs::read(harness.codex_rule()).unwrap();

    harness.set_codex_mode("execpolicy-fail");
    let failed = harness.run(&["plugin", "install", "codex"]);
    assert!(!failed.status.success());
    assert_eq!(fs::read(harness.codex_launcher()).unwrap(), launcher);
    assert_eq!(fs::read(harness.codex_rule()).unwrap(), rule);
    // The registration is part of the same transaction as the files: it was
    // at this release before, and it is again, unverified.
    let message = combined(&failed);
    assert!(
        message.contains("re-registered the same release source"),
        "{message}"
    );
    assert_eq!(
        harness.registered_source("codex").as_deref(),
        Some(harness.release_marketplace().display().to_string().as_str())
    );
}

#[test]
fn stable_codex_bridge_runs_the_current_enabled_plugin_helper_verbatim() {
    let harness = Harness::new(true);
    harness.install_fake("codex", FAKE_CODEX);
    harness.install_fake_plugin_helper(
        "0.6.0",
        "#!/bin/sh\nprintf '{\"ok\":true,\"args\":\"%s\",\"agent\":\"%s\"}\\n' \"$*\" \"${STORY_AGENT:-unset}\"\nexit 7\n",
    );

    let output = harness.run(&["plugin", "run", "codex", "--", "dispatch", "SH-9", "--auto"]);
    assert_eq!(output.status.code(), Some(7));
    // The launcher is Codex's own, so the helper runs as Codex without the
    // adapter having to say so: an environment prefix is neither a form the
    // installed-artifact guard admits nor one Codex's argv-prefix rule
    // matches (SH-632).
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "{\"ok\":true,\"args\":\"dispatch SH-9 --auto\",\"agent\":\"codex\"}\n"
    );
    assert!(String::from_utf8_lossy(&output.stderr).is_empty());
    assert!(
        harness.codex_log().contains("plugin list --json"),
        "{}",
        harness.codex_log()
    );
}

#[test]
fn stable_codex_bridge_keeps_a_callers_explicit_agent() {
    let harness = Harness::new(true);
    harness.install_fake("codex", FAKE_CODEX);
    harness.install_fake_plugin_helper(
        "0.6.0",
        "#!/bin/sh\nprintf '%s\\n' \"${STORY_AGENT:-unset}\"\n",
    );
    let mut command = Command::new(&harness.story);
    command
        .args(["plugin", "run", "codex", "--", "context"])
        .env("STORY_AGENT", "claude");
    let output = harness.run_with(command);
    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "claude\n");
}

#[test]
fn stable_launcher_follows_codex_plugin_version_changes_without_rule_edits() {
    let mut harness = Harness::new(true);
    harness.install_fake("codex", FAKE_CODEX);
    harness.install_story_on_path();
    let installed = harness.run(&["plugin", "install", "codex"]);
    assert!(installed.status.success(), "{}", combined(&installed));
    let original_rule = fs::read_to_string(harness.codex_rule()).unwrap();

    harness.install_fake_plugin_helper("0.6.0", "#!/bin/sh\necho old\n");
    let old = harness.run_launcher(&["context"]);
    assert!(old.status.success(), "{}", combined(&old));
    assert_eq!(String::from_utf8_lossy(&old.stdout), "old\n");

    harness.install_fake_plugin_helper("0.7.0", "#!/bin/sh\necho new\n");
    let new = harness.run_launcher(&["context"]);
    assert!(new.status.success(), "{}", combined(&new));
    assert_eq!(String::from_utf8_lossy(&new.stdout), "new\n");
    assert_eq!(
        fs::read_to_string(harness.codex_rule()).unwrap(),
        original_rule,
        "the stable rule is independent of the versioned cache path"
    );
}

#[test]
fn stable_codex_bridge_refuses_other_providers_and_missing_plugins() {
    let harness = Harness::new(true);
    harness.install_fake("codex", FAKE_CODEX);

    let missing = harness.run(&["plugin", "run", "codex", "context"]);
    assert!(!missing.status.success());
    assert!(combined(&missing).contains("could not locate the enabled"));

    let claude = harness.run(&["plugin", "run", "claude", "context"]);
    assert!(!claude.status.success());
    assert!(combined(&claude).contains("supports only the Codex stable launcher"));
}

#[test]
fn packaged_binary_materializes_and_registers_its_exact_embedded_marketplace() {
    let packaged = Harness::new(true);
    packaged.install_fake("codex", FAKE_CODEX);
    let output = packaged.run(&["plugin", "install", "codex"]);
    assert!(output.status.success(), "{}", combined(&output));
    let release = packaged.release_marketplace();
    assert!(
        packaged.codex_log().contains(&format!(
            "plugin marketplace add {} --json",
            release.display()
        )),
        "{}",
        packaged.codex_log()
    );
    assert_eq!(
        regular_files(&release),
        expected_marketplace(),
        "the installed release projection must be the complete build-time marketplace"
    );
    let managed = fs::read_to_string(packaged.home.join("data/storyhook/managed-paths"))
        .expect("the installer records its managed paths");
    assert!(
        managed.contains(&release.parent().unwrap().display().to_string()),
        "{managed}"
    );
}

#[test]
fn reinstall_repairs_a_corrupt_same_version_projection() {
    let harness = Harness::new(true);
    harness.install_fake("codex", FAKE_CODEX);
    let first = harness.run(&["plugin", "install", "codex"]);
    assert!(first.status.success(), "{}", combined(&first));

    let damaged = harness
        .release_marketplace()
        .join("plugins/story/bin/story.sh");
    fs::write(&damaged, "corrupt\n").unwrap();
    fs::set_permissions(&damaged, fs::Permissions::from_mode(0o644)).unwrap();

    let second = harness.run(&["plugin", "install", "codex"]);
    assert!(second.status.success(), "{}", combined(&second));
    assert_eq!(
        regular_files(&harness.release_marketplace()),
        expected_marketplace()
    );
}

#[test]
fn codex_replaces_a_stale_git_snapshot_with_the_current_release() {
    let harness = Harness::new(true);
    harness.install_fake("codex", FAKE_CODEX);
    harness.seed_codex_install("2.2.1-beta.3", "mikeydotio/storyhook");
    let stale = harness.install_fake_plugin_helper("2.2.1-beta.3", "stale catalog\n");
    fs::remove_file(harness.home.join("codex-plugin-version")).unwrap();

    let output = harness.run(&["plugin", "install", "codex"]);
    assert!(output.status.success(), "{}", combined(&output));

    let log = harness.codex_log();
    let remove_plugin = log.find("plugin remove story@storyhook --json").unwrap();
    let remove_marketplace = log
        .find("plugin marketplace remove storyhook --json")
        .unwrap();
    let add_marketplace = log.find("plugin marketplace add ").unwrap();
    let add_plugin = log.find("plugin add story@storyhook --json").unwrap();
    assert!(remove_plugin < remove_marketplace);
    assert!(remove_marketplace < add_marketplace);
    assert!(add_marketplace < add_plugin);

    let current_version = env!("CARGO_PKG_VERSION");
    assert_eq!(harness.codex_installed_version().trim(), current_version);
    assert_eq!(
        Path::new(harness.codex_marketplace_source().trim()),
        harness.release_marketplace()
    );
    let message = combined(&output);
    assert!(
        message.contains(&format!(
            "/.codex/plugins/cache/storyhook/story/{current_version}"
        )),
        "{message}"
    );
    assert!(!message.contains("2.2.1-beta.3"), "{message}");
    let current = harness
        .home
        .join(".codex/plugins/cache/storyhook/story")
        .join(current_version);
    assert_eq!(
        regular_files(&current),
        regular_files(&harness.release_marketplace().join("plugins/story"))
    );
    assert!(
        fs::read_to_string(current.join("bin/story.sh"))
            .unwrap()
            .contains("gpt-6-astra")
    );
    assert_eq!(
        fs::read_to_string(stale.join("bin/story.sh")).unwrap(),
        "stale catalog\n"
    );
}

#[test]
fn codex_install_refuses_unverified_enabled_payloads_before_writing_launcher() {
    for mode in [
        "payload-missing",
        "payload-stale",
        "payload-mode",
        "payload-extra",
        "payload-link",
        "divergent-path",
        "disabled",
        "list-fail",
    ] {
        let harness = Harness::new(true);
        harness.install_fake("codex", FAKE_CODEX);
        harness.set_codex_mode(mode);
        let output = harness.run(&["plugin", "install", "codex"]);
        let message = combined(&output);
        assert!(!output.status.success(), "{mode}: {message}");
        assert!(
            message.contains("verify") && message.contains("plugin"),
            "{mode}: {message}"
        );
        assert!(!harness.codex_launcher().exists(), "{mode}");
        assert!(!harness.codex_rule().exists(), "{mode}");
        assert!(!harness.codex_log().contains("execpolicy check"), "{mode}");
    }
}

#[test]
fn codex_install_refuses_when_provider_keeps_an_older_version_enabled() {
    let harness = Harness::new(true);
    harness.install_fake("codex", FAKE_CODEX);
    let old = harness.install_fake_plugin_helper("2.2.1-beta.3", "old helper\n");
    let output = harness.run(&["plugin", "install", "codex"]);
    let message = combined(&output);
    assert!(!output.status.success(), "{message}");
    assert!(
        message.contains("verify") && message.contains("2.2.1-beta.3"),
        "{message}"
    );
    assert_eq!(
        fs::read_to_string(old.join("bin/story.sh")).unwrap(),
        "old helper\n"
    );
    assert!(!harness.codex_launcher().exists());
}

#[test]
fn codex_uninstall_is_scoped_idempotent_and_preserves_project_files() {
    let harness = Harness::new(true);
    harness.install_fake("codex", FAKE_CODEX);
    let installed = harness.run(&["plugin", "install", "codex"]);
    assert!(installed.status.success(), "{}", combined(&installed));
    assert!(harness.codex_launcher().exists());
    assert!(harness.codex_rule().exists());
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
    assert!(!harness.codex_launcher().exists());
    assert!(!harness.codex_rule().exists());
}

#[test]
fn codex_uninstall_preserves_unmanaged_integration_files() {
    let harness = Harness::new(true);
    harness.install_fake("codex", FAKE_CODEX);
    fs::create_dir_all(harness.codex_launcher().parent().unwrap()).unwrap();
    fs::create_dir_all(harness.codex_rule().parent().unwrap()).unwrap();
    fs::write(harness.codex_launcher(), "user launcher\n").unwrap();
    fs::write(harness.codex_rule(), "user rule\n").unwrap();

    let output = harness.run(&["plugin", "uninstall", "codex"]);
    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(
        fs::read_to_string(harness.codex_launcher()).unwrap(),
        "user launcher\n"
    );
    assert_eq!(
        fs::read_to_string(harness.codex_rule()).unwrap(),
        "user rule\n"
    );
    let message = combined(&output);
    assert!(
        message.contains("preserved unmanaged launcher"),
        "{message}"
    );
    assert!(message.contains("preserved unmanaged rules"), "{message}");
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
    assert!(log.contains("plugin uninstall story@storyhook"), "{log}");
    assert!(log.contains("plugin marketplace remove storyhook"), "{log}");
    assert!(
        log.contains(&format!(
            "plugin marketplace add {} --scope user",
            harness.release_marketplace().display()
        )),
        "{log}"
    );
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

/// The receipt `story doctor install` reads when a provider has swept every
/// other trace of an install (SH-671): written only once the provider's own
/// registration succeeded, and removed by `story plugin uninstall`.
fn install_receipt(harness: &Harness, target: &str) -> PathBuf {
    harness
        .home
        .join("data/storyhook/provider-installs")
        .join(target)
}

#[test]
fn a_successful_install_writes_a_receipt_and_uninstall_removes_it() {
    for provider in ["claude", "codex"] {
        let harness = Harness::new(false);
        match provider {
            "claude" => {
                harness.install_fake("claude", FAKE_CLAUDE);
                fs::create_dir_all(harness.home.join(".claude")).unwrap();
            }
            _ => harness.install_fake("codex", FAKE_CODEX),
        }
        let receipt = install_receipt(&harness, provider);
        assert!(
            !receipt.exists(),
            "{provider}: no receipt before any install"
        );

        let output = harness.run(&["plugin", "install", provider]);
        assert!(output.status.success(), "{}", combined(&output));
        let body = fs::read_to_string(&receipt)
            .unwrap_or_else(|e| panic!("{provider}: receipt at {}: {e}", receipt.display()));
        assert!(
            body.contains(&format!("version {}", env!("CARGO_PKG_VERSION"))),
            "{provider}: the receipt names the installing release:\n{body}"
        );
        assert!(body.contains("installed_at "), "{body}");

        let output = harness.run(&["plugin", "uninstall", provider]);
        assert!(output.status.success(), "{}", combined(&output));
        assert!(
            !receipt.exists(),
            "{provider}: a deliberate uninstall leaves nothing for the doctor to read as a loss"
        );
        assert!(
            combined(&output).contains("receipt"),
            "{provider}: the uninstall names what it removed:\n{}",
            combined(&output)
        );
    }
}

#[test]
fn a_failed_install_writes_no_receipt_and_keeps_an_earlier_one() {
    let harness = Harness::new(false);
    harness.install_fake("claude", FAKE_CLAUDE);
    fs::create_dir_all(harness.home.join(".claude")).unwrap();
    let receipt = install_receipt(&harness, "claude");

    harness.set_claude_mode("plugin-install-fail");
    let output = harness.run(&["plugin", "install", "claude"]);
    assert!(!output.status.success(), "{}", combined(&output));
    assert!(
        !receipt.exists(),
        "a provider registration that never landed must not be receipted"
    );

    // Installed once for real, then a later reinstall fails after the
    // removes (SH-641 puts the previous registration back): the machine
    // WAS installed here, and the receipt must keep saying so.
    harness.set_claude_mode("");
    let output = harness.run(&["plugin", "install", "claude"]);
    assert!(output.status.success(), "{}", combined(&output));
    let first = fs::read_to_string(&receipt).unwrap();
    harness.set_claude_mode("plugin-install-fail");
    let output = harness.run(&["plugin", "install", "claude"]);
    assert!(!output.status.success(), "{}", combined(&output));
    assert_eq!(
        fs::read_to_string(&receipt).unwrap(),
        first,
        "a failed reinstall neither removes nor rewrites the receipt"
    );
}

#[test]
fn claude_install_recovers_when_the_plugin_is_absent() {
    let harness = Harness::new(false);
    harness.install_fake("claude", FAKE_CLAUDE);
    harness.set_claude_mode("plugin-absent");
    fs::create_dir_all(harness.home.join(".claude")).unwrap();

    let output = harness.run(&["plugin", "install", "claude"]);

    assert!(output.status.success(), "{}", combined(&output));
    let log = harness.claude_log();
    assert!(log.contains("plugin marketplace remove storyhook"), "{log}");
    assert!(log.contains("plugin marketplace add"), "{log}");
    assert!(
        log.contains("plugin install story@storyhook --scope user"),
        "{log}"
    );
}

#[test]
fn unrelated_claude_uninstall_failures_stop_before_marketplace_replacement() {
    let harness = Harness::new(false);
    harness.install_fake("claude", FAKE_CLAUDE);
    harness.set_claude_mode("uninstall-fail");
    fs::create_dir_all(harness.home.join(".claude")).unwrap();

    let output = harness.run(&["plugin", "install", "claude"]);

    assert!(!output.status.success());
    assert!(combined(&output).contains("unrelated uninstall failure"));
    let log = harness.claude_log();
    assert!(!log.contains("plugin marketplace"), "{log}");
    assert!(!log.contains("plugin install"), "{log}");
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

/// A Claude Code uninstall leaves the plugin cache behind — the provider's own
/// `plugin uninstall` does not clear it — and `story doctor install` reads
/// that cache as a lost registration (SH-640). A deliberate uninstall must
/// therefore sweep every directory the install owns, or every uninstalled
/// machine reads DEREGISTERED for ever and the flag stops meaning anything.
#[test]
fn claude_uninstall_sweeps_the_installed_copies_the_doctor_reads_as_residue() {
    let harness = Harness::new(false);
    harness.install_fake("claude", FAKE_CLAUDE);
    let plugins = harness.home.join(".claude/plugins");
    let cache = plugins.join("cache/storyhook/story/2.4.2");
    let marketplace = plugins.join("marketplaces/storyhook");
    let legacy = plugins.join("storyhook");
    for dir in [&cache, &marketplace, &legacy] {
        fs::create_dir_all(dir).unwrap();
    }

    let before = harness.run(&["doctor", "install"]);
    assert!(
        combined(&before).contains("DEREGISTERED"),
        "positive control: the seeded copies with no registration are a finding:\n{}",
        combined(&before)
    );

    let output = harness.run(&["plugin", "uninstall", "claude"]);
    assert!(output.status.success(), "{}", combined(&output));
    let text = combined(&output);
    for dir in [plugins.join("cache/storyhook"), marketplace, legacy] {
        assert!(!dir.exists(), "`{}` must be swept:\n{text}", dir.display());
        assert!(
            text.contains(&dir.display().to_string()),
            "the sweep must name `{}`:\n{text}",
            dir.display()
        );
    }

    let after = harness.run(&["doctor", "install"]);
    assert!(
        !combined(&after).contains("DEREGISTERED"),
        "a deliberate uninstall leaves the doctor quiet:\n{}",
        combined(&after)
    );
}

/// The Codex twin: the fake `codex plugin add` writes the versioned cache the
/// real one does, and neither's `plugin remove` clears it.
#[test]
fn codex_uninstall_sweeps_the_plugin_cache_the_doctor_reads_as_residue() {
    let harness = Harness::new(true);
    harness.install_fake("codex", FAKE_CODEX);
    let installed = harness.run(&["plugin", "install", "codex"]);
    assert!(installed.status.success(), "{}", combined(&installed));
    let cache = harness.home.join(".codex/plugins/cache/storyhook");
    assert!(
        cache.is_dir(),
        "positive control: the install populated the cache"
    );

    let output = harness.run(&["plugin", "uninstall", "codex"]);
    assert!(output.status.success(), "{}", combined(&output));
    assert!(
        !cache.exists(),
        "the cache must be swept:\n{}",
        combined(&output)
    );
    assert!(
        combined(&output).contains(&cache.display().to_string()),
        "the sweep must be named:\n{}",
        combined(&output)
    );
    assert!(!harness.codex_launcher().exists());
    assert!(!harness.codex_rule().exists());

    let after = harness.run(&["doctor", "install"]);
    assert!(
        !combined(&after).contains("DEREGISTERED"),
        "a deliberate uninstall leaves the doctor quiet:\n{}",
        combined(&after)
    );
}

// --- `story plugin reinstall` (SH-667) ---------------------------------------
//
// The verb every binary-replacement path runs: reinstall the plugin for every
// provider whose own configuration registers the storyhook marketplace, from
// the binary that is now installed, and touch nothing that is not registered.

impl Harness {
    /// Both fake providers on PATH, ready to install, from a packaged copy of
    /// the binary (the Codex fixtures' shape).
    fn for_both_providers() -> Self {
        let harness = Harness::new(true);
        harness.install_fake("claude", FAKE_CLAUDE);
        harness.install_fake("codex", FAKE_CODEX);
        fs::create_dir_all(harness.home.join(".claude")).unwrap();
        harness
    }

    /// A Codex registration at an OLDER release projection under this
    /// fixture's own data directory — the exact state `story doctor install`
    /// reads as `STALE RELEASE`, and the state every `make install` used to
    /// leave behind. A real directory carrying the payload, because the fake
    /// `codex plugin add` copies out of whatever source it was handed.
    fn seed_stale_release_registration(&self) -> PathBuf {
        let stale = self.home.join("data/storyhook/plugins/0.0.1");
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
        for relative in [
            ".agents/plugins/marketplace.json",
            ".claude-plugin/marketplace.json",
        ] {
            let target = stale.join(relative);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::copy(repository.join(relative), target).unwrap();
        }
        copy_tree(
            &repository.join("plugins/story"),
            &stale.join("plugins/story"),
        );
        let source = stale.display().to_string();
        self.seed_codex_install("0.0.1", &source);
        fs::create_dir_all(self.home.join(".codex")).unwrap();
        fs::write(
            self.home.join(".codex/config.toml"),
            format!("[marketplaces.storyhook]\nsource_type = \"local\"\nsource = \"{source}\"\n"),
        )
        .unwrap();
        stale
    }

    /// The `codex plugin` row of `story doctor install`, as one line.
    fn doctor_codex_row(&self) -> String {
        let output = self.run(&["doctor", "install"]);
        let text = combined(&output);
        text.lines()
            .find(|line| line.starts_with("codex plugin"))
            .unwrap_or_else(|| panic!("no `codex plugin` row in:\n{text}"))
            .to_string()
    }
}

#[test]
fn reinstall_refreshes_every_registered_provider_from_this_binary() {
    let harness = Harness::for_both_providers();
    for provider in ["claude", "codex"] {
        harness.seed_previous_registration(provider);
        assert_ne!(
            harness.registered_source(provider).as_deref(),
            Some(harness.release_marketplace().display().to_string().as_str()),
            "{provider}: the seed must register something other than this release"
        );
    }

    let output = harness.run(&["plugin", "reinstall"]);
    let text = combined(&output);
    assert!(output.status.success(), "{text}");
    assert!(
        text.contains("reinstalled the Claude Code plugin"),
        "{text}"
    );
    assert!(text.contains("reinstalled the Codex plugin"), "{text}");
    assert!(!text.contains("warning:"), "{text}");

    let release = harness.release_marketplace();
    for provider in ["claude", "codex"] {
        assert_eq!(
            harness.registered_source(provider).as_deref(),
            Some(release.display().to_string().as_str()),
            "{provider}: must now be registered at this binary's release"
        );
        let log = harness.provider_log(provider);
        for step in [
            "remove-plugin",
            "remove-marketplace",
            "add-marketplace",
            "add-plugin",
        ] {
            let line = verb(provider, step, &release);
            assert_eq!(
                log.lines().filter(|l| *l == line).count(),
                1,
                "{provider}: `{line}` must run exactly once:\n{log}"
            );
        }
    }
}

#[test]
fn reinstall_with_nothing_registered_succeeds_and_invokes_no_provider() {
    let harness = Harness::for_both_providers();

    let output = harness.run(&["plugin", "reinstall"]);
    let text = combined(&output);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("nothing to reinstall"), "{text}");
    assert!(!text.contains("warning:"), "{text}");
    assert_eq!(harness.claude_log(), "", "Claude must not be invoked");
    assert_eq!(harness.codex_log(), "", "Codex must not be invoked");
}

#[test]
fn reinstall_leaves_an_unregistered_provider_alone_even_when_its_cli_is_present() {
    let harness = Harness::for_both_providers();
    harness.seed_previous_registration("codex");

    let output = harness.run(&["plugin", "reinstall"]);
    let text = combined(&output);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("reinstalled the Codex plugin"), "{text}");
    assert!(!text.contains("Claude Code"), "{text}");
    assert_eq!(
        harness.claude_log(),
        "",
        "a provider with no registration must not be touched"
    );
    assert_eq!(
        harness.registered_source("codex").as_deref(),
        Some(harness.release_marketplace().display().to_string().as_str())
    );
}

/// The DEREGISTERED state (SH-640): installed copies remain but the provider
/// no longer lists the marketplace. Not intent, so not reinstalled — but never
/// silent, and the remedy is the one `story doctor install` names.
#[test]
fn reinstall_warns_about_residue_without_a_registration_and_does_not_install_it() {
    let harness = Harness::for_both_providers();
    let residue = harness.home.join(".claude/plugins/cache/storyhook");
    fs::create_dir_all(&residue).unwrap();

    let output = harness.run(&["plugin", "reinstall"]);
    let text = combined(&output);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("nothing to reinstall"), "{text}");
    assert!(text.contains("warning: Claude Code:"), "{text}");
    assert!(text.contains(&residue.display().to_string()), "{text}");
    assert!(text.contains("`story plugin install claude`"), "{text}");
    assert_eq!(harness.claude_log(), "", "residue must not be reinstalled");
    assert!(
        residue.is_dir(),
        "residue is reported, never swept, by a reinstall"
    );
}

/// One provider's failure never costs the other its refresh, and the exit
/// status says the reinstall as a whole did not finish.
#[test]
fn reinstall_finishes_the_other_provider_when_one_fails_and_exits_non_zero() {
    let harness = Harness::for_both_providers();
    harness.seed_previous_registration("claude");
    harness.seed_previous_registration("codex");
    harness.set_codex_mode("plugin-fail");

    let output = harness.run(&["plugin", "reinstall"]);
    let text = combined(&output);
    assert!(!output.status.success(), "{text}");
    assert!(
        text.contains("reinstalled the Claude Code plugin"),
        "{text}"
    );
    assert!(
        text.contains("failed to reinstall the Codex plugin"),
        "{text}"
    );
    assert!(text.contains("plugin exploded"), "{text}");
    assert!(
        text.contains("1 of 2 provider plugin(s) could not be reinstalled"),
        "{text}"
    );
    assert!(
        text.contains("run `story plugin reinstall` to retry"),
        "{text}"
    );
    assert_eq!(
        harness.registered_source("claude").as_deref(),
        Some(harness.release_marketplace().display().to_string().as_str()),
        "Claude must be refreshed even though Codex failed"
    );
    let codex = harness.codex_log();
    assert!(
        codex.contains("plugin add story@storyhook --json"),
        "Codex must have been attempted:\n{codex}"
    );
}

/// End to end against the detector: the state `make install` used to leave is
/// what `story doctor install` calls STALE RELEASE, and one reinstall clears it.
#[test]
fn reinstall_clears_the_stale_release_the_doctor_reports() {
    let harness = Harness::for_both_providers();
    let stale = harness.seed_stale_release_registration();

    let before = harness.doctor_codex_row();
    assert!(before.contains(&stale.display().to_string()), "{before}");
    let output = harness.run(&["doctor", "install"]);
    assert!(
        combined(&output).contains("STALE RELEASE"),
        "{}",
        combined(&output)
    );

    let output = harness.run(&["plugin", "reinstall"]);
    let text = combined(&output);
    assert!(output.status.success(), "{text}");

    let after = harness.doctor_codex_row();
    assert!(
        after.contains(&format!("release {}", env!("CARGO_PKG_VERSION"))),
        "{after}"
    );
    let output = harness.run(&["doctor", "install"]);
    assert!(
        !combined(&output).contains("STALE RELEASE"),
        "{}",
        combined(&output)
    );
}

#[test]
fn stable_codex_bridge_does_not_inject_a_provider_for_deterministic_commands() {
    let harness = Harness::new(true);
    harness.install_fake("codex", FAKE_CODEX);
    harness.install_fake_plugin_helper(
        "0.6.0",
        "#!/bin/sh\nprintf '%s\\n' \"${STORY_AGENT:-unset}\"\n",
    );
    for verb in [
        "reset", "unclaim", "capture", "complete", "reap", "context", "notify",
    ] {
        let output = harness.run(&[
            "plugin",
            "run",
            "codex",
            "--",
            "--project",
            "fixture",
            verb,
            "SH-9",
        ]);
        assert!(output.status.success(), "{}", combined(&output));
        assert_eq!(String::from_utf8_lossy(&output.stdout), "unset\n", "{verb}");
    }
}
