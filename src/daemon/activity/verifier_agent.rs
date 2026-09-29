//! The Verifier Agent pane of each project's verification window (SH-822).
//!
//! The daemon resolves the launch; `scripts/verification-view.py` owns the
//! pane. A missing provider or plugin never blocks the reader: the window
//! then has no agent pane, and the daemon journals why once, on the edge.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::env::Environment;
use crate::path_identity::resolve_on_path;
use crate::plugin::VERIFIER_AGENT;

/// The model the operator chose for the verification window's agent.
const MODEL: &str = "opus";
/// The effort the operator chose for the verification window's agent.
const EFFORT: &str = "xhigh";
/// The agent definition every candidate plugin root must carry. An installed
/// copy older than SH-822 has no such file and is passed over.
const DEFINITION: &str = "agents/verifier.md";

/// The last reason the agent could not be launched, so the journal records
/// each change of that answer once instead of every reconcile pass.
static UNAVAILABLE: Mutex<Option<String>> = Mutex::new(None);

/// The agent's argv for this daemon, `Ok(None)` when the environment turns
/// the pane off, or why it cannot be launched.
pub(super) fn launch(env: &Environment) -> Result<Option<Vec<OsString>>, String> {
    if !env.verifier_agent_enabled() {
        return Ok(None);
    }
    compose(
        std::env::var_os("PATH").as_deref(),
        &plugin_roots(env.home()),
    )
    .map(Some)
}

/// Every plugin root the agent may load from, most specific first: the copy
/// an operator's `STORYHOOK_DISPATCH_SCRIPT` names for every dispatch, this
/// binary's release projection (its own bytes), a development checkout, then
/// Claude Code's installed copy. File reads only: no provider CLI runs on
/// the five-second reconcile tick.
fn plugin_roots(home: &Path) -> Vec<PathBuf> {
    [
        std::env::var_os("STORYHOOK_DISPATCH_SCRIPT")
            .map(PathBuf::from)
            .and_then(|script| Some(script.parent()?.parent()?.to_path_buf())),
        crate::plugin::release_marketplace_root()
            .ok()
            .map(|root| root.join("plugins/story")),
        crate::plugin::dev_repo_root().map(|root| root.join("plugins/story")),
        crate::api::dispatch::installed_claude_plugin_root(home),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Builds the launch from the daemon's `PATH` and the candidate plugin roots.
///
/// `claude` is kept as spelled on `PATH`, never canonicalized: the native
/// installer symlinks a versioned binary that its updater later deletes.
fn compose(path: Option<&OsStr>, roots: &[PathBuf]) -> Result<Vec<OsString>, String> {
    let claude = resolve_on_path(path, "claude")
        .ok_or("`claude` is not on the daemon's PATH")?
        .spelling;
    let root = roots
        .iter()
        .find(|root| root.join(DEFINITION).is_file())
        .ok_or_else(|| {
            format!(
                "no plugin root carries {DEFINITION} (looked in {})",
                roots
                    .iter()
                    .map(|root| root.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
    Ok([
        claude.into_os_string(),
        "--plugin-dir".into(),
        root.clone().into_os_string(),
        "--agent".into(),
        VERIFIER_AGENT.into(),
        "--model".into(),
        MODEL.into(),
        "--effort".into(),
        EFFORT.into(),
    ]
    .into())
}

/// Journals a change in whether the agent can be launched: a WARN when a
/// reason appears or changes, an INFO when the launch is available again.
pub(super) fn note(outcome: &Result<Option<Vec<OsString>>, String>) {
    let reason = outcome.as_ref().err().cloned();
    let mut last = UNAVAILABLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if *last == reason {
        return;
    }
    match &reason {
        Some(reason) => super::emit(
            "WARN",
            "tmux",
            "event",
            "",
            &format!("verification window has no Verifier Agent pane: {reason}"),
        ),
        None => super::emit(
            "INFO",
            "tmux",
            "event",
            "",
            "verification window Verifier Agent pane is available again",
        ),
    }
    *last = reason;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use storyhook_test_support::scratch_dir;

    fn executable(path: &Path) {
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn plugin(root: &Path) -> PathBuf {
        let plugin = root.join("plugins/story");
        std::fs::create_dir_all(plugin.join("agents")).unwrap();
        std::fs::write(plugin.join(DEFINITION), "---\nname: verifier\n---\n").unwrap();
        plugin
    }

    #[test]
    fn the_launch_names_the_agent_model_and_effort_the_operator_chose() {
        let root = scratch_dir();
        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        executable(&bin.join("claude"));
        let plugin = plugin(root.path());
        let argv = compose(Some(bin.as_os_str()), std::slice::from_ref(&plugin)).unwrap();
        assert_eq!(
            argv,
            [
                bin.join("claude").into_os_string(),
                "--plugin-dir".into(),
                plugin.into_os_string(),
                "--agent".into(),
                "story:verifier".into(),
                "--model".into(),
                "opus".into(),
                "--effort".into(),
                "xhigh".into(),
            ]
        );
    }

    #[test]
    fn claude_keeps_its_path_spelling_through_a_symlink() {
        let root = scratch_dir();
        let bin = root.path().join("bin");
        let versions = root.path().join("versions");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&versions).unwrap();
        executable(&versions.join("2.1.284"));
        std::os::unix::fs::symlink(versions.join("2.1.284"), bin.join("claude")).unwrap();
        let argv = compose(Some(bin.as_os_str()), &[plugin(root.path())]).unwrap();
        assert_eq!(argv[0], bin.join("claude").into_os_string());
    }

    #[test]
    fn a_root_without_the_agent_definition_is_passed_over() {
        let root = scratch_dir();
        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        executable(&bin.join("claude"));
        let older = root.path().join("older/plugins/story");
        std::fs::create_dir_all(&older).unwrap();
        let current = plugin(root.path());
        let argv = compose(Some(bin.as_os_str()), &[older, current.clone()]).unwrap();
        assert_eq!(argv[2], current.into_os_string());
    }

    #[test]
    fn a_missing_provider_or_definition_is_named() {
        let root = scratch_dir();
        let empty = root.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        let error = compose(Some(empty.as_os_str()), &[plugin(root.path())]).unwrap_err();
        assert!(
            error.contains("`claude` is not on the daemon's PATH"),
            "{error}"
        );
        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        executable(&bin.join("claude"));
        let error = compose(Some(bin.as_os_str()), std::slice::from_ref(&empty)).unwrap_err();
        assert!(error.contains("agents/verifier.md"), "{error}");
        assert!(error.contains(&empty.display().to_string()), "{error}");
    }

    #[test]
    fn a_disabled_environment_launches_nothing() {
        let root = scratch_dir();
        let env = Environment::at(root.path());
        assert_eq!(launch(&env), Ok(None));
        assert!(env.with_test_verifier_agent().verifier_agent_enabled());
    }
}
