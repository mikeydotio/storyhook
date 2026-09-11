//! Native reset uses the same daemon and store as every ordinary CLI call.

use storyhook_test_support::{Project, TestEnv};

fn json(project: &Project<'_>, args: &[&str]) -> serde_json::Value {
    let output = project.story().args(args).arg("--json").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn reset_always_returns_to_todo_and_can_be_repeated_without_resources() {
    let project = TestEnv::shared().project().git().build();
    let id = project.new_story("Recover an interrupted lesson");
    json(&project, &["move", &id, "blocked"]);
    json(&project, &["claim", &id, "--no-comment"]);
    for _ in 0..2 {
        json(&project, &["reset", &id]);
        assert_eq!(
            json(&project, &["show", &id])["story"]["story"]["state"],
            "todo"
        );
    }
}

#[test]
fn reset_rejects_closed_stories_and_epics_even_with_force() {
    let project = TestEnv::shared().project().git().build();
    let closed = project.new_story("Completed lesson");
    json(&project, &["move", &closed, "done"]);
    let epic = json(&project, &["new", "Course outline", "--type", "epic"])["story"]["story"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    for (id, message) in [(&closed, "closed"), (&epic, "epic")] {
        let output = project
            .story()
            .args(["reset", id, "--force", "--json"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            result["error"].as_str().unwrap().contains(message),
            "{result}"
        );
    }
}

#[test]
fn reset_parser_rejects_extra_arguments() {
    let project = TestEnv::shared().project().build();
    for args in [
        vec!["reset"],
        vec!["reset", "SH-1", "extra"],
        vec!["reset", "SH-1", "--unknown"],
    ] {
        assert!(
            !project
                .story()
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
}

fn git_at(path: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn workspace(project: &Project<'_>, id: &str) -> std::path::PathBuf {
    let root = project.path().canonicalize().unwrap();
    let path = root.join(".codex/worktrees").join(id);
    git_at(
        &root,
        &[
            "worktree",
            "add",
            "-b",
            &format!("reset-{id}"),
            path.to_str().unwrap(),
        ],
    );
    let lease = serde_json::json!({
        "version": 1, "project_slug": project.slug(), "story_id": id,
        "repository_path": root, "worktree_path": path, "branch": format!("reset-{id}"),
        "tmux": {"socket_path": root.join("absent-tmux-socket")}
    });
    let git_dir = git_at(&path, &["rev-parse", "--absolute-git-dir"]);
    std::fs::write(
        std::path::Path::new(&git_dir).join("storyhook-cleanup-lease-v1.json"),
        serde_json::to_vec(&lease).unwrap(),
    )
    .unwrap();
    path
}

#[test]
fn clean_cleanup_preserves_branch_commits_and_story_content() {
    let project = TestEnv::shared().project().git().build();
    let id = project.new_story("Keep lesson history");
    json(&project, &["comment", &id, "Keep this comment"]);
    let path = workspace(&project, &id);
    let commit = git_at(&path, &["rev-parse", "HEAD"]);
    json(&project, &["reset", &id]);
    assert!(!path.exists());
    assert_eq!(
        git_at(project.path(), &["rev-parse", &format!("reset-{id}")]),
        commit
    );
    let after = json(&project, &["show", &id]);
    assert_eq!(after["story"]["story"]["title"], "Keep lesson history");
    assert!(after.to_string().contains("Keep this comment"));
}

#[test]
fn dirty_and_locked_worktrees_require_force_without_changing_state() {
    for kind in ["tracked", "untracked", "locked"] {
        let project = TestEnv::shared().project().git().build();
        let id = project.new_story(kind);
        json(&project, &["claim", &id, "--no-comment"]);
        let path = workspace(&project, &id);
        match kind {
            "tracked" => {
                std::fs::write(path.join("tracked"), "before").unwrap();
                git_at(&path, &["add", "tracked"]);
                git_at(
                    &path,
                    &[
                        "-c",
                        "user.name=Test",
                        "-c",
                        "user.email=test@example.com",
                        "commit",
                        "-m",
                        "fixture",
                    ],
                );
                std::fs::write(path.join("tracked"), "after").unwrap();
            }
            "untracked" => std::fs::write(path.join("unfinished"), "keep").unwrap(),
            _ => {
                git_at(
                    project.path(),
                    &["worktree", "lock", path.to_str().unwrap()],
                );
            }
        }
        assert!(
            !project
                .story()
                .args(["reset", &id])
                .output()
                .unwrap()
                .status
                .success()
        );
        assert!(path.exists());
        assert_eq!(
            json(&project, &["show", &id])["story"]["story"]["state"],
            "in-progress"
        );
        json(&project, &["reset", &id, "--force"]);
        assert!(!path.exists());
    }
}

#[test]
fn unowned_and_callers_worktrees_are_preserved_even_with_force() {
    let project = TestEnv::shared().project().git().build();
    let id = project.new_story("Ownership guard");
    let path = workspace(&project, &id);
    assert!(
        !project
            .story()
            .current_dir(&path)
            .args(["reset", &id, "--force"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(path.exists());
    let git_dir = git_at(&path, &["rev-parse", "--absolute-git-dir"]);
    std::fs::remove_file(std::path::Path::new(&git_dir).join("storyhook-cleanup-lease-v1.json"))
        .unwrap();
    assert!(
        !project
            .story()
            .args(["reset", &id, "--force"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(path.exists());
}

#[test]
fn durable_reservation_survives_restart_blocks_lifecycle_and_requires_explicit_force() {
    use storyhook::store::{ReadOps, Store, StoryNo, WriteOps};
    let project = TestEnv::shared().project().git().build();
    let id = project.new_story("Recover journal");
    let path = workspace(&project, &id);
    let marker = std::path::PathBuf::from(git_at(&path, &["rev-parse", "--absolute-git-dir"]))
        .join("storyhook-cleanup-lease-v1.json");
    let lease: serde_json::Value = serde_json::from_slice(&std::fs::read(marker).unwrap()).unwrap();
    let store = project.env().open_store();
    let project_id = storyhook_test_support::project_id_at(&store, project.path()).unwrap();
    let number = store
        .read(|tx| {
            let prefix = tx.project(project_id)?.unwrap().prefix;
            Ok(StoryNo::parse_id(&prefix, &id).unwrap())
        })
        .unwrap();
    let reservation = serde_json::json!({"operation": "interrupted-reset", "lease": lease, "force": true, "previous_awaiting": null, "detail": "Injected crash after reservation"});
    store
        .write(|tx| tx.put_story_reset(project_id, number, Some(&reservation.to_string())))
        .unwrap();
    drop(store);
    let snapshot = json(&project, &["show", &id]);
    assert_eq!(snapshot["story"]["reset"]["operation"], "interrupted-reset");
    for args in [
        vec!["claim", &id, "--no-comment"],
        vec!["move", &id, "verifying"],
        vec!["delete", &id, "--force"],
    ] {
        assert!(
            !project
                .story()
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    json(&project, &["comment", &id, "Recovery remains diagnosable"]);
    std::fs::write(path.join("new-work"), "must not infer old Force").unwrap();
    assert!(
        !project
            .story()
            .args(["reset", &id])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(path.exists());
    json(&project, &["reset", &id, "--force"]);
    assert!(json(&project, &["show", &id])["story"]["reset"].is_null());
    assert!(!path.exists());
}

#[test]
fn rest_reset_uses_the_same_force_and_state_contract_and_requires_mutation_headers() {
    use storyhook::api::{http::TrustedHosts, rest};
    use storyhook::daemon::http1::{Header, Method};
    let project = TestEnv::shared().project().git().build();
    let id = project.new_story("REST reset");
    json(&project, &["claim", &id, "--no-comment"]);
    let path = workspace(&project, &id);
    std::fs::write(path.join("unfinished"), "keep unless authorized").unwrap();
    let store = project.env().open_store();
    let env = project.env().environment();
    let endpoint = format!("/api/repos/{}/story/{id}/reset", project.slug());
    let headers = [
        ("Host", "127.0.0.1:3456"),
        ("X-Storyhook", "1"),
        ("Content-Type", "application/json"),
    ]
    .map(|(k, v)| Header::from_bytes(k, v).unwrap());
    for (body, expected) in [
        ("{}", 422),
        (r#"{"force":"yes"}"#, 422),
        (r#"{"force":true}"#, 200),
    ] {
        let result = rest::route(
            &store,
            &env,
            &Method::Post,
            &endpoint,
            &headers,
            body,
            &TrustedHosts::default(),
        );
        assert_eq!(
            result.reply.status,
            expected,
            "{}",
            result.reply.text_body().unwrap()
        );
    }
    assert!(!path.exists());
    assert_eq!(
        json(&project, &["show", &id])["story"]["story"]["state"],
        "todo"
    );
    let refused = rest::route(
        &store,
        &env,
        &Method::Post,
        &endpoint,
        &headers[..1],
        "{}",
        &TrustedHosts::default(),
    );
    assert_eq!(refused.reply.status, 403);
}

#[test]
fn active_workspace_owner_excludes_reset_without_mutation() {
    use fs4::FileExt;
    let project = TestEnv::shared().project().git().build();
    let id = project.new_story("Exclusive owner");
    json(&project, &["claim", &id, "--no-comment"]);
    let common = std::path::PathBuf::from(git_at(
        project.path(),
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ));
    let directory = common.join("storyhook/workspace-locks");
    std::fs::create_dir_all(&directory).unwrap();
    let lock = std::fs::File::create(directory.join(format!("{id}.lock"))).unwrap();
    lock.lock_exclusive().unwrap();
    let output = project
        .story()
        .args(["reset", &id, "--force"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        json(&project, &["show", &id])["story"]["story"]["state"],
        "in-progress"
    );
    drop(lock);
    json(&project, &["reset", &id]);
}

struct Tmux(std::path::PathBuf);
impl Tmux {
    fn run(&self, args: &[&str]) -> String {
        let output = std::process::Command::new("tmux")
            .arg("-S")
            .arg(&self.0)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "tmux {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }
}
impl Drop for Tmux {
    fn drop(&mut self) {
        let _ = std::process::Command::new("tmux")
            .arg("-S")
            .arg(&self.0)
            .arg("kill-server")
            .output();
    }
}

#[test]
fn closes_only_owned_tmux_window_and_refuses_callers_window() {
    let project = TestEnv::shared().project().git().build();
    let id = project.new_story("Owned tmux");
    let path = workspace(&project, &id);
    let tmux = Tmux(
        project
            .path()
            .canonicalize()
            .unwrap()
            .join("absent-tmux-socket"),
    );
    tmux.run(&[
        "-f",
        "/dev/null",
        "new-session",
        "-d",
        "-s",
        "reset-test",
        "-n",
        "unrelated",
        "-c",
        project.path().to_str().unwrap(),
        "sleep 600",
    ]);
    let pane = tmux.run(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        "reset-test",
        "-n",
        &id,
        "-c",
        path.to_str().unwrap(),
        "sleep 600",
    ]);
    let alias = project.path().join("socket-alias");
    std::os::unix::fs::symlink(project.path(), &alias).unwrap();
    for socket in [&tmux.0, &alias.join("absent-tmux-socket")] {
        let output = project
            .story()
            .env("TMUX", format!("{},1,0", socket.display()))
            .env("TMUX_PANE", &pane)
            .args(["reset", &id, "--force"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(path.exists());
    }
    json(&project, &["reset", &id]);
    assert_eq!(
        tmux.run(&["list-windows", "-a", "-F", "#{window_name}"]),
        "unrelated"
    );
    assert!(!path.exists());
}

#[test]
fn unrelated_pane_in_matching_window_prevents_any_cleanup() {
    let project = TestEnv::shared().project().git().build();
    let id = project.new_story("Foreign pane");
    let path = workspace(&project, &id);
    let tmux = Tmux(
        project
            .path()
            .canonicalize()
            .unwrap()
            .join("absent-tmux-socket"),
    );
    tmux.run(&[
        "-f",
        "/dev/null",
        "new-session",
        "-d",
        "-s",
        "reset-test",
        "-n",
        &id,
        "-c",
        project.path().to_str().unwrap(),
        "sleep 600",
    ]);
    assert!(
        !project
            .story()
            .args(["reset", &id, "--force"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(path.exists());
    assert_eq!(
        tmux.run(&["list-windows", "-a", "-F", "#{window_name}"]),
        id
    );
}

#[test]
fn last_owned_window_and_dead_pane_are_idempotently_removed() {
    let project = TestEnv::shared().project().git().build();
    let id = project.new_story("Dead pane");
    let path = workspace(&project, &id);
    let tmux = Tmux(
        project
            .path()
            .canonicalize()
            .unwrap()
            .join("absent-tmux-socket"),
    );
    tmux.run(&[
        "-f",
        "/dev/null",
        "new-session",
        "-d",
        "-s",
        "reset-test",
        "-n",
        &id,
        "-c",
        path.to_str().unwrap(),
        "sleep 600",
    ]);
    tmux.run(&[
        "set-option",
        "-w",
        "-t",
        "reset-test",
        "remain-on-exit",
        "on",
    ]);
    tmux.run(&["respawn-pane", "-k", "-t", "reset-test:0", "true"]);
    for _ in 0..100 {
        if tmux.run(&[
            "display-message",
            "-p",
            "-t",
            "reset-test:0",
            "#{pane_dead}",
        ]) == "1"
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        tmux.run(&[
            "display-message",
            "-p",
            "-t",
            "reset-test:0",
            "#{pane_dead}"
        ]),
        "1"
    );
    json(&project, &["reset", &id]);
    assert!(!path.exists());
    json(&project, &["reset", &id]);
}

#[test]
fn git_removal_failure_retains_authority_and_retry_finishes_after_resource_disappears() {
    let project = TestEnv::shared().project().git().build();
    let id = project.new_story("Submodule cleanup recovery");
    let path = workspace(&project, &id);
    git_at(
        &path,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            project.path().to_str().unwrap(),
            "lesson",
        ],
    );
    git_at(
        &path,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-am",
            "fixture submodule",
        ],
    );
    assert!(git_at(&path, &["status", "--porcelain"]).is_empty());
    let output = project
        .story()
        .args(["reset", &id, "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let first = json(&project, &["show", &id]);
    assert!(
        first["story"]["reset"]["detail"]
            .as_str()
            .unwrap()
            .contains("Reset incomplete")
    );
    assert!(path.exists());
    // Model external completion after a crash removed the private marker.
    git_at(
        project.path(),
        &["worktree", "remove", "--force", path.to_str().unwrap()],
    );
    json(&project, &["reset", &id]);
    assert!(json(&project, &["show", &id])["story"]["reset"].is_null());
}

#[test]
fn reset_fires_state_change_hook_after_committing_todo() {
    let project = TestEnv::shared().project().git().build();
    let id = project.new_story("Reset hook");
    json(&project, &["claim", &id, "--no-comment"]);
    let config = project.path().join(".storyhook.toml");
    let contents = std::fs::read_to_string(&config).unwrap();
    std::fs::write(
        config,
        format!("{contents}\n[hooks.on_state_change]\ncommand = \"cat > reset-hook.json\"\n"),
    )
    .unwrap();
    json(&project, &["reset", &id]);
    let result: serde_json::Value =
        serde_json::from_slice(&std::fs::read(project.path().join("reset-hook.json")).unwrap())
            .unwrap();
    assert_eq!(result["from_state"], "in-progress");
    assert_eq!(result["to_state"], "todo");
}
