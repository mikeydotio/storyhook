use std::fs;
use std::path::{Path, PathBuf};

use crate::error::AppError;

const PLUGIN_DIR_NAME: &str = "storyhook";
const SENTINEL_BEGIN: &str = "<!-- storyhook:begin -->";
const SENTINEL_END: &str = "<!-- storyhook:end -->";

/// Resolve the Claude Code plugins directory (~/.claude/plugins/).
fn claude_plugins_dir() -> Result<PathBuf, AppError> {
    let home = std::env::var("HOME")
        .map_err(|_| AppError::Storage("could not determine home directory".to_string()))?;
    Ok(PathBuf::from(home).join(".claude").join("plugins"))
}

/// Resolve where the bundled plugin source lives (sibling to the binary or in the repo).
fn bundled_plugin_source() -> Option<PathBuf> {
    // Check relative to the current executable
    if let Ok(exe) = std::env::current_exe() {
        // Development: repo/plugin/claude-code/
        if let Some(repo_root) = exe.parent().and_then(|p| p.parent()) {
            let candidate = repo_root.join("plugin").join("claude-code");
            if candidate.join(".claude-plugin").join("plugin.json").exists() {
                return Some(candidate);
            }
        }
        // Installed: next to binary in plugin/claude-code/
        if let Some(bin_dir) = exe.parent() {
            let candidate = bin_dir.join("plugin").join("claude-code");
            if candidate.join(".claude-plugin").join("plugin.json").exists() {
                return Some(candidate);
            }
        }
    }

    // Check relative to CWD (for development)
    let candidate = PathBuf::from("plugin/claude-code");
    if candidate.join(".claude-plugin").join("plugin.json").exists() {
        return Some(candidate);
    }

    None
}

/// Recursively copy a directory tree.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), AppError> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

pub fn install(target: &str, project_root: &Path) -> Result<String, AppError> {
    if target != "claude-code" {
        return Err(AppError::Usage(format!(
            "unknown plugin target: {target}. Supported: claude-code"
        )));
    }

    let plugins_dir = claude_plugins_dir()?;
    let claude_dir = plugins_dir.parent().unwrap_or(&plugins_dir);
    if !claude_dir.exists() {
        return Err(AppError::Storage(
            "Claude Code not detected (~/.claude/ does not exist). \
             Install Claude Code first, then retry."
                .to_string(),
        ));
    }

    let source = bundled_plugin_source().ok_or_else(|| {
        AppError::Storage(
            "could not find bundled plugin files. \
             Ensure the plugin/claude-code/ directory exists next to the storyhook binary or in the current repo."
                .to_string(),
        )
    })?;

    let dest = plugins_dir.join(PLUGIN_DIR_NAME);
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    copy_dir_recursive(&source, &dest)?;

    // Create default plugin config in the project if .storyhook/ exists
    let config_path = project_root.join(".storyhook").join("plugin-config.toml");
    if project_root.join(".storyhook").exists() && !config_path.exists() {
        fs::write(
            &config_path,
            "[plugin]\nenabled = true\ntracking = \"normal\"\n",
        )?;
    }

    let mut msg = format!("installed storyhook plugin to {}\n", dest.display());
    if config_path.exists() {
        msg.push_str("created .storyhook/plugin-config.toml with default settings\n");
    }
    msg.push_str("\nThe plugin adds session hooks and workflow skills to Claude Code.\n\
                   Use /storyhook:context to get started.");
    Ok(msg)
}

pub fn uninstall(target: &str, project_root: &Path) -> Result<String, AppError> {
    if target != "claude-code" {
        return Err(AppError::Usage(format!(
            "unknown plugin target: {target}. Supported: claude-code"
        )));
    }

    let mut removed = Vec::new();

    // Remove plugin directory
    let dest = claude_plugins_dir()?.join(PLUGIN_DIR_NAME);
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
        removed.push(format!("removed {}", dest.display()));
    }

    // Remove plugin config
    let config_path = project_root.join(".storyhook").join("plugin-config.toml");
    if config_path.exists() {
        fs::remove_file(&config_path)?;
        removed.push("removed .storyhook/plugin-config.toml".to_string());
    }

    // Remove sentinel-marked section from CLAUDE.md if present
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
