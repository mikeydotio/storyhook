//! The daemon resolves its own release projection when a provider's plugin
//! registry has lost `story@storyhook` (SH-671).
//!
//! On 2026-09-10 `story plugin install claude` succeeded and, seconds later,
//! Claude Code rewrote `~/.claude/plugins/installed_plugins.json` without the
//! plugin and swept its cache. The dashboard's Claude dispatch then failed with
//! "could not find plugins/story/bin/story.sh for agent `claude`" — while the
//! very tree that install had materialized under the data directory
//! (`<data dir>/plugins/<version>/plugins/story/bin/story.sh`) sat unused.
//! `/story do` never noticed: it launches from its own plugin root.
//!
//! Same harness as `tests/dispatch_options_endpoint.rs` — a real daemon
//! subprocess, a bash stub, no tmux/git/worktree — with one deliberate
//! difference: **no `STORYHOOK_DISPATCH_SCRIPT`**. Every other dispatch suite
//! pins the override, which is exactly why the per-provider branch below it
//! had no coverage. The stub lives where a release projection lives, so the
//! daemon has to find it by the resolution order alone. The negative half
//! (nothing resolves) is pinned at the unit level in `src/api/dispatch.rs`:
//! a test binary built in a checkout always has the checkout's own
//! `plugins/story/bin/story.sh` as the last-resort candidate, so a daemon
//! started from it can never be made to find nothing.

use std::path::{Path, PathBuf};

use storyhook::daemon::lifecycle::{self, DaemonInfo};
use storyhook_test_support::{TestEnv, scratch_dir};

/// A distinctive catalog: proves the projection answered, not the checkout's
/// real `story.sh` (which would list real models) and not a registry copy
/// (there is none under this private HOME).
const PROJECTION_MODEL: &str = "projection-only-model";

fn projection_stub() -> String {
    format!(
        r#"#!/usr/bin/env bash
DISPATCH_PROTOCOL=4
set -u
if [ "$1" = "capabilities" ]; then
  agent="${{2#--agent=}}"
  printf '{{"ok":true,"agent":"%s","models":[{{"id":"{PROJECTION_MODEL}","default":true}}],"efforts":[{{"id":"max"}}],"speeds":[]}}\n' "$agent"
  exit 0
fi
printf '{{"ok":true,"id":"%s","display":"projection dispatched %s"}}\n' "$4" "$4"
"#
    )
}

/// Writes the stub exactly where `story plugin install` materializes this
/// binary's release: `<STORYHOOK_DATA_DIR>/plugins/<CARGO_PKG_VERSION>/plugins/story/bin/story.sh`.
/// The version is this crate's, which is also the daemon binary's.
fn materialize_projection(env: &TestEnv) -> PathBuf {
    let root = env
        .data_dir()
        .join("plugins")
        .join(env!("CARGO_PKG_VERSION"))
        .join("plugins/story/bin");
    std::fs::create_dir_all(&root).expect("mkdir projection bin");
    let script = root.join("story.sh");
    std::fs::write(&script, projection_stub()).expect("write projection stub");
    script
}

struct DaemonGuard<'a>(&'a TestEnv);

impl Drop for DaemonGuard<'_> {
    fn drop(&mut self) {
        let _ = lifecycle::stop(&self.0.environment(), lifecycle::StopMode::Force);
    }
}

/// Starts the daemon with **no** `STORYHOOK_DISPATCH_SCRIPT`, so resolution
/// runs the provider registry (empty under this HOME), then the projection.
fn start_unpinned(env: &TestEnv) -> DaemonInfo {
    let dir = scratch_dir();
    env.story(dir.path())
        .args(["daemon", "start"])
        .env_remove("STORYHOOK_DISPATCH_SCRIPT")
        .assert()
        .success();
    env.daemon()
        .expect("a started daemon must publish a portfile")
}

fn body_json(resp: ureq::http::Response<ureq::Body>) -> serde_json::Value {
    resp.into_body().read_json().expect("a JSON body")
}

fn get_options(info: &DaemonInfo) -> serde_json::Value {
    body_json(
        ureq::get(format!(
            "http://127.0.0.1:{}/api/dispatch-options",
            info.port
        ))
        .header("X-Storyhook-Token", &info.token)
        .call()
        .expect("dispatch-options accepted"),
    )
}

fn post_dispatch(info: &DaemonInfo, agent: &str) -> ureq::http::Response<ureq::Body> {
    ureq::post(format!(
        "http://127.0.0.1:{}/api/repos/proj/story/SH-1/dispatch?agent={agent}",
        info.port
    ))
    .header("X-Storyhook", "1")
    .header("Host", "127.0.0.1")
    .header("X-Storyhook-Token", &info.token)
    .send_empty()
    .expect("dispatch accepted")
}

fn assert_no_claude_registry(home: &Path) {
    assert!(
        !home.join(".claude/plugins/installed_plugins.json").exists(),
        "this suite's premise is a HOME with no Claude plugin registry"
    );
}

/// SH-670's symptom, gone: the Claude catalog is served from the projection.
#[test]
fn dispatch_options_serve_claude_from_the_release_projection_when_no_plugin_is_registered() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    assert_no_claude_registry(env.home());
    materialize_projection(&env);
    let info = start_unpinned(&env);

    let body = get_options(&info);
    assert_eq!(body["claude"]["ok"], true, "{body}");
    assert_eq!(
        body["claude"]["models"][0]["id"], PROJECTION_MODEL,
        "{body}"
    );
    assert_eq!(body["codex"]["ok"], true, "{body}");
    assert_eq!(body["codex"]["models"][0]["id"], PROJECTION_MODEL, "{body}");
}

/// SH-671's symptom, gone: the dispatch is accepted (202) and runs the
/// projection's script, for both providers.
#[test]
fn dispatch_launches_from_the_release_projection_when_no_plugin_is_registered() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    assert_no_claude_registry(env.home());
    materialize_projection(&env);
    let info = start_unpinned(&env);

    for agent in ["claude", "codex"] {
        let resp = post_dispatch(&info, agent);
        assert_eq!(
            resp.status(),
            202,
            "agent {agent} must be accepted, not 500"
        );
        let accepted = body_json(resp);
        assert_eq!(accepted["dispatch"]["state"], "running", "{accepted}");
    }
}
