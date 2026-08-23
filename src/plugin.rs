use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::env::spawn_env::apply_plugin_cli_allowlist;
use crate::error::AppError;

const MARKETPLACE_NAME: &str = "storyhook";
const PLUGIN_REF: &str = "story@storyhook";
const GITHUB_REPO: &str = "mikeydotio/storyhook";
const LEGACY_PLUGIN_DIR_NAME: &str = "storyhook";
const SENTINEL_BEGIN: &str = "<!-- storyhook:begin -->";
const SENTINEL_END: &str = "<!-- storyhook:end -->";
const INSTRUCTIONS_SENTINEL_BEGIN: &str = "<!-- BEGIN STORYHOOK -->";
const INSTRUCTIONS_SENTINEL_END: &str = "<!-- END STORYHOOK -->";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PluginTarget {
    ClaudeCode,
    Codex,
}

impl PluginTarget {
    fn parse(raw: &str) -> Result<Self, AppError> {
        match raw {
            "claude-code" => Ok(Self::ClaudeCode),
            "codex" => Ok(Self::Codex),
            _ => Err(AppError::Usage(format!(
                "unknown plugin target: {raw}. Supported: claude-code, codex"
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

/// Resolve the Claude Code config directory (~/.claude/).
fn claude_dir() -> Result<PathBuf, AppError> {
    let home = std::env::var("HOME")
        .map_err(|_| AppError::Storage("could not determine home directory".to_string()))?;
    Ok(PathBuf::from(home).join(".claude"))
}

fn claude_plugins_dir() -> Result<PathBuf, AppError> {
    Ok(claude_dir()?.join("plugins"))
}

/// Detect a Storyhook development checkout from either provider marketplace.
/// A contributor installs the checkout; a packaged binary registers GitHub.
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

fn marketplace_source(dev_repo_root: Option<PathBuf>) -> String {
    match dev_repo_root {
        Some(path) => path.display().to_string(),
        None => GITHUB_REPO.to_string(),
    }
}

fn missing_message(target: PluginTarget) -> String {
    match target {
        PluginTarget::ClaudeCode => {
            "Claude Code CLI (`claude`) not found on PATH. Install or enable it, then retry — \
             or install the plugin manually with `/plugin marketplace add mikeydotio/storyhook` \
             followed by `/plugin install story@storyhook`."
                .to_string()
        }
        PluginTarget::Codex => {
            "Codex CLI (`codex`) not found on PATH. Install or enable Codex, then retry — \
             or register the marketplace and plugin manually with `codex plugin marketplace add \
             mikeydotio/storyhook` followed by `codex plugin add story@storyhook`."
                .to_string()
        }
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

fn add_claude_marketplace(source: &str) -> Result<(), AppError> {
    let out = run_provider(
        PluginTarget::ClaudeCode,
        &["plugin", "marketplace", "add", source],
    )?;
    if out.status.success() || combined_output(&out).to_lowercase().contains("already") {
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

fn add_codex_marketplace(source: &str) -> Result<bool, AppError> {
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
    })
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
    if !claude_dir()?.exists() {
        return Err(AppError::Storage(
            "Claude Code not detected (~/.claude/ does not exist). \
             Install Claude Code first, then retry."
                .to_string(),
        ));
    }
    if !provider_available(PluginTarget::ClaudeCode) {
        return Err(AppError::Storage(missing_message(PluginTarget::ClaudeCode)));
    }

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
    if !provider_available(PluginTarget::Codex) {
        return Err(AppError::Storage(missing_message(PluginTarget::Codex)));
    }

    let already_added = add_codex_marketplace(source)?;
    let installed_path = add_codex_plugin()?;
    let marketplace_status = if already_added {
        "already registered"
    } else {
        "registered"
    };
    let mut message = format!(
        "{marketplace_status} the `{MARKETPLACE_NAME}` marketplace (source: {source})\n\
         installed `{PLUGIN_REF}` at {installed_path}\n"
    );
    append_settings_guidance(&mut message, project_root);
    message.push_str(
        "\nStart a new Codex conversation to load the Storyhook skills, then select the \
         `story-context` skill or ask for the current Storyhook project context.",
    );
    Ok(message)
}

pub fn install(target: &str, project_root: &Path) -> Result<String, AppError> {
    let target = PluginTarget::parse(target)?;
    let source = marketplace_source(dev_repo_root());
    match target {
        PluginTarget::ClaudeCode => install_claude(project_root, &source),
        PluginTarget::Codex => install_codex(project_root, &source),
    }
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
    match PluginTarget::parse(target)? {
        PluginTarget::ClaudeCode => uninstall_claude(project_root),
        PluginTarget::Codex => uninstall_codex(project_root),
    }
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
    use super::*;

    #[test]
    fn targets_are_typed_and_the_error_lists_both() {
        assert_eq!(
            PluginTarget::parse("claude-code").unwrap(),
            PluginTarget::ClaudeCode
        );
        assert_eq!(PluginTarget::parse("codex").unwrap(), PluginTarget::Codex);
        let error = PluginTarget::parse("vscode").unwrap_err().to_string();
        assert!(error.contains("claude-code, codex"), "{error}");
    }

    #[test]
    fn marketplace_source_uses_local_path_for_dev_checkout() {
        let dev = PathBuf::from("/Volumes/Code/mikeyward/storyhook");
        assert_eq!(
            marketplace_source(Some(dev)),
            "/Volumes/Code/mikeyward/storyhook"
        );
    }

    #[test]
    fn marketplace_source_falls_back_to_github_repo() {
        assert_eq!(marketplace_source(None), "mikeydotio/storyhook");
    }

    #[test]
    fn provider_missing_messages_name_manual_install() {
        let claude = missing_message(PluginTarget::ClaudeCode);
        assert!(claude.contains("/plugin marketplace add mikeydotio/storyhook"));
        assert!(claude.contains("/plugin install story@storyhook"));
        let codex = missing_message(PluginTarget::Codex);
        assert!(codex.contains("codex plugin marketplace add mikeydotio/storyhook"));
        assert!(codex.contains("codex plugin add story@storyhook"));
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
