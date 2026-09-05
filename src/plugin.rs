use std::collections::BTreeSet;
use std::fs;
use std::fs::OpenOptions;
use std::io::ErrorKind;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output};

use fs4::FileExt;

use crate::env::spawn_env::apply_plugin_cli_allowlist;
use crate::error::AppError;

const MARKETPLACE_NAME: &str = "storyhook";
const PLUGIN_REF: &str = "story@storyhook";
const LEGACY_PLUGIN_DIR_NAME: &str = "storyhook";
const SENTINEL_BEGIN: &str = "<!-- storyhook:begin -->";
const SENTINEL_END: &str = "<!-- storyhook:end -->";
const INSTRUCTIONS_SENTINEL_BEGIN: &str = "<!-- BEGIN STORYHOOK -->";
const INSTRUCTIONS_SENTINEL_END: &str = "<!-- END STORYHOOK -->";
const CODEX_LAUNCHER_MARKER: &str = "# storyhook-managed: codex-launcher-v1";
const CODEX_RULE_MARKER: &str = "# storyhook-managed: codex-rules-v1";
const CODEX_LAUNCHER_RELATIVE: &str = ".codex/storyhook/story.sh";
const CODEX_RULE_RELATIVE: &str = ".codex/rules/storyhook.rules";

struct EmbeddedFile {
    relative_path: &'static str,
    bytes: &'static [u8],
    executable: bool,
}

include!(concat!(env!("OUT_DIR"), "/embedded_marketplace.rs"));

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PluginTarget {
    ClaudeCode,
    Codex,
}

impl PluginTarget {
    fn parse(raw: &str) -> Result<Self, AppError> {
        match raw {
            "claude" | "claude-code" => Ok(Self::ClaudeCode),
            "codex" => Ok(Self::Codex),
            _ => Err(AppError::Usage(format!(
                "unknown plugin target: {raw}. Supported: claude, codex"
            ))),
        }
    }

    const fn executable(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
        }
    }
}

fn compatibility_alias_warning(raw: &str) -> Option<&'static str> {
    (raw == "claude-code")
        .then_some("warning: plugin target `claude-code` is deprecated; use `claude` instead.\n")
}

fn home_dir() -> Result<PathBuf, AppError> {
    let home = std::env::var("HOME")
        .map_err(|_| AppError::Storage("could not determine home directory".to_string()))?;
    Ok(PathBuf::from(home))
}

/// Resolve the Claude Code config directory (~/.claude/).
fn claude_dir() -> Result<PathBuf, AppError> {
    Ok(home_dir()?.join(".claude"))
}

fn claude_plugins_dir() -> Result<PathBuf, AppError> {
    Ok(claude_dir()?.join("plugins"))
}

/// Detect a StoryHook development checkout for the explicit dispatch fallback.
///
/// Plugin installation never uses this path: its marketplace comes from the
/// binary's embedded release payload. Checkout dispatch remains useful for a
/// developer running an uninstalled build against a protocol-compatible local
/// helper.
pub(crate) fn dev_repo_root() -> Option<PathBuf> {
    let has_marketplace = |dir: &Path| {
        dir.join(".claude-plugin/marketplace.json").is_file()
            || dir.join(".agents/plugins/marketplace.json").is_file()
    };

    if let Ok(cwd) = std::env::current_dir()
        && has_marketplace(&cwd)
    {
        return Some(cwd);
    }

    if let Ok(exe) = std::env::current_exe() {
        let mut cursor = exe.parent();
        for _ in 0..3 {
            if let Some(dir) = cursor {
                if has_marketplace(dir) {
                    return Some(dir.to_path_buf());
                }
                cursor = dir.parent();
            }
        }
    }

    None
}

fn data_dir() -> Result<PathBuf, AppError> {
    if let Ok(dir) = std::env::var("STORYHOOK_DATA_DIR")
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir));
    }
    if let Ok(dir) = std::env::var("XDG_DATA_HOME")
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir).join("storyhook"));
    }
    Ok(home_dir()?.join(".local/share/storyhook"))
}

/// Stable parent containing every binary-versioned plugin marketplace.
pub(crate) fn release_marketplaces_root() -> Result<PathBuf, AppError> {
    Ok(data_dir()?.join("plugins"))
}

/// Marketplace projection carried by this exact binary version.
pub(crate) fn release_marketplace_root() -> Result<PathBuf, AppError> {
    Ok(release_marketplaces_root()?.join(env!("CARGO_PKG_VERSION")))
}

fn missing_message(target: PluginTarget) -> String {
    match target {
        PluginTarget::ClaudeCode => "Claude Code CLI (`claude`) not found on PATH. Install or \
             enable it, then retry `story plugin install claude`."
            .to_string(),
        PluginTarget::Codex => "Codex CLI (`codex`) not found on PATH. Install or enable Codex, \
             then retry `story plugin install codex`."
            .to_string(),
    }
}

fn preflight_provider(target: PluginTarget) -> Result<(), AppError> {
    if target == PluginTarget::ClaudeCode && !claude_dir()?.exists() {
        return Err(AppError::Storage(
            "Claude Code not detected (~/.claude/ does not exist). Install Claude Code first, \
             then retry `story plugin install claude`."
                .to_string(),
        ));
    }
    if provider_available(target) {
        Ok(())
    } else {
        Err(AppError::Storage(missing_message(target)))
    }
}

