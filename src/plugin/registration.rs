//! What a provider has registered as the storyhook marketplace, read from the
//! provider's own configuration rather than by invoking it (SH-641).
//!
//! `story doctor install` needs this answer on a machine where the provider
//! CLI is not installed and must not pay a subprocess for it; the installer
//! needs it *before* it removes the registration, so that a later failure can
//! put it back. One parser serves both (SH-136): a second copy would drift the
//! first time a provider changed its config layout.

use std::path::{Path, PathBuf};

use super::{MARKETPLACE_NAME, PluginTarget};

/// The provider configuration file that records marketplace registrations.
pub(crate) fn config_path(home: &Path, target: PluginTarget) -> PathBuf {
    match target {
        PluginTarget::ClaudeCode => home.join(".claude/plugins/known_marketplaces.json"),
        PluginTarget::Codex => home.join(".codex/config.toml"),
    }
}

/// The storyhook marketplace source a provider configuration names.
///
/// `Ok(None)` is a configuration with no storyhook marketplace in it; `Err`
/// is a configuration this parser could not read, with the reason, which the
/// caller reports rather than resolving (SH-372: an unreadable record states
/// nothing and is never promoted to "there was nothing").
pub(crate) fn configured_source(
    body: &str,
    target: PluginTarget,
) -> Result<Option<String>, String> {
    match target {
        PluginTarget::ClaudeCode => {
            let value: serde_json::Value = serde_json::from_str(body)
                .map_err(|error| format!("its configuration is invalid JSON: {error}"))?;
            let Some(marketplace) = value.get(MARKETPLACE_NAME) else {
                return Ok(None);
            };
            let source = marketplace
                .get("source")
                .ok_or_else(|| "its storyhook marketplace has no `source` record".to_string())?;
            if let Some(source) = source.as_str() {
                return Ok(Some(source.to_string()));
            }
            for key in ["path", "repo", "url"] {
                if let Some(source) = source.get(key).and_then(serde_json::Value::as_str) {
                    return Ok(Some(source.to_string()));
                }
            }
            Err("its storyhook marketplace source has no path, repository or URL".to_string())
        }
        PluginTarget::Codex => {
            let value: toml::Value = toml::from_str(body)
                .map_err(|error| format!("its configuration is invalid TOML: {error}"))?;
            let Some(marketplace) = value
                .get("marketplaces")
                .and_then(|value| value.get(MARKETPLACE_NAME))
            else {
                return Ok(None);
            };
            marketplace
                .get("source")
                .and_then(toml::Value::as_str)
                .map(str::to_string)
                .map(Some)
                .ok_or_else(|| "its storyhook marketplace has no string `source`".to_string())
        }
    }
}
