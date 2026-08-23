//! Drift pins across the shared Storyhook plugin package, its provider
//! manifests, and the daemon that dispatches through it. Hermetic: no
//! fixture, store, daemon, or provider CLI.
//!
//! # What these guard against
//!
//! **The protocol pin.** `src/api/dispatch.rs::resolve_dispatch_script_from`
//! refuses any `story.sh` whose declared `DISPATCH_PROTOCOL` is older than
//! `REQUIRED_DISPATCH_PROTOCOL`. That check is only as good as the two
//! numbers actually agreeing about what "the current contract" is — a typo
//! or a forgotten bump on either side would either refuse a script that is
//! actually fine, or (worse) accept one that is not. This test fails loudly
//! if they ever disagree, rather than leaving that only discoverable by
//! running a stale plugin against a mismatched daemon.
//!
//! **The package pins.** Claude and Codex each discover the same canonical
//! `plugins/story` payload through provider-specific manifests and
//! marketplaces. These tests keep names, source paths, capabilities, and
//! versions synchronized without coupling either provider to the other's
//! unsupported declarations.

use std::path::Path;

use storyhook::api::dispatch::{REQUIRED_DISPATCH_PROTOCOL, declared_dispatch_protocol};

const PLUGIN_ROOT: &str = "plugins/story";
const PLUGIN_NAME: &str = "story";

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read_json(path: &Path) -> serde_json::Value {
    let raw =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parsing {} as JSON: {e}", path.display()))
}

fn story_entry<'a>(marketplace: &'a serde_json::Value, path: &Path) -> &'a serde_json::Value {
    marketplace["plugins"]
        .as_array()
        .unwrap_or_else(|| panic!("{} has no \"plugins\" array", path.display()))
        .iter()
        .find(|entry| entry["name"] == PLUGIN_NAME)
        .unwrap_or_else(|| {
            panic!(
                "{} has no plugin entry named \"{PLUGIN_NAME}\"",
                path.display()
            )
        })
}

#[test]
fn story_sh_declares_the_protocol_the_daemon_requires() {
    let script = repo_root().join(PLUGIN_ROOT).join("bin/story.sh");
    assert!(
        script.is_file(),
        "expected to find story.sh at {}",
        script.display()
    );
    let declared = declared_dispatch_protocol(&script);
    assert_eq!(
        declared, REQUIRED_DISPATCH_PROTOCOL,
        "plugins/story/bin/story.sh declares DISPATCH_PROTOCOL={declared}, but \
         src/api/dispatch.rs::REQUIRED_DISPATCH_PROTOCOL is {REQUIRED_DISPATCH_PROTOCOL} -- \
         bump whichever one is behind so the daemon's own check means what it says"
    );
}

/// Vacuity guard for the test above: confirms `declared_dispatch_protocol`
/// actually reads *this repo's* story.sh and not, say, an empty path that
/// would coincidentally return 0 and pass by construction if
/// `REQUIRED_DISPATCH_PROTOCOL` were ever (wrongly) also 0.
#[test]
fn story_sh_actually_declares_a_marker_at_all() {
    let script = repo_root().join(PLUGIN_ROOT).join("bin/story.sh");
    assert!(
        declared_dispatch_protocol(&script) > 0,
        "story.sh must declare a real DISPATCH_PROTOCOL, not merely fail to parse one"
    );
}

/// Reads a JSON file's `version` field as a string, failing loudly (naming
/// the path) rather than silently treating a missing/malformed field as an
/// empty string that could coincidentally equal the other side.
fn json_version(path: &Path) -> String {
    let value = read_json(path);
    value["version"]
        .as_str()
        .unwrap_or_else(|| panic!("{} has no string \"version\" field", path.display()))
        .to_string()
}

#[test]
fn provider_manifests_and_claude_marketplace_agree_on_version() {
    let claude_manifest = repo_root()
        .join(PLUGIN_ROOT)
        .join(".claude-plugin/plugin.json");
    let codex_manifest = repo_root()
        .join(PLUGIN_ROOT)
        .join(".codex-plugin/plugin.json");
    let claude_version = json_version(&claude_manifest);
    let codex_version = json_version(&codex_manifest);

    let marketplace_json = repo_root().join(".claude-plugin/marketplace.json");
    let marketplace = read_json(&marketplace_json);
    let marketplace_version = story_entry(&marketplace, &marketplace_json)["version"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "{}'s story entry has no string \"version\" field",
                marketplace_json.display()
            )
        })
        .to_string();

    assert_eq!(claude_version, codex_version, "provider manifests drifted");
    assert_eq!(
        claude_version, marketplace_version,
        "Claude manifest and marketplace versions drifted"
    );
}

#[test]
fn provider_manifest_names_match_the_shared_plugin_folder() {
    let plugin_root = repo_root().join(PLUGIN_ROOT);
    let folder_name = plugin_root
        .file_name()
        .and_then(|name| name.to_str())
        .expect("plugin root must have a UTF-8 folder name");
    assert_eq!(folder_name, PLUGIN_NAME);

    for relative in [".claude-plugin/plugin.json", ".codex-plugin/plugin.json"] {
        let path = plugin_root.join(relative);
        let manifest = read_json(&path);
        assert_eq!(
            manifest["name"].as_str(),
            Some(folder_name),
            "{} name must match its plugin folder",
            path.display()
        );
    }
}

#[test]
fn provider_marketplaces_resolve_the_shared_plugin_root() {
    let claude_path = repo_root().join(".claude-plugin/marketplace.json");
    let claude = read_json(&claude_path);
    assert_eq!(
        story_entry(&claude, &claude_path)["source"],
        PLUGIN_ROOT_PREFIXED
    );

    let codex_path = repo_root().join(".agents/plugins/marketplace.json");
    let codex = read_json(&codex_path);
    assert_eq!(codex["name"], "storyhook");
    let codex_story = story_entry(&codex, &codex_path);
    assert_eq!(codex_story["source"]["source"], "local");
    assert_eq!(codex_story["source"]["path"], PLUGIN_ROOT_PREFIXED);
}

const PLUGIN_ROOT_PREFIXED: &str = "./plugins/story";

#[test]
fn codex_manifest_is_skills_only() {
    let path = repo_root()
        .join(PLUGIN_ROOT)
        .join(".codex-plugin/plugin.json");
    let manifest = read_json(&path);
    assert_eq!(manifest["skills"], "./skills/");
    for unsupported in ["apps", "mcpServers", "hooks"] {
        assert!(
            manifest.get(unsupported).is_none(),
            "{} must omit unsupported Phase 1 field {unsupported}",
            path.display()
        );
    }
}
