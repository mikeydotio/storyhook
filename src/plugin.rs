use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::AppError;

/// Name of the marketplace declared in `.claude-plugin/marketplace.json`.
const MARKETPLACE_NAME: &str = "storyhook";
/// Plugin reference used with the `claude` CLI: `<plugin>@<marketplace>`.
const PLUGIN_REF: &str = "storyhook@storyhook";
/// GitHub shorthand source used when not installing from a local dev checkout.
const GITHUB_REPO: &str = "mikeydotio/storyhook";
/// Legacy directory created by the old copy-based installer (cleaned up on uninstall).
const LEGACY_PLUGIN_DIR_NAME: &str = "storyhook";
const SENTINEL_BEGIN: &str = "<!-- storyhook:begin -->";
const SENTINEL_END: &str = "<!-- storyhook:end -->";

/// Resolve the Claude Code config directory (~/.claude/).
fn claude_dir() -> Result<PathBuf, AppError> {
    let home = std::env::var("HOME")
        .map_err(|_| AppError::Storage("could not determine home directory".to_string()))?;
    Ok(PathBuf::from(home).join(".claude"))
}

/// Resolve the Claude Code plugins directory (~/.claude/plugins/).
fn claude_plugins_dir() -> Result<PathBuf, AppError> {
    Ok(claude_dir()?.join("plugins"))
}

