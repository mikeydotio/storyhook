//! Every production place that can start a tmux server applies one policy
//! (SH-758).
//!
//! A tmux server keeps the environment of the client that started it as the
//! base environment of every later pane, the user's own terminals included.
//! SH-758 was a second launcher, `scripts/verification-view.py`, that started
//! the default server with the daemon's whole inherited environment while the
//! dispatch launcher already scrubbed its own. Which one ran first decided
//! what every pane on the machine inherited.
//!
//! This scan pins the set of production files whose code can issue a
//! server-starting command (`new-session`, `start-server`) and requires each
//! one to route through `plugins/story/lib/tmux_server_env.py`. A new site
//! cannot appear without somebody deciding what its server retains. Derived
//! over the working tree's non-ignored files, comment lines stripped, in the
//! style of `tests/completion_state_search.rs`.

use std::collections::BTreeSet;
use std::path::Path;

/// Files allowed to start a server, each with the evidence that it applies
/// the shared policy rather than a copy of it.
const SITES: [(&str, &str); 3] = [
    // Its only server-starting call goes through the dispatch launcher.
    (
        "plugins/story/bin/story.sh",
        "lib/tmux-launch.py\" new-session",
    ),
    (
        "plugins/story/lib/tmux-launch.py",
        "environment = client_environment(os.environ)",
    ),
    (
        "scripts/verification-view.py",
        "env = client_environment(os.environ)",
    ),
];

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Production sources: the crate, shipped scripts, and the plugin payload.
fn production_sources(root: &Path) -> Vec<(String, String)> {
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            "src",
            "scripts",
            "plugins/story/bin",
            "plugins/story/lib",
            "plugins/story/hooks",
        ])
        .output()
        .expect("listing this repository's production sources");
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so this scan proved nothing: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|path| std::str::from_utf8(path).expect("a UTF-8 path").to_string())
        .filter(|path| !path.starts_with("scripts/tests/") && !path.contains("__pycache__"))
        .filter_map(|path| {
            std::fs::read_to_string(root.join(&path))
                .ok()
                .map(|text| (path, text))
        })
        .collect()
}

/// The code lines of `text`: comments dropped, and a Rust file's
/// `#[cfg(test)]` tail excluded, because fixtures legitimately own servers.
fn code_lines<'a>(path: &str, text: &'a str) -> Vec<&'a str> {
    let comment = if path.ends_with(".rs") { "//" } else { "#" };
    let mut lines = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if path.ends_with(".rs") && trimmed.starts_with("#[cfg(test)]") {
            break;
        }
        if !trimmed.starts_with(comment) {
            lines.push(line);
        }
    }
    lines
}

fn starts_a_server(line: &str) -> bool {
    line.contains("new-session") || line.contains("start-server")
}

#[test]
fn only_policy_bound_sites_can_start_a_tmux_server() {
    let root = repo_root();
    let sources = production_sources(root);
    assert!(
        sources.iter().any(|(path, _)| path == "src/main.rs"),
        "the scan found no crate sources, so it proved nothing"
    );
    let found: BTreeSet<&str> = sources
        .iter()
        .filter(|(path, text)| code_lines(path, text).into_iter().any(starts_a_server))
        .map(|(path, _)| path.as_str())
        .collect();
    let expected: BTreeSet<&str> = SITES.iter().map(|(path, _)| *path).collect();
    assert_eq!(
        found, expected,
        "a production file can start a tmux server; route it through \
         plugins/story/lib/tmux_server_env.py and add it to SITES"
    );
    for (path, evidence) in SITES {
        let text = &sources
            .iter()
            .find(|(candidate, _)| candidate == path)
            .expect("a pinned site exists")
            .1;
        assert!(
            code_lines(path, text)
                .into_iter()
                .any(|line| line.contains(evidence)),
            "{path} no longer shows that it applies the shared server policy (`{evidence}`)"
        );
        if path == "plugins/story/bin/story.sh" {
            for line in code_lines(path, text)
                .into_iter()
                .filter(|line| starts_a_server(line))
            {
                assert!(
                    line.contains("lib/tmux-launch.py"),
                    "story.sh starts a server without the dispatch launcher: {line}"
                );
            }
        }
    }
}

#[test]
fn the_daemon_runs_the_view_after_the_policy_it_calls() {
    let window = std::fs::read_to_string(repo_root().join("src/daemon/activity/window.rs"))
        .expect("reading the view launcher");
    let policy = window
        .find("include_str!(\"../../../plugins/story/lib/tmux_server_env.py\")")
        .expect("the daemon composes the shared policy into the view program");
    let view = window
        .find("include_str!(\"../../../scripts/verification-view.py\")")
        .expect("the daemon composes the view program");
    assert!(
        policy < view,
        "the policy must precede the view that calls it"
    );
}
