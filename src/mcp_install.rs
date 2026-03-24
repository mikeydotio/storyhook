use std::fs;
use std::path::PathBuf;
use toml::map::Map as TomlMap;

use dialoguer::MultiSelect;
use dialoguer::theme::ColorfulTheme;

use crate::error::AppError;

struct Provider {
    name: &'static str,
    label: String,
    path: PathBuf,
    format: ConfigFormat,
}

enum ConfigFormat {
    /// JSON with `mcpServers` root key (Claude Code, Cursor, Antigravity)
    JsonMcpServers,
    /// TOML with `[mcp_servers.<name>]` tables (Codex CLI)
    TomlMcpServers,
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn discover_providers() -> Vec<Provider> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };

    let mut providers = Vec::new();

    // Claude Code
    let claude_path = home.join(".claude.json");
    providers.push(Provider {
        name: "Claude Code",
        label: format!("Claude Code ({})", claude_path.display()),
        path: claude_path,
        format: ConfigFormat::JsonMcpServers,
    });

    // Cursor
    let cursor_path = home.join(".cursor/mcp.json");
    providers.push(Provider {
        name: "Cursor",
        label: format!("Cursor ({})", cursor_path.display()),
        path: cursor_path,
        format: ConfigFormat::JsonMcpServers,
    });

    // Codex CLI
    let codex_path = home.join(".codex/config.toml");
    providers.push(Provider {
        name: "Codex CLI",
        label: format!("Codex CLI ({})", codex_path.display()),
        path: codex_path,
        format: ConfigFormat::TomlMcpServers,
    });

    // Antigravity
    let antigravity_path = home.join(".gemini/antigravity/mcp_config.json");
    providers.push(Provider {
        name: "Antigravity",
        label: format!("Antigravity ({})", antigravity_path.display()),
        path: antigravity_path,
        format: ConfigFormat::JsonMcpServers,
    });

    providers
}

fn resolve_binary_path() -> String {
    if let Ok(output) = std::process::Command::new("which").arg("story").output()
        && output.status.success()
    {
        return "story".to_string();
    }
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "story".to_string())
}

fn write_json_config(path: &PathBuf, binary_path: &str) -> Result<String, AppError> {
    // Read existing config or start with empty object
    let mut root: serde_json::Map<String, serde_json::Value> = if path.exists() {
        let content = fs::read_to_string(path)?;
        serde_json::from_str(&content).map_err(|e| {
            AppError::Storage(format!(
                "failed to parse {}: {e}\nRefusing to modify a file we cannot parse.",
                path.display()
            ))
        })?
    } else {
        serde_json::Map::new()
    };

    // Get or create mcpServers object
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));

    let servers_map = servers.as_object_mut().ok_or_else(|| {
        AppError::Storage(format!("{}: mcpServers is not an object", path.display()))
    })?;

    // Check if already registered
    if servers_map.contains_key("storyhook") {
        return Ok(format!(
            "  {} — already registered, updated",
            path.display()
        ));
    }

    // Add storyhook entry
    servers_map.insert(
        "storyhook".to_string(),
        serde_json::json!({
            "command": binary_path,
            "args": ["--mcp"]
        }),
    );

    // Write back
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json_str = serde_json::to_string_pretty(&serde_json::Value::Object(root))?;
    fs::write(path, json_str + "\n")?;

    Ok(format!("  {} — registered", path.display()))
}

fn write_toml_config(path: &PathBuf, binary_path: &str) -> Result<String, AppError> {
    // Read existing config or start with empty table
    let mut root: TomlMap<String, toml::Value> = if path.exists() {
        let content = fs::read_to_string(path)?;
        toml::from_str(&content).map_err(|e| {
            AppError::Storage(format!(
                "failed to parse {}: {e}\nRefusing to modify a file we cannot parse.",
                path.display()
            ))
        })?
    } else {
        TomlMap::new()
    };

    // Get or create mcp_servers table
    let servers = root
        .entry("mcp_servers".to_string())
        .or_insert_with(|| toml::Value::Table(TomlMap::new()));

    let servers_table = match servers {
        toml::Value::Table(t) => t,
        _ => {
            return Err(AppError::Storage(format!(
                "{}: mcp_servers is not a table",
                path.display()
            )));
        }
    };

    // Check if already registered
    if servers_table.contains_key("storyhook") {
        return Ok(format!(
            "  {} — already registered, updated",
            path.display()
        ));
    }

    // Add storyhook entry
    let mut entry = TomlMap::new();
    entry.insert(
        "command".to_string(),
        toml::Value::String(format!("{binary_path} --mcp")),
    );
    servers_table.insert("storyhook".to_string(), toml::Value::Table(entry));

    // Write back
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let toml_str = toml::to_string_pretty(&root)
        .map_err(|e| AppError::Storage(format!("failed to serialize TOML: {e}")))?;
    fs::write(path, toml_str)?;

    Ok(format!("  {} — registered", path.display()))
}

/// Run the interactive MCP configuration flow.
/// Caller must verify stdout is a terminal before calling.
pub fn run_interactive() -> Result<String, AppError> {
    let providers = discover_providers();
    if providers.is_empty() {
        return Err(AppError::Storage(
            "could not determine home directory".to_string(),
        ));
    }

    let labels: Vec<&str> = providers.iter().map(|p| p.label.as_str()).collect();

    // Pre-select providers whose config files already exist
    let defaults: Vec<bool> = providers.iter().map(|p| p.path.exists()).collect();

    let selections = MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Select MCP clients to configure (space to toggle, enter to confirm)")
        .items(&labels)
        .defaults(&defaults)
        .interact()
        .map_err(|e| AppError::Storage(format!("selection cancelled: {e}")))?;

    if selections.is_empty() {
        return Ok("No clients selected.".to_string());
    }

    let binary_path = resolve_binary_path();
    let mut results = Vec::new();

    for &idx in &selections {
        let provider = &providers[idx];
        let result = match provider.format {
            ConfigFormat::JsonMcpServers => write_json_config(&provider.path, &binary_path),
            ConfigFormat::TomlMcpServers => write_toml_config(&provider.path, &binary_path),
        };

        match result {
            Ok(msg) => results.push(msg),
            Err(e) => results.push(format!("  {} — error: {e}", provider.name)),
        }
    }

    Ok(format!(
        "Registered storyhook MCP server:\n{}",
        results.join("\n")
    ))
}