fn provider_available(target: PluginTarget) -> bool {
    let mut command = Command::new(target.executable());
    apply_plugin_cli_allowlist(&mut command);
    match command
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        // Preserve Claude's historical "invokable is enough" detection.
        Ok(status) => target == PluginTarget::ClaudeCode || status.success(),
        Err(_) => false,
    }
}

fn run_provider(target: PluginTarget, args: &[&str]) -> Result<Output, AppError> {
    let mut command = Command::new(target.executable());
    apply_plugin_cli_allowlist(&mut command);
    command.args(args).output().map_err(|error| {
        if error.kind() == ErrorKind::NotFound {
            AppError::Storage(missing_message(target))
        } else {
            AppError::Storage(format!(
                "failed to run `{} {}`: {error}",
                target.executable(),
                args.join(" ")
            ))
        }
    })
}

fn combined_output(out: &Output) -> String {
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    text
}

fn embedded_file_set(root: &Path) -> Option<BTreeSet<PathBuf>> {
    fn visit(root: &Path, directory: &Path, found: &mut BTreeSet<PathBuf>) -> Option<()> {
        let mut entries: Vec<_> = fs::read_dir(directory)
            .ok()?
            .collect::<Result<_, _>>()
            .ok()?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).ok()?;
            if metadata.is_dir() {
                visit(root, &path, found)?;
            } else if metadata.is_file() {
                found.insert(path.strip_prefix(root).ok()?.to_path_buf());
            } else {
                return None;
            }
        }
        Some(())
    }

    let mut found = BTreeSet::new();
    visit(root, root, &mut found)?;
    Some(found)
}

fn release_marketplace_matches(root: &Path) -> bool {
    let Some(found) = embedded_file_set(root) else {
        return false;
    };
    let expected: BTreeSet<PathBuf> = EMBEDDED_MARKETPLACE
        .iter()
        .map(|file| PathBuf::from(file.relative_path))
        .collect();
    if found != expected {
        return false;
    }
    EMBEDDED_MARKETPLACE.iter().all(|file| {
        let path = root.join(file.relative_path);
        let Ok(metadata) = fs::metadata(&path) else {
            return false;
        };
        let executable = {
            #[cfg(unix)]
            {
                metadata.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                false
            }
        };
        executable == file.executable && fs::read(path).is_ok_and(|bytes| bytes == file.bytes)
    })
}

fn write_release_marketplace(root: &Path) -> Result<(), AppError> {
    for file in EMBEDDED_MARKETPLACE {
        let path = root.join(file.relative_path);
        let parent = path.parent().ok_or_else(|| {
            AppError::Storage(format!(
                "embedded marketplace path `{}` has no parent",
                file.relative_path
            ))
        })?;
        fs::create_dir_all(parent)?;
        fs::write(&path, file.bytes)?;
        #[cfg(unix)]
        {
            let mode = if file.executable { 0o755 } else { 0o644 };
            fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
        }
    }
    Ok(())
}

fn materialize_release_marketplace() -> Result<PathBuf, AppError> {
    let releases = release_marketplaces_root()?;
    fs::create_dir_all(&releases)?;
    let lock_path = releases.join(".install.lock");
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| {
            AppError::Storage(format!(
                "failed to open plugin installation lock `{}`: {error}",
                lock_path.display()
            ))
        })?;
    FileExt::lock_exclusive(&lock).map_err(|error| {
        AppError::Storage(format!(
            "failed to lock plugin installation at `{}`: {error}",
            lock_path.display()
        ))
    })?;

    let destination = release_marketplace_root()?;
    if release_marketplace_matches(&destination) {
        return Ok(destination);
    }

    let staged = tempfile::Builder::new()
        .prefix(".plugin-staging-")
        .tempdir_in(&releases)?;
    write_release_marketplace(staged.path())?;
    if !release_marketplace_matches(staged.path()) {
        return Err(AppError::Storage(
            "the staged plugin marketplace did not match its embedded payload".to_string(),
        ));
    }

    if destination.exists() || fs::symlink_metadata(&destination).is_ok() {
        let backup = tempfile::Builder::new()
            .prefix(".plugin-previous-")
            .tempdir_in(&releases)?;
        let previous = backup.path().join("marketplace");
        fs::rename(&destination, &previous)?;
        if let Err(error) = fs::rename(staged.path(), &destination) {
            let restore = fs::rename(&previous, &destination);
            return Err(AppError::Storage(match restore {
                Ok(()) => format!(
                    "failed to publish plugin marketplace at `{}`; restored the previous copy: {error}",
                    destination.display()
                ),
                Err(restore_error) => format!(
                    "failed to publish plugin marketplace at `{}` ({error}) and failed to restore its previous copy ({restore_error})",
                    destination.display()
                ),
            }));
        }
    } else {
        fs::rename(staged.path(), &destination)?;
    }
    Ok(destination)
}

fn remove_claude_plugin() -> Result<(), AppError> {
    let out = run_provider(
        PluginTarget::ClaudeCode,
        &["plugin", "uninstall", PLUGIN_REF],
    )?;
    if out.status.success() {
        return Ok(());
    }
    let text = combined_output(&out);
    let lower = text.to_lowercase();
    if lower.contains("not installed") || lower.contains("not found in installed plugins") {
        return Ok(());
    }
    Err(AppError::Storage(format!(
        "failed to remove `{PLUGIN_REF}` before reinstalling it:\n{}",
        text.trim()
    )))
}