/// Detect whether we are running inside a storyhook dev checkout, returning the
/// repo root (the directory containing `.claude-plugin/marketplace.json`).
///
/// When present, the local repo is used as the marketplace source so contributors
/// install their working copy instead of the published GitHub version.
fn dev_repo_root() -> Option<PathBuf> {
    let has_marketplace = |dir: &Path| dir.join(".claude-plugin").join("marketplace.json").exists();

    // Current working directory (the common dev case: running from the repo).
    if let Ok(cwd) = std::env::current_dir()
        && has_marketplace(&cwd)
    {
        return Some(cwd);
    }

    // Relative to the executable, e.g. `<repo>/target/{release,debug}/story`.
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

/// Choose the marketplace source string for `claude plugin marketplace add`.
///
/// A dev checkout installs from its local path; otherwise the published GitHub
/// repository shorthand is used.
fn marketplace_source(dev_repo_root: Option<PathBuf>) -> String {
    match dev_repo_root {
        Some(path) => path.display().to_string(),
        None => GITHUB_REPO.to_string(),
    }
}

/// Guidance shown whenever the `claude` CLI cannot be found on PATH.
fn claude_missing_message() -> String {
    "Claude Code CLI (`claude`) not found on PATH. Install or enable it, then retry — \
     or install the plugin manually with `/plugin marketplace add mikeydotio/storyhook` \
     followed by `/plugin install storyhook@storyhook`."
        .to_string()
}

/// Returns true if the `claude` CLI is invokable on PATH.
fn claude_available() -> bool {
    Command::new("claude")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

/// Run a `claude` subcommand, capturing its output.
///
/// A missing binary is mapped to a clear, actionable error rather than a raw IO error.
fn run_claude(args: &[&str]) -> Result<std::process::Output, AppError> {
    Command::new("claude").args(args).output().map_err(|e| {
        if e.kind() == ErrorKind::NotFound {
            AppError::Storage(claude_missing_message())
        } else {
            AppError::Storage(format!("failed to run `claude {}`: {e}", args.join(" ")))
        }
    })
}

/// Combine stdout + stderr from a process output into a single lowercased string,
/// used for tolerant matching of "already exists"-style messages.
fn combined_output(out: &std::process::Output) -> String {
    let mut s = String::from_utf8_lossy(&out.stdout).to_string();
    s.push('\n');
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s.to_lowercase()
}

/// Register the storyhook marketplace, tolerating the case where it is already added.
fn add_marketplace(source: &str) -> Result<(), AppError> {
    let out = run_claude(&["plugin", "marketplace", "add", source])?;
    if out.status.success() {
        return Ok(());
    }
    let combined = combined_output(&out);
    if combined.contains("already") {
        return Ok(());
    }
    Err(AppError::Storage(format!(
        "failed to add storyhook marketplace from `{source}`:\n{}",
        combined.trim()
    )))
}

/// Install (or re-install) the storyhook plugin via the marketplace.
fn install_plugin() -> Result<(), AppError> {
    let out = run_claude(&["plugin", "install", PLUGIN_REF, "--scope", "user"])?;
    if out.status.success() {
        return Ok(());
    }
    let combined = combined_output(&out);
    if combined.contains("already") {
        return Ok(());
    }
    Err(AppError::Storage(format!(
        "failed to install `{PLUGIN_REF}`:\n{}",
        combined.trim()
    )))
}

pub fn install(target: &str, project_root: &Path) -> Result<String, AppError> {
    if target != "claude-code" {
        return Err(AppError::Usage(format!(
            "unknown plugin target: {target}. Supported: claude-code"
        )));
    }

    // Verify Claude Code is present at all.
    if !claude_dir()?.exists() {
        return Err(AppError::Storage(
            "Claude Code not detected (~/.claude/ does not exist). \
             Install Claude Code first, then retry."
                .to_string(),
        ));
    }

    // The new install path requires the `claude` CLI for proper registration.
    if !claude_available() {
        return Err(AppError::Storage(claude_missing_message()));
    }

    let source = marketplace_source(dev_repo_root());
    add_marketplace(&source)?;
    install_plugin()?;

    // Create default plugin config in the project if .storyhook/ exists.
    let config_path = project_root.join(".storyhook").join("plugin-config.toml");
    let mut wrote_config = false;
    if project_root.join(".storyhook").exists() && !config_path.exists() {
        fs::write(
            &config_path,
            "[plugin]\nenabled = true\ntracking = \"normal\"\n",
        )?;
        wrote_config = true;
    }

    let mut msg = format!(
        "registered storyhook plugin via the `{MARKETPLACE_NAME}` marketplace (source: {source})\n"
    );
    if wrote_config {
        msg.push_str("created .storyhook/plugin-config.toml with default settings\n");
    }
    msg.push_str(
        "\nStart a new Claude Code session to load the plugin, then run \
         /storyhook:storyhook-context to get started (or run story load-context directly).",
    );
    Ok(msg)
}

pub fn uninstall(target: &str, project_root: &Path) -> Result<String, AppError> {
    if target != "claude-code" {
        return Err(AppError::Usage(format!(
            "unknown plugin target: {target}. Supported: claude-code"
        )));
    }

    let mut removed = Vec::new();

    // Unregister via the claude CLI when available, tolerating "not installed".
    if claude_available() {
        let uninstalled = run_claude(&["plugin", "uninstall", PLUGIN_REF])?;
        if uninstalled.status.success() {
            removed.push(format!("unregistered {PLUGIN_REF} via claude"));
        }
        // Remove the now-empty marketplace too (best effort).
        let _ = run_claude(&["plugin", "marketplace", "remove", MARKETPLACE_NAME]);
    }

    // Remove the legacy bare directory created by the old copy-based installer.
    let legacy = claude_plugins_dir()?.join(LEGACY_PLUGIN_DIR_NAME);
    if legacy.exists() {
        fs::remove_dir_all(&legacy)?;
        removed.push(format!(
            "removed legacy plugin directory {}",
            legacy.display()
        ));
    }

    // Remove plugin config.
    let config_path = project_root.join(".storyhook").join("plugin-config.toml");
    if config_path.exists() {
        fs::remove_file(&config_path)?;
        removed.push("removed .storyhook/plugin-config.toml".to_string());
    }

    // Remove sentinel-marked section from CLAUDE.md if present.
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

    // Trim trailing whitespace but keep a final newline
    let trimmed = result.trim_end();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn claude_missing_message_mentions_manual_install() {
        let msg = claude_missing_message();
        assert!(msg.contains("/plugin marketplace add mikeydotio/storyhook"));
        assert!(msg.contains("/plugin install storyhook@storyhook"));
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
}
