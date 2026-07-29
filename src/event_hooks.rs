use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Deserialize;
use wait_timeout::ChildExt;

#[derive(Debug, Deserialize, Default)]
pub struct HooksConfig {
    #[serde(default)]
    pub settings: HooksSettings,
    #[serde(default)]
    pub on_create: Option<HookDef>,
    #[serde(default)]
    pub on_state_change: Option<HookDef>,
    #[serde(default)]
    pub on_close: Option<HookDef>,
    #[serde(default)]
    pub on_comment: Option<HookDef>,
    #[serde(default)]
    pub on_priority_change: Option<HookDef>,
    #[serde(default)]
    pub on_label_change: Option<HookDef>,
    #[serde(default)]
    pub on_relationship_change: Option<HookDef>,
}

#[derive(Debug, Deserialize)]
pub struct HooksSettings {
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_timeout() -> u64 {
    10
}
fn default_enabled() -> bool {
    true
}

impl Default for HooksSettings {
    fn default() -> Self {
        Self {
            timeout_seconds: 10,
            enabled: true,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct HookDef {
    pub command: String,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
pub enum HookEventType {
    Create,
    StateChange,
    Close,
    Comment,
    PriorityChange,
    LabelChange,
    RelationshipChange,
}

impl HookEventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::StateChange => "state_change",
            Self::Close => "close",
            Self::Comment => "comment",
            Self::PriorityChange => "priority_change",
            Self::LabelChange => "label_change",
            Self::RelationshipChange => "relationship_change",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "create" => Some(Self::Create),
            "state_change" | "state-change" => Some(Self::StateChange),
            "close" => Some(Self::Close),
            "comment" => Some(Self::Comment),
            "priority_change" | "priority-change" => Some(Self::PriorityChange),
            "label_change" | "label-change" => Some(Self::LabelChange),
            "relationship_change" | "relationship-change" => Some(Self::RelationshipChange),
            _ => None,
        }
    }
}

/// The repository's event-hook configuration, from whichever of its two homes
/// it lives in.
///
/// The committed pointer file's `[hooks]` table is consulted first and the
/// legacy `.storyhook/hooks.toml` second. Both, rather than one, because the
/// two storage models coexist until the legacy web daemon is retired: a
/// repository that has been moved into the store carries its hooks in the
/// pointer, and one that has not still carries them in the directory. A
/// repository with both is answered by the pointer, which is the file its
/// current storyhook writes about and reads.
pub fn load_hooks_config(root: &Path) -> Option<HooksConfig> {
    if let Some(table) = crate::service::project::pointer_hooks(root) {
        return match table.try_into::<HooksConfig>() {
            Ok(config) => Some(config),
            Err(e) => {
                eprintln!("warning: failed to parse the [hooks] table in .storyhook.toml: {e}");
                None
            }
        };
    }
    let path = root.join(".storyhook/hooks.toml");
    let raw = std::fs::read_to_string(&path).ok()?;
    match toml::from_str(&raw) {
        Ok(config) => Some(config),
        Err(e) => {
            eprintln!("warning: failed to parse hooks.toml: {e}");
            None
        }
    }
}

fn resolve_hook(config: &HooksConfig, event_type: HookEventType) -> Option<&HookDef> {
    match event_type {
        HookEventType::Create => config.on_create.as_ref(),
        HookEventType::StateChange => config.on_state_change.as_ref(),
        HookEventType::Close => config.on_close.as_ref(),
        HookEventType::Comment => config.on_comment.as_ref(),
        HookEventType::PriorityChange => config.on_priority_change.as_ref(),
        HookEventType::LabelChange => config.on_label_change.as_ref(),
        HookEventType::RelationshipChange => config.on_relationship_change.as_ref(),
    }
}

pub fn fire_hook(root: &Path, config: &HooksConfig, event_type: HookEventType, payload_json: &str) {
    if !config.settings.enabled {
        return;
    }

    // Loop prevention
    if let Ok(depth) = std::env::var("STORYHOOK_HOOK_DEPTH")
        && depth.parse::<u32>().unwrap_or(0) >= 1
    {
        return;
    }

    let Some(hook) = resolve_hook(config, event_type) else {
        return;
    };
    let timeout = Duration::from_secs(
        hook.timeout_seconds
            .unwrap_or(config.settings.timeout_seconds),
    );
    let event_name = event_type.as_str();

    let child = Command::new("sh")
        .args(["-c", &hook.command])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .current_dir(root)
        .env("STORYHOOK_HOOK_DEPTH", "1")
        .spawn();

    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            eprintln!("warning: {event_name} hook failed to spawn: {e}");
            return;
        }
    };