fn remove_claude_marketplace() -> Result<(), AppError> {
    let out = run_provider(
        PluginTarget::ClaudeCode,
        &["plugin", "marketplace", "remove", MARKETPLACE_NAME],
    )?;
    if out.status.success() {
        return Ok(());
    }
    let text = combined_output(&out);
    let lower = text.to_lowercase();
    if lower.contains("not found") || lower.contains("not configured") {
        return Ok(());
    }
    Err(AppError::Storage(format!(
        "failed to remove the storyhook marketplace before reinstalling it:\n{}",
        text.trim()
    )))
}

fn add_claude_marketplace(source: &str) -> Result<(), AppError> {
    let out = run_provider(
        PluginTarget::ClaudeCode,
        &["plugin", "marketplace", "add", source, "--scope", "user"],
    )?;
    if out.status.success() {
        return Ok(());
    }
    Err(AppError::Storage(format!(
        "failed to add storyhook marketplace from `{source}`:\n{}",
        combined_output(&out).trim()
    )))
}

fn install_claude_plugin() -> Result<(), AppError> {
    let out = run_provider(
        PluginTarget::ClaudeCode,
        &["plugin", "install", PLUGIN_REF, "--scope", "user"],
    )?;
    if out.status.success() || combined_output(&out).to_lowercase().contains("already") {
        return Ok(());
    }
    Err(AppError::Storage(format!(
        "failed to install `{PLUGIN_REF}`:\n{}",
        combined_output(&out).trim()
    )))
}

fn codex_json(out: &Output, action: &str) -> Result<serde_json::Value, AppError> {
    if !out.status.success() {
        return Err(AppError::Storage(format!(
            "failed to {action}:\n{}",
            combined_output(out).trim()
        )));
    }
    serde_json::from_slice(&out.stdout).map_err(|error| {
        AppError::Storage(format!(
            "Codex reported success while trying to {action}, but returned invalid JSON: {error}"
        ))
    })
}

/// The immutable cache root of the installed Storyhook Codex plugin.
///
/// Codex's durable config records only that `story@storyhook` is enabled; its
/// `plugin list --json` response is the authority on the installed version.
/// The response does not repeat `installedPath`, so derive the documented
/// cache layout from its exact marketplace/name/version identity and accept
/// it only when the plugin manifest exists there. This avoids choosing an
/// arbitrary stale cache directory when more than one version remains.
pub(crate) fn codex_installed_plugin_root(home: &Path) -> Option<PathBuf> {
    let out = run_provider(PluginTarget::Codex, &["plugin", "list", "--json"]).ok()?;
    if !out.status.success() {
        return None;
    }
    codex_installed_plugin_root_from(home, &out.stdout)
}

fn codex_installed_plugin_root_from(home: &Path, raw: &[u8]) -> Option<PathBuf> {
    let value: serde_json::Value = serde_json::from_slice(raw).ok()?;
    let plugin = value.get("installed")?.as_array()?.iter().find(|plugin| {
        plugin.get("pluginId").and_then(|v| v.as_str()) == Some(PLUGIN_REF)
            && plugin.get("marketplaceName").and_then(|v| v.as_str()) == Some(MARKETPLACE_NAME)
            && plugin.get("name").and_then(|v| v.as_str()) == Some("story")
            && plugin.get("installed").and_then(|v| v.as_bool()) == Some(true)
            && plugin.get("enabled").and_then(|v| v.as_bool()) != Some(false)
    })?;
    let version = plugin.get("version")?.as_str()?;
    if version.is_empty()
        || version == "."
        || version == ".."
        || version.contains('/')
        || version.contains('\\')
    {
        return None;
    }
    let root = home
        .join(".codex/plugins/cache")
        .join(MARKETPLACE_NAME)
        .join("story")
        .join(version);
    root.join(".codex-plugin/plugin.json")
        .is_file()
        .then_some(root)
}

fn codex_launcher_path(home: &Path) -> PathBuf {
    home.join(CODEX_LAUNCHER_RELATIVE)
}

fn codex_rule_path(home: &Path) -> PathBuf {
    home.join(CODEX_RULE_RELATIVE)
}

fn codex_launcher_contents() -> String {
    format!(
        "{CODEX_LAUNCHER_MARKER}\n\
         # Recreated by `story plugin install codex`; do not edit.\n\
         exec story plugin run codex -- \"$@\"\n"
    )
}

fn codex_rule_contents(launcher: &Path) -> Result<String, AppError> {
    let launcher = serde_json::to_string(&launcher.to_string_lossy()).map_err(|error| {
        AppError::Storage(format!("failed to encode the Codex launcher path: {error}"))
    })?;
    Ok(format!(
        "{CODEX_RULE_MARKER}\n\
         # Recreated by `story plugin install codex`; Codex loads it at startup.\n\
         prefix_rule(\n\
             pattern = [\"bash\", {launcher}],\n\
             decision = \"allow\",\n\
             justification = \"Allow the installed Storyhook skill to access its daemon and state store\",\n\
         )\n"
    ))
}

