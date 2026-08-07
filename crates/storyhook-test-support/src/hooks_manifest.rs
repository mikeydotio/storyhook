//! Reads `plugin/claude-code/hooks/hooks.json`, the manifest declaring how
//! long Claude Code lets each of storyhook's session hooks run before
//! killing it.
//!
//! One parser rather than one copy per test file: `tests/session_start_hook.rs`
//! (SH-140) and `tests/hook_budgets.rs` (SH-182) both need the same answer to
//! "what does the manifest declare for this hook", and a second hand-rolled
//! copy is exactly how the two could quietly start disagreeing about which
//! JSON path finds it.

use std::path::PathBuf;

/// The manifest's path, relative to the repository root.
pub const HOOKS_MANIFEST: &str = "plugin/claude-code/hooks/hooks.json";

/// The repository root, regardless of which crate this function was compiled
/// into.
///
/// `CARGO_MANIFEST_DIR` is set at compile time to the directory holding the
/// crate's *own* `Cargo.toml` — the repository root for an integration test
/// in the main crate, but this code lives in `storyhook-test-support`, whose
/// manifest sits two levels under it (`crates/storyhook-test-support/`).
/// Walking up two parents is the fixed cost of that, not a guess: the
/// workspace layout is declared in the repository's own root `Cargo.toml`
/// (`members = ["crates/storyhook-test-support"]`), so this only breaks if
/// that member's own depth changes, and it would break loudly — the
/// `.expect` two calls up the stack, reading a `hooks.json` that would not
/// exist at the wrong root.
fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("storyhook-test-support must be two directories under the workspace root")
        .to_path_buf()
}

/// The timeout Claude Code gives the hook event named `event` running the
/// script whose command line contains `command_substring`, read out of the
/// manifest that ships it.
///
/// **Read rather than restated (SH-140).** A literal here would look like a
/// speed budget somebody chose; it is not — it is `hooks.json`'s own
/// `"timeout"`, and Claude Code kills the hook at it. Reading the manifest
/// keeps the assertion tracking the contract if the declared value ever
/// moves, and fails loudly rather than silently guarding nothing if the
/// manifest stops declaring one.
///
/// The entry is located by its command rather than by position, so a
/// manifest that gains a second hook under the same event cannot quietly
/// hand a test somebody else's budget.
///
/// # Panics
///
/// If the manifest is missing, is not JSON, does not declare `event`, has no
/// entry whose command names `command_substring`, or that entry has no
/// `timeout` — every one of those is a broken contract this function exists
/// to catch, not a case worth degrading gracefully for.
#[must_use]
pub fn declared_timeout(event: &str, command_substring: &str) -> std::time::Duration {
    let raw = std::fs::read_to_string(manifest_dir().join(HOOKS_MANIFEST))
        .expect("the plugin's hooks manifest must be readable");
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).expect("the plugin's hooks manifest must be JSON");

    let seconds = parsed["hooks"][event]
        .as_array()
        .unwrap_or_else(|| panic!("hooks.json must declare {event} matchers"))
        .iter()
        .flat_map(|matcher| {
            matcher["hooks"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
        })
        .find(|hook| {
            hook["command"]
                .as_str()
                .is_some_and(|command| command.contains(command_substring))
        })
        .and_then(|hook| hook["timeout"].as_u64())
        .unwrap_or_else(|| {
            panic!(
                "the {event} entry running a command containing `{command_substring}` must \
                 declare a timeout"
            )
        });

    std::time::Duration::from_secs(seconds)
}

/// The absolute path to a script under `plugin/claude-code/hooks/`.
#[must_use]
pub fn hook_script(name: &str) -> PathBuf {
    manifest_dir().join("plugin/claude-code/hooks").join(name)
}
