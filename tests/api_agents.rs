//! SH-850: `GET /api/repos/{project}/agents`, the census the dashboard gates
//! Resume on, answered by a real daemon from the resume helper's own evidence.
//!
//! A real daemon subprocess (for its minted token, as `tests/dispatch_endpoint.rs`
//! explains) over a real Git checkout: a story claimed and dispatched into a
//! registered worktree, whose private cleanup marker names a tmux server that
//! is gone -- the reboot case -- reads `lost`, with the launch settings its
//! last dispatch recorded. A story claimed by hand has no dispatch evidence and
//! is omitted; an unclaimed one is never asked about.

use std::path::Path;

use storyhook::daemon::lifecycle::{self, DaemonInfo};
use storyhook::service::launch_record::LAUNCH_RECORD_FILE;
use storyhook_test_support::{TestEnv, git, scratch_dir};

/// Stops whatever daemon `env` is running, even if the test panics first.
struct DaemonGuard<'a>(&'a TestEnv);

impl Drop for DaemonGuard<'_> {
    fn drop(&mut self) {
        let _ = lifecycle::stop(&self.0.environment(), lifecycle::StopMode::Force);
    }
}

fn census_url(info: &DaemonInfo, project: &str) -> String {
    format!("http://127.0.0.1:{}/api/repos/{project}/agents", info.port)
}

fn status_of(result: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> u16 {
    match result {
        Ok(response) => response.status().as_u16(),
        Err(ureq::Error::StatusCode(code)) => code,
        Err(other) => panic!("expected an HTTP answer, got: {other}"),
    }
}

/// The private Git directory of the linked worktree at `worktree`.
fn private_git_dir(worktree: &Path) -> std::path::PathBuf {
    let gitfile = std::fs::read_to_string(worktree.join(".git")).unwrap();
    Path::new(gitfile.trim().strip_prefix("gitdir: ").unwrap()).to_path_buf()
}

#[test]
fn the_census_is_token_gated_and_reads_a_lost_agent_with_its_launch_settings() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let project = env.project().with_local_origin().build();
    let lost = project.new_story("its window went with the reboot");
    let manual = project.new_story("claimed by hand, never dispatched");
    let unclaimed = project.new_story("not claimed");
    for id in [&lost, &manual] {
        project
            .story()
            .args(["move", id, "in-progress"])
            .assert()
            .success();
    }
    let slug = {
        let show = project
            .story()
            .args(["project", "show", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice::<serde_json::Value>(&show).unwrap()["project"]["slug"]
            .as_str()
            .unwrap()
            .to_string()
    };

    // The dispatch evidence a reboot leaves: a registered worktree on the
    // story's branch, whose private marker names a tmux server that is gone.
    let repository = project.path().canonicalize().unwrap();
    let branch = format!("worktree-{lost}");
    let worktree = repository.join(".claude/worktrees").join(&lost);
    git(
        &env,
        &repository,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            worktree.to_str().unwrap(),
            "HEAD",
        ],
    );
    let worktree = worktree.canonicalize().unwrap();
    let gone = repository.join("private-tmux/gone.sock");
    let admin = private_git_dir(&worktree);
    std::fs::write(
        admin.join(storyhook::domain::CLEANUP_LEASE_MARKER),
        serde_json::json!({
            "version": 1, "project_slug": slug, "story_id": lost,
            "repository_path": repository, "worktree_path": worktree,
            "branch": branch, "tmux": {"socket_path": gone}
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        admin.join(LAUNCH_RECORD_FILE),
        serde_json::json!({
            "version": 1, "project_slug": slug, "story_id": lost,
            "provider": "claude", "model": "opus", "effort": null, "speed": "fast",
            "autonomy": "auto", "recorded_at": "2026-09-29T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    let dir = scratch_dir();
    env.story(dir.path())
        .args(["daemon", "start"])
        .assert()
        .success();
    let info = env
        .daemon()
        .expect("a started daemon must publish a portfile");
    let url = census_url(&info, &slug);

    assert_eq!(
        status_of(ureq::get(&url).call()),
        401,
        "the census names panes and paths"
    );

    let text = ureq::get(&url)
        .header("X-Storyhook-Token", &info.token)
        .call()
        .expect("an authorized census")
        .into_body()
        .read_to_string()
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["result"], "ok", "{body}");
    let agents = body["agents"].as_array().expect("an agents array");
    assert_eq!(agents.len(), 1, "only the dispatched claimed story: {body}");
    let agent = &agents[0];
    assert_eq!(agent["story"], lost.as_str());
    assert_eq!(agent["state"], "lost", "{agent}");
    assert_eq!(
        agent["provider"], "claude",
        "the .claude container names it"
    );
    assert!(
        agent["detail"]
            .as_str()
            .unwrap()
            .contains("no window named"),
        "{agent}"
    );
    assert_eq!(agent["launch"]["model"], "opus");
    assert_eq!(agent["launch"]["speed"], "fast");
    assert_eq!(agent["launch"]["autonomy"], "auto");
    for absent in [&manual, &unclaimed] {
        assert!(
            !agents.iter().any(|a| a["story"] == absent.as_str()),
            "{absent} has no dispatch evidence or no claim: {body}"
        );
    }

    let post = ureq::post(&url)
        .header("X-Storyhook", "1")
        .header("Host", "127.0.0.1")
        .header("X-Storyhook-Token", &info.token)
        .send_empty();
    assert_eq!(status_of(post), 405, "the census is a read");
    let unknown = ureq::get(census_url(&info, "no-such-project"))
        .header("X-Storyhook-Token", &info.token)
        .call();
    assert_eq!(status_of(unknown), 404);
}