fn existing_managed_file(path: &Path, marker: &str) -> Result<Option<Vec<u8>>, AppError> {
    match fs::read(path) {
        Ok(contents) => {
            if contents.starts_with(marker.as_bytes()) {
                Ok(Some(contents))
            } else {
                Err(AppError::Storage(format!(
                    "refusing to overwrite unmanaged file `{}`; move it aside and retry",
                    path.display()
                )))
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(AppError::Storage(format!(
            "failed to read `{}`: {error}",
            path.display()
        ))),
    }
}

fn write_managed_file(path: &Path, contents: &str, executable: bool) -> Result<(), AppError> {
    let parent = path.parent().ok_or_else(|| {
        AppError::Storage(format!("managed path `{}` has no parent", path.display()))
    })?;
    fs::create_dir_all(parent)?;
    fs::write(path, contents)?;
    #[cfg(unix)]
    {
        let mode = if executable { 0o755 } else { 0o644 };
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(mode);
        fs::set_permissions(path, permissions)?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    Ok(())
}

fn restore_managed_file(path: &Path, previous: Option<&[u8]>, executable: bool) {
    match previous {
        Some(contents) => {
            if fs::write(path, contents).is_ok() {
                #[cfg(unix)]
                if let Ok(metadata) = fs::metadata(path) {
                    let mut permissions = metadata.permissions();
                    permissions.set_mode(if executable { 0o755 } else { 0o644 });
                    let _ = fs::set_permissions(path, permissions);
                }
            }
        }
        None => {
            let _ = fs::remove_file(path);
        }
    }
}

fn verify_codex_rule(rule: &Path, launcher: &Path) -> Result<(), AppError> {
    let rule = rule.to_string_lossy().into_owned();
    let launcher = launcher.to_string_lossy().into_owned();
    let out = run_provider(
        PluginTarget::Codex,
        &[
            "execpolicy",
            "check",
            "--pretty",
            "--rules",
            &rule,
            "--",
            "bash",
            &launcher,
            "context",
        ],
    )?;
    let value = codex_json(&out, "verify the Storyhook Codex sandbox rule")?;
    if value["decision"].as_str() == Some("allow") {
        Ok(())
    } else {
        Err(AppError::Storage(format!(
            "Codex did not allow the Storyhook launcher while verifying `{}`",
            rule
        )))
    }
}

fn install_codex_sandbox_integration() -> Result<(PathBuf, PathBuf), AppError> {
    let home = home_dir()?;
    let launcher = codex_launcher_path(&home);
    let rule = codex_rule_path(&home);
    let previous_launcher = existing_managed_file(&launcher, CODEX_LAUNCHER_MARKER)?;
    let previous_rule = existing_managed_file(&rule, CODEX_RULE_MARKER)?;
    let launcher_contents = codex_launcher_contents();
    let rule_contents = codex_rule_contents(&launcher)?;

    let result = (|| {
        write_managed_file(&launcher, &launcher_contents, true)?;
        write_managed_file(&rule, &rule_contents, false)?;
        verify_codex_rule(&rule, &launcher)
    })();
    if let Err(error) = result {
        restore_managed_file(&launcher, previous_launcher.as_deref(), true);
        restore_managed_file(&rule, previous_rule.as_deref(), false);
        return Err(error);
    }
    Ok((launcher, rule))
}

enum ManagedRemoval {
    Missing,
    Removed,
    Preserved,
}

fn remove_managed_file(path: &Path, marker: &str) -> Result<ManagedRemoval, AppError> {
    match fs::read(path) {
        Ok(contents) if contents.starts_with(marker.as_bytes()) => {
            fs::remove_file(path)?;
            Ok(ManagedRemoval::Removed)
        }
        Ok(_) => Ok(ManagedRemoval::Preserved),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(ManagedRemoval::Missing),
        Err(error) => Err(AppError::Storage(format!(
            "failed to read `{}`: {error}",
            path.display()
        ))),
    }
}

fn remove_empty_dir(path: &Path) {
    if fs::read_dir(path)
        .ok()
        .is_some_and(|mut entries| entries.next().is_none())
    {
        let _ = fs::remove_dir(path);
    }
}

/// Run the helper from the exact enabled Codex plugin version while preserving
/// the caller's streams, cwd, terminal, and exit status. This is intercepted
/// by the CLI before any daemon/store work; the stable launcher is its only
/// intended caller.
pub fn run_helper(target: &str, args: &[String]) -> Result<ExitStatus, AppError> {
    if PluginTarget::parse(target)? != PluginTarget::Codex {
        return Err(AppError::Usage(
            "`story plugin run` supports only the Codex stable launcher".to_string(),
        ));
    }
    if args.is_empty() {
        return Err(AppError::Usage(
            "usage: story plugin run codex -- <helper-command> [args...]".to_string(),
        ));
    }
    let home = home_dir()?;
    let root = codex_installed_plugin_root(&home).ok_or_else(|| {
        AppError::Storage(
            "could not locate the enabled `story@storyhook` Codex plugin; run `story plugin install codex`"
                .to_string(),
        )
    })?;
    let helper = root.join("bin/story.sh");
    if !helper.is_file() {
        return Err(AppError::Storage(format!(
            "the enabled Storyhook Codex plugin has no helper at `{}`; reinstall it with `story plugin install codex`",
            helper.display()
        )));
    }
    Command::new("bash")
        .arg(&helper)
        .args(args)
        .status()
        .map_err(|error| {
            AppError::Storage(format!(
                "failed to run the installed Storyhook helper `{}`: {error}",
                helper.display()
            ))
        })
}

fn expect_codex_field(
    value: &serde_json::Value,
    key: &str,
    expected: &str,
    action: &str,
) -> Result<(), AppError> {
    if value[key].as_str() == Some(expected) {
        Ok(())
    } else {
        Err(AppError::Storage(format!(
            "Codex returned an unexpected result while trying to {action}: expected `{key}` to be `{expected}`"
        )))
    }
}

fn add_codex_marketplace(source: &str) -> Result<(), AppError> {
    let out = run_provider(
        PluginTarget::Codex,
        &["plugin", "marketplace", "add", source, "--json"],
    )?;
    let value = codex_json(&out, "add the storyhook marketplace")?;
    expect_codex_field(
        &value,
        "marketplaceName",
        MARKETPLACE_NAME,
        "add the storyhook marketplace",
    )?;
    value["alreadyAdded"].as_bool().ok_or_else(|| {
        AppError::Storage(
            "Codex returned an unexpected marketplace result: `alreadyAdded` was not boolean"
                .to_string(),
        )
    })?;
    Ok(())
}

fn add_codex_plugin() -> Result<String, AppError> {
    let out = run_provider(
        PluginTarget::Codex,
        &["plugin", "add", PLUGIN_REF, "--json"],
    )?;
    let value = codex_json(&out, "install the Storyhook plugin")?;
    expect_codex_field(
        &value,
        "pluginId",
        PLUGIN_REF,
        "install the Storyhook plugin",
    )?;
    expect_codex_field(
        &value,
        "marketplaceName",
        MARKETPLACE_NAME,
        "install the Storyhook plugin",
    )?;
    value["installedPath"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| {
            AppError::Storage(
                "Codex returned an unexpected install result: `installedPath` was missing"
                    .to_string(),
            )
        })
}

fn remove_codex_plugin() -> Result<(), AppError> {
    let out = run_provider(
        PluginTarget::Codex,
        &["plugin", "remove", PLUGIN_REF, "--json"],
    )?;
    if !out.status.success() {
        let text = combined_output(&out);
        if text.to_lowercase().contains("not installed") {
            return Ok(());
        }
        return Err(AppError::Storage(format!(
            "failed to remove `{PLUGIN_REF}`:\n{}",
            text.trim()
        )));
    }
    let value = codex_json(&out, "remove the Storyhook plugin")?;
    expect_codex_field(
        &value,
        "pluginId",
        PLUGIN_REF,
        "remove the Storyhook plugin",
    )
}

fn remove_codex_marketplace() -> Result<(), AppError> {
    let out = run_provider(
        PluginTarget::Codex,
        &[
            "plugin",
            "marketplace",
            "remove",
            MARKETPLACE_NAME,
            "--json",
        ],
    )?;
    if !out.status.success() {
        let text = combined_output(&out);
        if text
            .to_lowercase()
            .contains("marketplace `storyhook` is not configured or installed")
        {
            return Ok(());
        }
        return Err(AppError::Storage(format!(
            "failed to remove the storyhook marketplace:\n{}",
            text.trim()
        )));
    }
    let value = codex_json(&out, "remove the storyhook marketplace")?;
    expect_codex_field(
        &value,
        "marketplaceName",
        MARKETPLACE_NAME,
        "remove the storyhook marketplace",
    )
}

fn append_settings_guidance(message: &mut String, project_root: &Path) {
    if crate::service::project::pointer_plugin(project_root).is_none() {
        message.push_str(
            "\nThe plugin runs with default settings. To change them, add this to \
             `.storyhook.toml`:\n\n    [plugin]\n    enabled = true\n    tracking = \
             \"normal\"\n",
        );
    }
}

fn install_claude(project_root: &Path, source: &str) -> Result<String, AppError> {
    remove_claude_plugin()?;
    remove_claude_marketplace()?;
    add_claude_marketplace(source)?;
    install_claude_plugin()?;

    let mut message = format!(
        "registered storyhook plugin via the `{MARKETPLACE_NAME}` marketplace (source: {source})\n"
    );
    append_settings_guidance(&mut message, project_root);
    message.push_str(
        "\nStart a new Claude Code session to load the plugin, then run \
         /story-context to get started (or run story load-context directly).",
    );
    Ok(message)
}

fn install_codex(project_root: &Path, source: &str) -> Result<String, AppError> {
    remove_codex_plugin()?;
    remove_codex_marketplace()?;
    add_codex_marketplace(source)?;
    let installed_path = add_codex_plugin()?;
    let (launcher_path, rule_path) = install_codex_sandbox_integration()?;
    let mut message = format!(
        "registered the `{MARKETPLACE_NAME}` marketplace (source: {source})\n\
         installed `{PLUGIN_REF}` at {installed_path}\n\
         installed the stable Codex launcher at {}\n\
         installed and verified its sandbox rule at {}\n",
        launcher_path.display(),
        rule_path.display()
    );
    append_settings_guidance(&mut message, project_root);
    message.push_str(
        "\nRestart Codex so it loads the sandbox rule and Storyhook skills, then select \
         the `story-context` skill or ask for the current Storyhook project context.",
    );
    Ok(message)
}

/// Where storyhook's own installed copies live: every directory a provider
/// plugin install writes into or registers (SH-530).
///
/// The single definition of "managed", consumed by the installer that creates
/// these paths and by `hooks/protect-install.sh`, which refuses edits to them.
/// One definition rather than two, because a shell-side list would drift from
/// this one the first time a provider changed a directory — the hand-kept-list
/// shape SH-136, SH-198, SH-258, SH-260/276, SH-360 and SH-364 each cost this
/// project once already.
///
/// **The `story` binary is deliberately NOT here.** Its overwrite paths are
/// `make install` and `story update`, and `make install` in particular is the
/// recovery `StoreError::SchemaTooNew`'s own message prescribes. Refusing it
/// would make the store's advice a dead end — the trap SH-404 documented and
/// SH-405 was filed for. This list is about release artifacts a *plugin
/// install* overwrites, which is a narrower and better-defined thing.
pub fn managed_paths() -> Result<Vec<PathBuf>, AppError> {
    let home = home_dir()?;
    let claude = claude_plugins_dir()?;
    Ok(vec![
        claude.join("cache").join(MARKETPLACE_NAME),
        claude.join("marketplaces").join(MARKETPLACE_NAME),
        claude.join(LEGACY_PLUGIN_DIR_NAME),
        release_marketplaces_root()?,
        home.join(".codex/plugins/cache").join(MARKETPLACE_NAME),
        home.join(CODEX_LAUNCHER_RELATIVE)
            .parent()
            .expect("the launcher has a parent directory")
            .to_path_buf(),
        home.join(CODEX_RULE_RELATIVE),
    ])
}

/// The file `hooks/protect-install.sh` reads to learn what it must protect.
///
/// Resolved the way `crate::env` resolves the data home — `$STORYHOOK_DATA_DIR`,
/// else `$XDG_DATA_HOME/storyhook`, else `~/.local/share/storyhook` — because
/// the hook has to find it in three lines of shell, with no `story` invocation:
/// a `PreToolUse` hook fails OPEN at its timeout (SH-306), and it may be asked
/// to run while the binary it would have called is mid-replacement.
pub fn managed_paths_file() -> Result<PathBuf, AppError> {
    Ok(data_dir()?.join("managed-paths"))
}

/// Records [`managed_paths`] where the hook can read them.
///
/// Best effort on purpose: failing an otherwise successful install because a
/// convenience file could not be written would be the wrong trade, and the
/// hook's own absence-of-manifest behaviour is to stay inert rather than to
/// guess.
fn record_managed_paths() {
    let (Ok(file), Ok(paths)) = (managed_paths_file(), managed_paths()) else {
        return;
    };
    let Some(parent) = file.parent() else { return };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let mut body = String::from(
        "# Written by `story plugin install`. One path prefix per line.\n         # `plugins/story/hooks/protect-install.sh` refuses edits beneath these.\n",
    );
    for path in paths {
        body.push_str(&path.display().to_string());
        body.push('\n');
    }
    let _ = fs::write(&file, body);
}

pub fn install(target: &str, project_root: &Path) -> Result<String, AppError> {
    let warning = compatibility_alias_warning(target);
    let target = PluginTarget::parse(target)?;
    preflight_provider(target)?;
    let marketplace = materialize_release_marketplace()?;
    record_managed_paths();
    let source = marketplace.display().to_string();
    let message = match target {
        PluginTarget::ClaudeCode => install_claude(project_root, &source),
        PluginTarget::Codex => install_codex(project_root, &source),
    }?;
    Ok(format!("{}{message}", warning.unwrap_or_default()))
}

fn uninstall_claude(project_root: &Path) -> Result<String, AppError> {
    let mut removed = Vec::new();

    if provider_available(PluginTarget::ClaudeCode) {
        let uninstalled = run_provider(
            PluginTarget::ClaudeCode,
            &["plugin", "uninstall", PLUGIN_REF],
        )?;
        if uninstalled.status.success() {
            removed.push(format!("unregistered {PLUGIN_REF} via claude"));
        }
        let _ = run_provider(
            PluginTarget::ClaudeCode,
            &["plugin", "marketplace", "remove", MARKETPLACE_NAME],
        );
    }

    let legacy = claude_plugins_dir()?.join(LEGACY_PLUGIN_DIR_NAME);
    if legacy.exists() {
        fs::remove_dir_all(&legacy)?;
        removed.push(format!(
            "removed legacy plugin directory {}",
            legacy.display()
        ));
    }

    let claude_md_path = project_root.join("CLAUDE.md");
    if claude_md_path.exists() {
        let content = fs::read_to_string(&claude_md_path)?;
        if content.contains(SENTINEL_BEGIN) {
            let cleaned = remove_sentinel_section(&content);
            fs::write(&claude_md_path, cleaned)?;
            removed.push("removed storyhook section from CLAUDE.md".to_string());
        }
    }

    if removed.is_empty() {
        Ok("nothing to remove — storyhook plugin was not installed".to_string())
    } else {
        Ok(removed.join("\n"))
    }
}

fn uninstall_codex(project_root: &Path) -> Result<String, AppError> {
    if !provider_available(PluginTarget::Codex) {
        return Err(AppError::Storage(missing_message(PluginTarget::Codex)));
    }
    remove_codex_plugin()?;
    remove_codex_marketplace()?;
    let mut message =
        format!("removed `{PLUGIN_REF}` and the `{MARKETPLACE_NAME}` marketplace from Codex");

    let home = home_dir()?;
    let launcher = codex_launcher_path(&home);
    let rule = codex_rule_path(&home);
    match remove_managed_file(&launcher, CODEX_LAUNCHER_MARKER)? {
        ManagedRemoval::Removed => {
            message.push_str(&format!(
                "\nremoved the Storyhook launcher {}",
                launcher.display()
            ));
        }
        ManagedRemoval::Preserved => {
            message.push_str(&format!(
                "\npreserved unmanaged launcher file {}",
                launcher.display()
            ));
        }
        ManagedRemoval::Missing => {}
    }
    match remove_managed_file(&rule, CODEX_RULE_MARKER)? {
        ManagedRemoval::Removed => {
            message.push_str(&format!(
                "\nremoved the Storyhook sandbox rule {}",
                rule.display()
            ));
        }
        ManagedRemoval::Preserved => {
            message.push_str(&format!(
                "\npreserved unmanaged rules file {}",
                rule.display()
            ));
        }
        ManagedRemoval::Missing => {}
    }
    if let Some(parent) = launcher.parent() {
        remove_empty_dir(parent);
    }

    // Setup may add one explicitly delimited Storyhook block to a pre-existing
    // AGENTS.md. Remove only that complete block. `story project new` writes a
    // canonical file with no sentinel, and user-authored/malformed files are
    // therefore left byte-for-byte untouched.
    let agents_md_path = project_root.join("AGENTS.md");
    if agents_md_path.exists() {
        let content = fs::read_to_string(&agents_md_path)?;
        if let Some(cleaned) = remove_complete_sentinel_section(
            &content,
            INSTRUCTIONS_SENTINEL_BEGIN,
            INSTRUCTIONS_SENTINEL_END,
        ) {
            fs::write(&agents_md_path, cleaned)?;
            message.push_str("\nremoved the Storyhook sentinel block from AGENTS.md");
        }
    }
    Ok(message)
}

pub fn uninstall(target: &str, project_root: &Path) -> Result<String, AppError> {
    let warning = compatibility_alias_warning(target);
    let message = match PluginTarget::parse(target)? {
        PluginTarget::ClaudeCode => uninstall_claude(project_root),
        PluginTarget::Codex => uninstall_codex(project_root),
    }?;
    Ok(format!("{}{message}", warning.unwrap_or_default()))
}

fn remove_sentinel_section(content: &str) -> String {
    let mut result = String::new();
    let mut in_section = false;

    for line in content.lines() {
        if line.trim() == SENTINEL_BEGIN {
            in_section = true;
            continue;
        }
        if line.trim() == SENTINEL_END {
            in_section = false;
            continue;
        }
        if !in_section {
            result.push_str(line);
            result.push('\n');
        }
    }

    let trimmed = result.trim_end();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

fn remove_complete_sentinel_section(content: &str, begin: &str, end: &str) -> Option<String> {
    if content.match_indices(begin).count() != 1 || content.match_indices(end).count() != 1 {
        return None;
    }
    let begin_at = content.find(begin)?;
    let end_at = content.find(end)?;
    if end_at <= begin_at + begin.len() {
        return None;
    }

    let begin_line_start = content[..begin_at].rfind('\n').map_or(0, |at| at + 1);
    let begin_line_end = content[begin_at..]
        .find('\n')
        .map_or(content.len(), |at| begin_at + at);
    let end_line_start = content[..end_at].rfind('\n').map_or(0, |at| at + 1);
    let end_line_content_end = content[end_at..]
        .find('\n')
        .map_or(content.len(), |at| end_at + at);
    let end_line_end = if end_line_content_end < content.len() {
        end_line_content_end + 1
    } else {
        end_line_content_end
    };
    if content[begin_line_start..begin_line_end].trim_end_matches('\r') != begin
        || content[end_line_start..end_line_content_end].trim_end_matches('\r') != end
    {
        return None;
    }

    let mut cleaned = String::with_capacity(content.len());
    cleaned.push_str(&content[..begin_line_start]);
    cleaned.push_str(&content[end_line_end..]);
    Some(cleaned)
}

#[cfg(test)]
mod tests {
    /// Every managed-path constant this module writes through must appear in
    /// [`managed_paths`], because that list is what
    /// `hooks/protect-install.sh` reads and therefore the whole of what the
    /// hook can protect.
    ///
    /// Derived from the constants rather than from a second list of paths: a
    /// hand-kept copy is the shape SH-136, SH-198, SH-258, SH-260/276, SH-360
    /// and SH-364 each cost this project once, and a guard whose coverage
    /// silently narrows is worse than no guard, because it still reads as
    /// protection.
    #[test]
    fn every_managed_location_this_module_writes_is_declared() {
        // SAFETY: single-threaded test setup, and the value is read back
        // immediately by `managed_paths` in this same thread.
        unsafe { std::env::set_var("HOME", "/tmp/storyhook-managed-paths-fixture") };
        let declared = super::managed_paths().expect("resolving managed paths");
        let rendered: Vec<String> = declared.iter().map(|p| p.display().to_string()).collect();
        let joined = rendered.join("\n");

        for needle in [
            MARKETPLACE_NAME,
            LEGACY_PLUGIN_DIR_NAME,
            // The parent directory of the launcher, not the file itself.
            CODEX_LAUNCHER_RELATIVE
                .rsplit_once('/')
                .expect("the launcher path has a directory")
                .0,
            CODEX_RULE_RELATIVE,
        ] {
            assert!(
                joined.contains(needle),
                "`{needle}` is written by this module but is not covered by \
                 `managed_paths`, so `hooks/protect-install.sh` cannot protect \
                 it:\n{joined}"
            );
        }

        // Positive control: a list that resolved to nothing would satisfy no
        // assertion above by accident, but an empty one would make every
        // `contains` fail for the wrong reason. Say which it is.
        assert!(
            rendered.len() >= 4,
            "expected the claude cache, the claude marketplace, the codex cache \
             and the codex managed files; got {rendered:?}"
        );

        // The `story` binary must NOT be here: `make install` is the recovery
        // `StoreError::SchemaTooNew` prescribes, and refusing it would make the
        // store's own advice a dead end (SH-404, SH-405).
        assert!(
            !joined.contains("/bin/story\n") && !joined.ends_with("/bin/story"),
            "the installed binary must stay editable — `make install` is a \
             sanctioned recovery:\n{joined}"
        );
    }

    use super::*;

    #[test]
    fn targets_are_typed_and_the_error_lists_both() {
        assert_eq!(
            PluginTarget::parse("claude").unwrap(),
            PluginTarget::ClaudeCode
        );
        assert_eq!(
            PluginTarget::parse("claude-code").unwrap(),
            PluginTarget::ClaudeCode,
            "the previous public token remains a compatibility alias"
        );
        assert_eq!(PluginTarget::parse("codex").unwrap(), PluginTarget::Codex);
        let error = PluginTarget::parse("vscode").unwrap_err().to_string();
        assert!(error.contains("claude, codex"), "{error}");
        assert!(!error.contains("claude-code"), "{error}");
    }

    #[test]
    fn the_old_claude_token_is_a_warned_alias() {
        let warning = compatibility_alias_warning("claude-code").expect("legacy alias warning");
        assert!(warning.contains("deprecated"));
        assert!(warning.contains("`claude`"));
        assert_eq!(compatibility_alias_warning("claude"), None);
        assert_eq!(compatibility_alias_warning("codex"), None);
    }

    #[test]
    fn provider_missing_messages_return_to_the_release_aware_installer() {
        let claude = missing_message(PluginTarget::ClaudeCode);
        assert!(claude.contains("story plugin install claude"));
        assert!(!claude.contains("mikeydotio/storyhook"));
        let codex = missing_message(PluginTarget::Codex);
        assert!(codex.contains("story plugin install codex"));
        assert!(!codex.contains("mikeydotio/storyhook"));
    }

    #[test]
    fn codex_plugin_root_uses_the_authoritative_installed_version() {
        let home = storyhook_test_support::scratch_dir();
        let stale = home
            .path()
            .join(".codex/plugins/cache/storyhook/story/0.5.0/.codex-plugin");
        let current = home
            .path()
            .join(".codex/plugins/cache/storyhook/story/0.6.0+codex.123/.codex-plugin");
        fs::create_dir_all(&stale).unwrap();
        fs::create_dir_all(&current).unwrap();
        fs::write(stale.join("plugin.json"), "{}").unwrap();
        fs::write(current.join("plugin.json"), "{}").unwrap();
        let raw = br#"{"installed":[{"pluginId":"story@storyhook","name":"story","marketplaceName":"storyhook","version":"0.6.0+codex.123","installed":true,"enabled":true}]}"#;
        assert_eq!(
            codex_installed_plugin_root_from(home.path(), raw),
            Some(current.parent().unwrap().to_path_buf())
        );
    }

    #[test]
    fn codex_plugin_root_rejects_disabled_or_unsafe_records() {
        let home = storyhook_test_support::scratch_dir();
        for raw in [
            br#"{"installed":[{"pluginId":"story@storyhook","name":"story","marketplaceName":"storyhook","version":"0.6.0","installed":true,"enabled":false}]}"#.as_slice(),
            br#"{"installed":[{"pluginId":"story@storyhook","name":"story","marketplaceName":"storyhook","version":"../escape","installed":true,"enabled":true}]}"#.as_slice(),
        ] {
            assert_eq!(codex_installed_plugin_root_from(home.path(), raw), None);
        }
    }

    #[test]
    fn removes_sentinel_section() {
        let input = "# My Project\n\nSome content.\n\n<!-- storyhook:begin -->\n## Storyhook\nWorkflow stuff.\n<!-- storyhook:end -->\n\nMore content.\n";
        let expected = "# My Project\n\nSome content.\n\n\nMore content.\n";
        assert_eq!(remove_sentinel_section(input), expected);
    }

    #[test]
    fn no_sentinel_returns_unchanged() {
        let input = "# My Project\n\nContent.\n";
        assert_eq!(remove_sentinel_section(input), input);
    }

    #[test]
    fn removes_only_one_complete_instruction_sentinel_block() {
        let input =
            "user before\n<!-- BEGIN STORYHOOK -->\nmanaged\n<!-- END STORYHOOK -->\nuser after\n";
        assert_eq!(
            remove_complete_sentinel_section(
                input,
                INSTRUCTIONS_SENTINEL_BEGIN,
                INSTRUCTIONS_SENTINEL_END,
            ),
            Some("user before\nuser after\n".to_string())
        );
        let malformed = "user before\n<!-- BEGIN STORYHOOK -->\nmanaged\nuser after\n";
        assert_eq!(
            remove_complete_sentinel_section(
                malformed,
                INSTRUCTIONS_SENTINEL_BEGIN,
                INSTRUCTIONS_SENTINEL_END,
            ),
            None
        );
        let suffixed =
            "user before\n<!-- BEGIN STORYHOOK -->\nmanaged\n<!-- END STORYHOOK --> user suffix\n";
        assert_eq!(
            remove_complete_sentinel_section(
                suffixed,
                INSTRUCTIONS_SENTINEL_BEGIN,
                INSTRUCTIONS_SENTINEL_END,
            ),
            None
        );
    }
}