    // Write payload to stdin
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload_json.as_bytes());
        // Drop stdin to close pipe
    }

    // Wait with timeout
    match child.wait_timeout(timeout) {
        Ok(Some(status)) => {
            if !status.success() {
                let stderr = read_stderr_limited(&mut child, 4096);
                if stderr.is_empty() {
                    eprintln!("warning: {event_name} hook exited with {status}");
                } else {
                    eprintln!("warning: {event_name} hook failed: {stderr}");
                }
            }
        }
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            eprintln!(
                "warning: {event_name} hook timed out after {}s",
                timeout.as_secs()
            );
        }
        Err(e) => {
            eprintln!("warning: {event_name} hook error: {e}");
        }
    }
}

fn read_stderr_limited(child: &mut std::process::Child, max_bytes: usize) -> String {
    use std::io::Read;
    if let Some(mut stderr) = child.stderr.take() {
        let mut buf = vec![0u8; max_bytes];
        let n = stderr.read(&mut buf).unwrap_or(0);
        String::from_utf8_lossy(&buf[..n]).trim().to_string()
    } else {
        String::new()
    }
}

pub fn build_payload(fields: serde_json::Value) -> String {
    serde_json::to_string(&fields).unwrap_or_default()
}

pub fn list_hooks(root: &Path) -> String {
    match load_hooks_config(root) {
        None => "no hooks configured (no .storyhook/hooks.toml)".to_string(),
        Some(config) => {
            let mut lines = Vec::new();
            let events: [(&str, &Option<HookDef>); 7] = [
                ("on_create", &config.on_create),
                ("on_state_change", &config.on_state_change),
                ("on_close", &config.on_close),
                ("on_comment", &config.on_comment),
                ("on_priority_change", &config.on_priority_change),
                ("on_label_change", &config.on_label_change),
                ("on_relationship_change", &config.on_relationship_change),
            ];
            for (name, hook) in events {
                if let Some(h) = hook {
                    let timeout = h.timeout_seconds.unwrap_or(config.settings.timeout_seconds);
                    lines.push(format!(
                        "{name}: {cmd} (timeout: {timeout}s)",
                        cmd = h.command
                    ));
                }
            }
            if lines.is_empty() {
                "hooks.toml exists but no event hooks are configured".to_string()
            } else {
                if !config.settings.enabled {
                    lines.insert(
                        0,
                        "hooks are DISABLED (settings.enabled = false)".to_string(),
                    );
                }
                lines.join("\n")
            }
        }
    }
}

pub fn test_hook(root: &Path, event_type_str: &str) -> Result<String, crate::error::AppError> {
    let event_type = HookEventType::parse(event_type_str).ok_or_else(|| {
        crate::error::AppError::Validation(format!(
            "unknown event type `{event_type_str}` (valid: create, state_change, close, comment, priority_change, label_change, relationship_change)"
        ))
    })?;

    let config = load_hooks_config(root).ok_or_else(|| {
        crate::error::AppError::NotFound("no .storyhook/hooks.toml found".to_string())
    })?;

    let hook = resolve_hook(&config, event_type).ok_or_else(|| {
        crate::error::AppError::NotFound(format!("no hook configured for {event_type_str}"))
    })?;

    let payload = serde_json::json!({
        "event_type": event_type.as_str(),
        "story_id": "TEST-1",
        "timestamp": crate::storage::now(),
        "story_title": "Test Story",
        "test": true
    });
    let payload_str = serde_json::to_string(&payload).unwrap();

    fire_hook(root, &config, event_type, &payload_str);

    Ok(format!("fired {} hook: {}", event_type_str, hook.command))
}
