//! Two drift pins between the Claude Code plugin and the daemon that
//! dispatches through it (SH-196). Hermetic: no fixture, no store, no
//! daemon — reads three files directly, in the shape of
//! `tests/readme_command_reference.rs`.
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
//! **The manifest pin.** `plugin/claude-code/.claude-plugin/plugin.json`
//! and `.claude-plugin/marketplace.json`'s `story` entry each carry their
//! own `version` field, and nothing enforces they agree. They drifted for
//! real once (`3cbd08a` bumped `plugin.json` to `0.4.0` and forgot
//! `marketplace.json`, which stayed at `0.3.0`) — the exact reason `claude
//! plugin update story@storyhook` kept answering "already at the latest
//! version" during SH-196's own investigation, discovered only by hand
//! (SH-196's second comment) rather than by a test.

use std::path::Path;

use storyhook::api::dispatch::{REQUIRED_DISPATCH_PROTOCOL, declared_dispatch_protocol};

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn story_sh_declares_the_protocol_the_daemon_requires() {
    let script = repo_root().join("plugin/claude-code/bin/story.sh");
    assert!(
        script.is_file(),
        "expected to find story.sh at {}",
        script.display()
    );
    let declared = declared_dispatch_protocol(&script);
    assert_eq!(
        declared, REQUIRED_DISPATCH_PROTOCOL,
        "plugin/claude-code/bin/story.sh declares DISPATCH_PROTOCOL={declared}, but \
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
    let script = repo_root().join("plugin/claude-code/bin/story.sh");
    assert!(
        declared_dispatch_protocol(&script) > 0,
        "story.sh must declare a real DISPATCH_PROTOCOL, not merely fail to parse one"
    );
}

/// Reads a JSON file's `version` field as a string, failing loudly (naming
/// the path) rather than silently treating a missing/malformed field as an
/// empty string that could coincidentally equal the other side.
fn json_version(path: &Path) -> String {
    let raw =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let value: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("parsing {} as JSON: {e}", path.display()));
    value["version"]
        .as_str()
        .unwrap_or_else(|| panic!("{} has no string \"version\" field", path.display()))
        .to_string()
}

#[test]
fn the_plugin_manifest_and_marketplace_entry_agree_on_version() {
    let plugin_json = repo_root().join("plugin/claude-code/.claude-plugin/plugin.json");
    let plugin_version = json_version(&plugin_json);

    let marketplace_json = repo_root().join(".claude-plugin/marketplace.json");
    let raw = std::fs::read_to_string(&marketplace_json)
        .unwrap_or_else(|e| panic!("reading {}: {e}", marketplace_json.display()));
    let manifest: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("parsing {} as JSON: {e}", marketplace_json.display()));
    let plugins = manifest["plugins"]
        .as_array()
        .unwrap_or_else(|| panic!("{} has no \"plugins\" array", marketplace_json.display()));
    let story_entry = plugins
        .iter()
        .find(|entry| entry["name"] == "story")
        .unwrap_or_else(|| {
            panic!(
                "{} has no plugin entry named \"story\"",
                marketplace_json.display()
            )
        });
    let marketplace_version = story_entry["version"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "{}'s story entry has no string \"version\" field",
                marketplace_json.display()
            )
        })
        .to_string();

    assert_eq!(
        plugin_version,
        marketplace_version,
        "{} declares version {plugin_version} but {}'s story entry declares \
         {marketplace_version} -- `claude plugin update` compares against the marketplace \
         entry, so a forgotten bump on either side makes it silently report \"already at the \
         latest version\" (SH-196)",
        plugin_json.display(),
        marketplace_json.display()
    );
}
