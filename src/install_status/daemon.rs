//! PATH diagnostics from the daemon's own process identity, never the caller's.

use super::Row;
use crate::daemon::{agent, lifecycle};
use crate::env::Environment;
use crate::path_identity::resolve_on_path;
use crate::plugin::PluginTarget;
use std::ffi::OsStr;
use std::path::Path;

const PATH_REMEDY: &str = if cfg!(target_os = "macos") {
    "run `story daemon install` from a shell with the required absolute PATH, then `story daemon restart`"
} else {
    "set an absolute PATH in the daemon's service or launch-shell environment, then restart it"
};

pub(super) fn rows(env: &Environment, home: &Path) -> Vec<Row> {
    let (installed_row, installed) = installed(env);
    let mut rows = vec![installed_row];
    match lifecycle::observe_local(env) {
        Ok(Some(info)) => {
            let caller = std::env::var_os("PATH");
            rows.extend(runtime(
                info.execution_path.as_deref(),
                installed.as_ref(),
                caller.as_deref(),
                [
                    registered(home, PluginTarget::ClaudeCode),
                    registered(home, PluginTarget::Codex),
                ],
            ));
        }
        Ok(None) => rows.push(Row::ok(
            "daemon PATH",
            "not running — tool availability unknown",
        )),
        Err(error) => rows.push(Row::flagged("daemon PATH", "unknown", error.to_string())),
    }
    rows
}

fn installed(env: &Environment) -> (Row, Option<agent::ExecutionPath>) {
    let path = agent::path(env);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (Row::ok("agent PATH", "not installed"), None);
        }
        Err(error) => {
            return (
                Row::flagged(
                    "agent PATH",
                    "unknown",
                    format!("cannot read {}: {error}", path.display()),
                ),
                None,
            );
        }
    };
    match agent::registered_path(&text) {
        Ok(Some(path)) => (Row::ok("agent PATH", path.as_str()), Some(path)),
        Ok(None) => (
            Row::flagged(
                "agent PATH",
                "no explicit PATH",
                "legacy agent: run `story daemon install` from a shell whose PATH includes the required tools, then `story daemon restart`",
            ),
            None,
        ),
        Err(error) => (
            Row::flagged(
                "agent PATH",
                "unknown",
                format!(
                    "{}: {error}; run `story daemon install` with a valid PATH",
                    path.display()
                ),
            ),
            None,
        ),
    }
}

fn runtime(
    raw: Option<&str>,
    installed: Option<&agent::ExecutionPath>,
    caller: Option<&OsStr>,
    providers: [bool; 2],
) -> Vec<Row> {
    let Some(raw) = raw else {
        return vec![Row::flagged(
            "daemon PATH",
            "unknown",
            "the daemon did not publish PATH (older build, unset, or non-UTF-8); restart an updated daemon to record it",
        )];
    };
    let path = match agent::ExecutionPath::parse(Some(OsStr::new(raw))) {
        Ok(path) => path,
        Err(error) => {
            return vec![Row::flagged(
                "daemon PATH",
                format!("{raw:?} (invalid)"),
                format!("tool availability unknown: {error}; {PATH_REMEDY}"),
            )];
        }
    };
    let row = if installed.is_some_and(|installed| installed != &path) {
        Row::flagged(
            "daemon PATH",
            raw,
            "the agent PATH differs from the running daemon; `story daemon restart` loads it; to change the saved PATH, run `story daemon install` first",
        )
    } else {
        Row::ok("daemon PATH", raw)
    };
    let mut rows = vec![row];
    for (name, label, capability, required) in [
        (
            "bash",
            "daemon bash",
            "hooks and verification scripts",
            true,
        ),
        ("git", "daemon git", "repository operations", true),
        (
            "gh",
            "daemon gh",
            "pull request submission and verification",
            true,
        ),
        ("tmux", "daemon tmux", "agent sessions", true),
        ("python3", "daemon python3", "verification helpers", true),
        (
            "claude",
            "daemon claude",
            "Claude sessions and plugin management",
            providers[0],
        ),
        (
            "codex",
            "daemon codex",
            "Codex sessions and plugin management",
            providers[1],
        ),
        (
            "cargo",
            "daemon cargo",
            "project gates that use Cargo",
            false,
        ),
        (
            "node",
            "daemon node",
            "Node-based tools and project gates",
            false,
        ),
        ("make", "daemon make", "project gates that use Make", false),
    ] {
        if let Some(tool) = resolve_on_path(Some(OsStr::new(path.as_str())), name) {
            rows.push(Row::ok(label, tool.spelling.display().to_string()));
        } else if required || resolve_on_path(caller, name).is_some() {
            rows.push(Row::flagged(label, "not found", format!("daemon cannot find `{name}` for {capability}; install the tool if needed, then {PATH_REMEDY}")));
        } else {
            rows.push(Row::ok(
                label,
                "not found — optional unless your provider or project needs it",
            ));
        }
    }
    rows
}

fn registered(home: &Path, target: PluginTarget) -> bool {
    // The existing provider row reports unreadable/malformed configuration.
    // Only positive registration evidence makes this tool required here.
    let Ok(text) = std::fs::read_to_string(super::config_path(home, target)) else {
        return false;
    };
    matches!(super::configured_source(&text, target), Ok(Some(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_or_invalid_daemon_path_never_uses_the_callers_path() {
        for raw in [None, Some(""), Some("/usr/bin:"), Some("relative:/bin")] {
            let rows = runtime(raw, None, Some(OsStr::new("/usr/bin:/bin")), [true, true]);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].label, "daemon PATH");
            assert!(rows[0].finding.is_some());
        }
    }

    #[test]
    fn missing_optional_tools_only_flag_registered_or_caller_visible_capabilities() {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let raw = dir.path().to_str().unwrap();
        for providers in [[false, false], [true, false], [false, true]] {
            let rows = runtime(Some(raw), None, None, providers);
            for name in [
                "daemon bash",
                "daemon git",
                "daemon gh",
                "daemon tmux",
                "daemon python3",
            ] {
                assert!(
                    rows.iter()
                        .find(|row| row.label == name)
                        .unwrap()
                        .finding
                        .is_some()
                );
            }
            for (name, required) in [
                ("daemon claude", providers[0]),
                ("daemon codex", providers[1]),
                ("daemon cargo", false),
                ("daemon node", false),
                ("daemon make", false),
            ] {
                assert_eq!(
                    rows.iter()
                        .find(|row| row.label == name)
                        .unwrap()
                        .finding
                        .is_some(),
                    required,
                    "{name}"
                );
            }
        }
    }
}
