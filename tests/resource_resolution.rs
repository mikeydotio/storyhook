//! SH-709: the native resource reader observes real Git identity, never an LLM.
use storyhook_test_support::{TestEnv, git};

fn report(project: &storyhook_test_support::Project<'_>, args: &[&str]) -> serde_json::Value {
    let output = project
        .story()
        .env("TMUX_TMPDIR", project.path().join("private-tmux"))
        .arg("resources")
        .args(args)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice::<serde_json::Value>(&output).unwrap()["resources"].clone()
}

#[test]
fn absence_branch_only_and_custom_name_preserve_identity() {
    let env = TestEnv::isolated();
    let project = env.project().with_local_origin().build();
    let id = project.new_story("resource states");
    let absent = report(&project, &[&id, "--window-name", "custom"]);
    assert_eq!(absent["status"], "absent");
    assert_eq!(absent["branch"], "worktree-custom");
    git(&env, project.path(), &["branch", &format!("worktree-{id}")]);
    let branch = report(&project, &[&id]);
    assert_eq!(branch["status"], "resolved");
    assert!(branch["worktree"].is_null());
}

#[test]
fn stale_registration_unregistered_directory_and_dangling_link_are_not_absence() {
    let env = TestEnv::isolated();
    let project = env.project().with_local_origin().build();
    let id = project.new_story("stale resource");
    let branch = format!("worktree-{id}");
    let path = project.path().join(".codex/worktrees").join(&id);
    git(
        &env,
        project.path(),
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            path.to_str().unwrap(),
            "HEAD",
        ],
    );
    std::fs::rename(&path, project.path().join("moved-without-git")).unwrap();
    assert_eq!(report(&project, &[&id])["status"], "invalid");
    let other = project.new_story("unregistered resource");
    let unregistered = project.path().join(".claude/worktrees").join(&other);
    std::fs::create_dir_all(&unregistered).unwrap();
    assert_eq!(report(&project, &[&other])["status"], "invalid");
    let dangling = project.new_story("dangling resource");
    std::os::unix::fs::symlink(
        project.path().join("missing"),
        project.path().join(".claude/worktrees").join(&dangling),
    )
    .unwrap();
    project
        .story()
        .args(["resources", &dangling, "--json"])
        .assert()
        .failure();
}

#[test]
fn broken_unrelated_registration_does_not_hide_an_independent_story() {
    let env = TestEnv::isolated();
    let project = env.project().with_local_origin().build();
    let affected = project.new_story("damaged worktree");
    let independent = project.new_story("independent work");
    let outside = tempfile::tempdir_in("/tmp").unwrap();
    let path = outside
        .path()
        .canonicalize()
        .unwrap()
        .join("custom agent workspace");
    let branch = format!("worktree-{affected}");
    git(
        &env,
        project.path(),
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            path.to_str().unwrap(),
            "HEAD",
        ],
    );
    let marker = lease(
        &project,
        &affected,
        &path,
        &branch,
        &project.path().join("unused.sock"),
    );
    let marker_path = write_marker(&path, &marker);
    std::fs::write(path.join("uncommitted.txt"), "preserve me\n").unwrap();
    git(
        &env,
        project.path(),
        &["worktree", "lock", path.to_str().unwrap()],
    );
    std::fs::remove_file(path.join(".git")).unwrap();

    let independent_report = report(&project, &[&independent]);
    assert_eq!(
        independent_report["status"], "absent",
        "{independent_report}"
    );
    assert_eq!(
        independent_report["observations"][0]["path"],
        path.to_str().unwrap()
    );
    assert_eq!(independent_report["observations"][0]["status"], "stale");
    assert!(
        independent_report["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let affected_report = report(&project, &[&affected]);
    assert_eq!(affected_report["status"], "invalid", "{affected_report}");
    assert_eq!(affected_report["candidates"][0]["locked"], true);
    assert!(affected_report["diagnostics"].to_string().contains("stale"));
    assert!(marker_path.is_file());
    assert_eq!(
        std::fs::read_to_string(path.join("uncommitted.txt")).unwrap(),
        "preserve me\n"
    );
    assert!(path.is_dir());
    assert!(storyhook::service::resources::git::branch_exists(project.path(), &branch).unwrap());
}

#[test]
fn surviving_private_marker_claims_a_custom_path_without_a_worktree_git_link() {
    let env = TestEnv::isolated();
    let project = env.project().with_local_origin().build();
    let target = project.new_story("private claim");
    let outside = tempfile::tempdir_in("/tmp").unwrap();
    let path = outside
        .path()
        .canonicalize()
        .unwrap()
        .join("unrelated-looking-custom-path");
    let branch = "worktree-legacy-name";
    git(
        &env,
        project.path(),
        &[
            "worktree",
            "add",
            "-b",
            branch,
            path.to_str().unwrap(),
            "HEAD",
        ],
    );
    let marker = lease(
        &project,
        &target,
        &path,
        branch,
        &project.path().join("unused.sock"),
    );
    write_marker(&path, &marker);
    std::fs::remove_file(path.join(".git")).unwrap();

    let found = report(&project, &[&target]);
    assert_eq!(found["status"], "invalid", "{found}");
    assert_eq!(found["candidates"][0]["worktree"], path.to_str().unwrap());
    assert!(found["diagnostics"].to_string().contains("stale"));
}

#[test]
fn malformed_claimed_path_is_a_registration_failure_not_a_repository_failure() {
    let env = TestEnv::isolated();
    let project = env.project().with_local_origin().build();
    let affected = project.new_story("bad private path");
    let independent = project.new_story("independent target");
    let outside = tempfile::tempdir_in("/tmp").unwrap();
    let root = outside.path().canonicalize().unwrap();
    let path = root.join("custom");
    let branch = format!("worktree-{affected}");
    git(
        &env,
        project.path(),
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            path.to_str().unwrap(),
            "HEAD",
        ],
    );
    let mut marker: serde_json::Value = serde_json::from_str(&lease(
        &project,
        &affected,
        &path,
        &branch,
        &project.path().join("unused.sock"),
    ))
    .unwrap();
    let alias = root.join("dangling-repository");
    std::os::unix::fs::symlink(root.join("missing"), &alias).unwrap();
    marker["repository_path"] = alias.to_str().unwrap().into();
    write_marker(&path, &marker.to_string());

    let found = report(&project, &[&independent]);
    assert_eq!(found["status"], "absent", "{found}");
    assert_eq!(found["observations"][0]["status"], "invalid");
    assert_eq!(found["observations"][0]["owner_story_id"], affected);
    assert_eq!(report(&project, &[&affected])["status"], "invalid");
}

#[test]
fn multiple_damaged_registrations_preserve_independent_absence() {
    let env = TestEnv::isolated();
    let project = env.project().with_local_origin().build();
    let independent = project.new_story("independent dispatch");
    let outside = tempfile::tempdir_in("/tmp").unwrap();
    let root = outside.path().canonicalize().unwrap();
    let mut affected = Vec::new();
    for index in 0..3 {
        let id = project.new_story(&format!("damaged {index}"));
        let branch = format!("worktree-{id}");
        let path = root.join(format!("workspace-{index}"));
        git(
            &env,
            project.path(),
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                path.to_str().unwrap(),
                "HEAD",
            ],
        );
        if index == 0 {
            let marker = lease(
                &project,
                &id,
                &path,
                &branch,
                &project.path().join("unused.sock"),
            );
            write_marker(&path, &marker);
            std::fs::remove_file(path.join(".git")).unwrap();
        } else if index == 1 {
            write_marker(&path, "{malformed");
            std::fs::remove_file(path.join(".git")).unwrap();
        } else {
            std::fs::remove_dir_all(&path).unwrap();
        }
        affected.push((id, path));
    }

    let found = report(&project, &[&independent]);
    assert_eq!(found["status"], "absent", "{found}");
    assert_eq!(found["observations"].as_array().unwrap().len(), 3);
    assert!(found["diagnostics"].as_array().unwrap().is_empty());
    for (id, path) in affected {
        let own = report(&project, &[&id]);
        assert_eq!(own["status"], "invalid", "{own}");
        assert!(
            own["diagnostics"].to_string().contains("registration")
                || own["diagnostics"].to_string().contains("marker")
        );
        assert!(
            storyhook::service::resources::git::branch_exists(
                project.path(),
                &format!("worktree-{id}")
            )
            .unwrap()
        );
        assert!(
            storyhook::service::resources::git::inventory(project.path())
                .unwrap()
                .iter()
                .any(|record| record.path == path)
        );
    }
}

#[test]
fn duplicate_private_backlinks_refuse_shared_identity_selection() {
    let env = TestEnv::isolated();
    let project = env.project().with_local_origin().build();
    let independent = project.new_story("identity requires one administration");
    let outside = tempfile::tempdir_in("/tmp").unwrap();
    let path = outside.path().canonicalize().unwrap().join("linked");
    git(
        &env,
        project.path(),
        &[
            "worktree",
            "add",
            "-b",
            "worktree-other",
            path.to_str().unwrap(),
            "HEAD",
        ],
    );
    let common = storyhook::service::resources::git::text(
        project.path(),
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .unwrap();
    let duplicate = std::path::Path::new(common.trim()).join("worktrees/duplicate-link");
    std::fs::create_dir_all(&duplicate).unwrap();
    std::fs::write(
        duplicate.join("gitdir"),
        format!("{}/.git\n", path.display()),
    )
    .unwrap();
    std::fs::write(duplicate.join("HEAD"), "ref: refs/heads/worktree-other\n").unwrap();

    let found = report(&project, &[&independent]);
    assert_eq!(found["status"], "unavailable", "{found}");
    assert!(
        found["diagnostics"]
            .to_string()
            .contains("multiple private Git administrations")
    );
    assert!(path.join(".git").is_file());
}

#[test]
fn failed_full_git_inventory_never_reports_absence() {
    let env = TestEnv::isolated();
    let project = env.project().with_local_origin().build();
    let id = project.new_story("inventory unavailable");
    std::fs::rename(
        project.path().join(".git"),
        project.path().join(".git-held"),
    )
    .unwrap();

    let output = project
        .story()
        .args(["resources", &id, "--json"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let response: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert!(
        response["error"].to_string().contains("git worktree list"),
        "{response}"
    );
    assert!(project.path().join(".git-held").is_dir());
}

#[test]
fn unrelated_branch_and_main_checkout_never_become_disposable() {
    let env = TestEnv::isolated();
    let project = env.project().with_local_origin().build();
    let id = project.new_story("wrong branch");
    let path = project.path().join(".claude/worktrees").join(&id);
    git(
        &env,
        project.path(),
        &[
            "worktree",
            "add",
            "-b",
            "unrelated",
            path.to_str().unwrap(),
            "HEAD",
        ],
    );
    assert_eq!(report(&project, &[&id])["status"], "invalid");
    let main = project.new_story("main checkout");
    git(
        &env,
        project.path(),
        &["checkout", "-b", &format!("worktree-{main}")],
    );
    assert_eq!(report(&project, &[&main])["status"], "invalid");
}

#[test]
fn configured_path_is_additional_evidence_and_does_not_hide_registered_resources() {
    let env = TestEnv::isolated();
    let project = env.project().with_local_origin().build();
    let id = project.new_story("custom path");
    let path = project.path().join("outside-defaults").join(&id);
    git(
        &env,
        project.path(),
        &[
            "worktree",
            "add",
            "-b",
            &format!("worktree-{id}"),
            path.to_str().unwrap(),
            "HEAD",
        ],
    );
    let query = report(&project, &[&id, "--worktree-root", "different-container"]);
    assert_eq!(query["status"], "resolved");
    assert_eq!(query["worktree"], path.to_str().unwrap());
    assert!(query["provider"].is_null());
}

#[test]
fn resource_reader_discovers_custom_registered_worktrees_without_provider_hints() {
    let env = TestEnv::isolated();
    let project = env.project().with_local_origin().build();
    let id = project.new_story("custom resource");
    let branch = format!("worktree-{id}");
    let path = project.path().join("custom space\nwith newline");
    git(
        &env,
        project.path(),
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            path.to_str().unwrap(),
            "HEAD",
        ],
    );
    for caller in ["claude", "codex", "unknown"] {
        let output = project
            .story()
            .env("TMUX_TMPDIR", project.path().join("private-tmux"))
            .env("STORY_AGENT", caller)
            .args(["resources", &id, "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(json["resources"]["status"], "resolved");
        assert_eq!(json["resources"]["worktree"], path.to_str().unwrap());
        assert_eq!(json["resources"]["branch"], branch);
    }
}

#[test]
fn resource_reader_reports_conflicting_legacy_paths_without_mutation() {
    let env = TestEnv::isolated();
    let project = env.project().with_local_origin().build();
    let id = project.new_story("conflicting resource");
    let branch = format!("worktree-{id}");
    let first = project.path().join(".claude/worktrees").join(&id);
    let second = project.path().join(".codex/worktrees").join(&id);
    git(
        &env,
        project.path(),
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            first.to_str().unwrap(),
            "HEAD",
        ],
    );
    git(
        &env,
        project.path(),
        &[
            "worktree",
            "add",
            "--force",
            second.to_str().unwrap(),
            &branch,
        ],
    );
    let output = project
        .story()
        .env("TMUX_TMPDIR", project.path().join("private-tmux"))
        .args(["resources", &id, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(report["resources"]["status"], "ambiguous");
    assert_eq!(
        report["resources"]["candidates"].as_array().unwrap().len(),
        2
    );
    assert!(first.exists() && second.exists());
}

struct PrivateTmux {
    root: tempfile::TempDir,
}
impl PrivateTmux {
    fn new() -> Self {
        Self {
            root: tempfile::Builder::new()
                .prefix("story-resource-tmux-")
                .tempdir_in("/tmp")
                .unwrap(),
        }
    }
    fn socket(&self) -> std::path::PathBuf {
        self.root.path().canonicalize().unwrap().join("server")
    }
    fn run(&self, args: &[&str]) -> String {
        let out = std::process::Command::new("tmux")
            .args(["-f", "/dev/null", "-S"])
            .arg(self.socket())
            .args(args)
            .env_remove("TMUX")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "tmux {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim_end().into()
    }
}
impl Drop for PrivateTmux {
    fn drop(&mut self) {
        if self.socket().exists() {
            let out = std::process::Command::new("tmux")
                .arg("-S")
                .arg(self.socket())
                .arg("kill-server")
                .output()
                .unwrap();
            assert!(out.status.success(), "owned tmux server cleanup failed");
        }
    }
}

fn lease(
    project: &storyhook_test_support::Project<'_>,
    id: &str,
    path: &std::path::Path,
    branch: &str,
    socket: &std::path::Path,
) -> String {
    let show = project
        .story()
        .args(["project", "show", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let show: serde_json::Value = serde_json::from_slice(&show).unwrap();
    serde_json::json!({"version":1,"project_slug":show["project"]["slug"],"story_id":id,"repository_path":project.path(),"worktree_path":path,"branch":branch,"tmux":{"socket_path":socket}}).to_string()
}

#[test]
fn lease_socket_wins_over_caller_and_duplicate_names_refuse() {
    let env = TestEnv::isolated();
    let project = env.project().with_local_origin().build();
    let id = project.new_story("socket-bound resources");
    let path = project.path().join("custom");
    let branch = format!("worktree-{id}");
    git(
        &env,
        project.path(),
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            path.to_str().unwrap(),
            "HEAD",
        ],
    );
    let owner = PrivateTmux::new();
    let caller = PrivateTmux::new();
    let pane = owner.run(&[
        "new-session",
        "-d",
        "-s",
        "owned",
        "-n",
        &id,
        "-c",
        path.to_str().unwrap(),
        "-P",
        "-F",
        "#{pane_id}",
        "sleep 120",
    ]);
    owner.run(&[
        "set-window-option",
        "-t",
        &pane,
        "@storyhook-agent",
        "codex",
    ]);
    caller.run(&[
        "new-session",
        "-d",
        "-s",
        "caller",
        "-n",
        &id,
        "-c",
        project.path().to_str().unwrap(),
        "sleep 120",
    ]);
    let lease = lease(&project, &id, &path, &branch, &owner.socket());
    let found = report(
        &project,
        &[
            &id,
            "--lease-json",
            &lease,
            "--tmux-socket",
            caller.socket().to_str().unwrap(),
        ],
    );
    assert_eq!(found["status"], "resolved", "{found}");
    assert_eq!(found["socket_path"], owner.socket().to_str().unwrap());
    assert_eq!(found["provider"], "codex");
    owner.run(&[
        "new-window",
        "-d",
        "-t",
        "owned:",
        "-n",
        &id,
        "-c",
        path.to_str().unwrap(),
        "sleep 120",
    ]);
    let duplicate = report(&project, &[&id, "--lease-json", &lease]);
    assert_eq!(duplicate["status"], "ambiguous", "{duplicate}");
    assert!(path.exists());
}

#[test]
fn historical_custom_window_survives_git_removal_and_missing_socket_keeps_identity() {
    let env = TestEnv::isolated();
    let project = env.project().with_local_origin().build();
    let id = project.new_story("custom terminal history");
    let path = project.path().join("custom");
    let branch = "worktree-custom-window";
    git(
        &env,
        project.path(),
        &[
            "worktree",
            "add",
            "-b",
            branch,
            path.to_str().unwrap(),
            "HEAD",
        ],
    );
    let server = PrivateTmux::new();
    server.run(&[
        "new-session",
        "-d",
        "-s",
        "owned",
        "-n",
        "custom-window",
        "-c",
        path.to_str().unwrap(),
        "sleep 120",
    ]);
    let lease = lease(&project, &id, &path, branch, &server.socket());
    let gitdir = std::fs::read_to_string(path.join(".git")).unwrap();
    let gitdir = std::path::Path::new(gitdir.trim().strip_prefix("gitdir: ").unwrap());
    std::fs::write(gitdir.join(storyhook::domain::CLEANUP_LEASE_MARKER), &lease).unwrap();
    // Persist the real private marker through the production submission capture.
    env.story(&path)
        .args(["move", &id, "in-progress"])
        .assert()
        .success();
    env.story(&path)
        .args(["move", &id, "verifying"])
        .assert()
        .success();
    git(
        &env,
        project.path(),
        &["worktree", "remove", path.to_str().unwrap()],
    );
    git(&env, project.path(), &["branch", "-D", branch]);
    let found = report(&project, &[&id]);
    assert_eq!(found["status"], "resolved", "{found}");
    assert_eq!(found["pane"]["window_name"], "custom-window");
    let absent_socket = server.socket().with_file_name("never-created");
    let absent = report(
        &project,
        &[
            &project.new_story("absent socket"),
            "--tmux-socket",
            absent_socket.to_str().unwrap(),
        ],
    );
    assert_eq!(absent["status"], "absent");
    assert_eq!(absent["socket_path"], absent_socket.to_str().unwrap());
    std::fs::write(&absent_socket, "not a server").unwrap();
    let unavailable = report(
        &project,
        &[
            &project.new_story("unavailable socket"),
            "--tmux-socket",
            absent_socket.to_str().unwrap(),
        ],
    );
    assert_eq!(unavailable["status"], "unavailable");
}

fn write_marker(worktree: &std::path::Path, value: &str) -> std::path::PathBuf {
    let gitdir = std::fs::read_to_string(worktree.join(".git")).unwrap();
    let path = std::path::Path::new(gitdir.trim().strip_prefix("gitdir: ").unwrap())
        .join(storyhook::domain::CLEANUP_LEASE_MARKER);
    std::fs::write(&path, value).unwrap();
    path
}

#[test]
fn invalid_or_foreign_private_markers_never_authorize_a_target() {
    let env = TestEnv::isolated();
    let project = env.project().with_local_origin().build();
    let id = project.new_story("marker validation");
    let path = project.path().join(".codex/worktrees").join(&id);
    let branch = format!("worktree-{id}");
    git(
        &env,
        project.path(),
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            path.to_str().unwrap(),
            "HEAD",
        ],
    );
    let socket = project.path().join("missing.sock");
    write_marker(&path, "{bad json");
    assert_eq!(report(&project, &[&id])["status"], "invalid");
    let valid = lease(&project, &id, &path, &branch, &socket);
    write_marker(&path, &valid);
    assert_eq!(report(&project, &[&id])["status"], "resolved");
    let mut wrong_branch: serde_json::Value = serde_json::from_str(&valid).unwrap();
    wrong_branch["branch"] = "foreign-branch".into();
    write_marker(&path, &wrong_branch.to_string());
    let mismatch = report(&project, &[&id]);
    assert_eq!(mismatch["status"], "invalid");
    assert_eq!(mismatch["observations"][0]["status"], "invalid");
    let diagnostics = mismatch["diagnostics"].to_string();
    assert!(diagnostics.contains("cleanup lease marker"));
    assert!(diagnostics.contains("branch mismatch"));
    assert!(diagnostics.contains("foreign-branch"));
    assert!(diagnostics.contains(&branch));
    let foreign = lease(
        &project,
        &project.new_story("another owner"),
        &path,
        &branch,
        &socket,
    );
    write_marker(&path, &foreign);
    assert_eq!(report(&project, &[&id])["status"], "invalid");
    let marker = write_marker(&path, &valid);
    let saved = marker.with_extension("saved");
    std::fs::rename(&marker, &saved).unwrap();
    std::os::unix::fs::symlink(&saved, &marker).unwrap();
    assert_eq!(report(&project, &[&id])["status"], "invalid");
    assert!(path.exists());
}

#[test]
fn replacement_cannot_hide_a_surviving_historical_custom_branch() {
    let env = TestEnv::isolated();
    let project = env.project().with_local_origin().build();
    let id = project.new_story("historical branch");
    let old_path = project.path().join("old");
    let old_branch = "worktree-historical";
    git(
        &env,
        project.path(),
        &[
            "worktree",
            "add",
            "-b",
            old_branch,
            old_path.to_str().unwrap(),
            "HEAD",
        ],
    );
    let old = lease(
        &project,
        &id,
        &old_path,
        old_branch,
        &project.path().join("old.sock"),
    );
    write_marker(&old_path, &old);
    env.story(&old_path)
        .args(["move", &id, "in-progress"])
        .assert()
        .success();
    env.story(&old_path)
        .args(["move", &id, "verifying"])
        .assert()
        .success();
    git(
        &env,
        project.path(),
        &["worktree", "remove", old_path.to_str().unwrap()],
    );
    let new_path = project.path().join("replacement");
    let new_branch = format!("worktree-{id}");
    git(
        &env,
        project.path(),
        &[
            "worktree",
            "add",
            "-b",
            &new_branch,
            new_path.to_str().unwrap(),
            "HEAD",
        ],
    );
    let new = lease(
        &project,
        &id,
        &new_path,
        &new_branch,
        &project.path().join("new.sock"),
    );
    write_marker(&new_path, &new);
    let both = report(&project, &[&id]);
    assert_eq!(both["status"], "ambiguous", "{both}");
    assert_eq!(both["candidates"].as_array().unwrap().len(), 2);
    git(&env, project.path(), &["branch", "-D", old_branch]);
    let replaced = report(&project, &[&id]);
    assert_eq!(replaced["status"], "resolved", "{replaced}");
    assert_eq!(replaced["worktree"], new_path.to_str().unwrap());
    let exact_old = report(&project, &[&id, "--lease-json", &old]);
    assert_eq!(
        exact_old["status"], "ambiguous",
        "an explicit lease must never redirect: {exact_old}"
    );
}

#[test]
fn recorded_engine_provider_survives_a_custom_worktree_without_a_pane() {
    use storyhook::store::{
        EngineAgent, EngineLaneRecord, EngineLaneState, EngineRunRecord, EngineRunState,
        EngineScope, ReadOps, SqliteStore, Store, WriteOps,
    };
    let env = TestEnv::isolated();
    let project = env.project().with_local_origin().build();
    let id = project.new_story("engine provider");
    let path = project.path().join("custom-engine-lane");
    let branch = format!("worktree-{id}");
    git(
        &env,
        project.path(),
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            path.to_str().unwrap(),
            "HEAD",
        ],
    );
    let binding: storyhook::domain::StoryCleanupLease = serde_json::from_str(&lease(
        &project,
        &id,
        &path,
        &branch,
        &project.path().join("missing.sock"),
    ))
    .unwrap();
    env.stop_daemon();
    let store = SqliteStore::open(env.store_path()).unwrap();
    let project_id = store
        .read(|tx| Ok(tx.project_by_slug(&binding.project_slug)?.unwrap().id))
        .unwrap();
    let run = EngineRunRecord {
        id: "bound-run".into(),
        project_slug: binding.project_slug.clone(),
        scope: EngineScope::Project,
        lanes: 1,
        agent: EngineAgent::Codex,
        model: None,
        effort: None,
        speed: None,
        state: EngineRunState::Paused,
        consecutive_hard_stops: 0,
        recent_quarantines: vec![],
        stop_reason: None,
        acknowledged_at: None,
        created_at: "2026-09-12T00:00:00Z".into(),
        updated_at: "2026-09-12T00:00:00Z".into(),
    };
    let lane = EngineLaneRecord {
        adopted_identity: None,
        run_id: run.id.clone(),
        lane_index: 0,
        state: EngineLaneState::Working,
        story_id: Some(id.clone()),
        pane_id: None,
        window_name: Some(id.clone()),
        worktree_path: Some(path.to_str().unwrap().into()),
        cleanup_lease: Some(binding),
        dispatched_at: None,
        last_observed_at: run.created_at.clone(),
        last_progress_seq: None,
        last_progress_at: None,
        outcome: None,
        outcome_detail: None,
        probe_detail: None,
    };
    store
        .write(|tx| {
            tx.create_engine_run(&run)?;
            tx.put_engine_lane(&lane)
        })
        .unwrap();
    let ctx = storyhook::service::Ctx::new(&store, project_id, project.path(), env.environment());
    let report = storyhook::service::resources::ResourceService::new(&ctx)
        .resolve(&id, &Default::default())
        .unwrap();
    assert_eq!(report.status, "resolved");
    assert_eq!(report.provider.as_deref(), Some("codex"));
}

#[test]
fn configured_checkout_cannot_override_another_projects_pointer() {
    let env = TestEnv::isolated();
    let project = env.project().with_local_origin().build();
    let id = project.new_story("project association");
    let raw = project
        .story()
        .args(["project", "show", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let project_json: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    let slug = project_json["project"]["slug"].as_str().unwrap();
    let uuid = storyhook::service::project::read_pointer(project.path())
        .unwrap()
        .unwrap()
        .uuid;
    let pointer = project.path().join(".storyhook.toml");
    let previous = std::fs::read_to_string(&pointer).unwrap();
    std::fs::write(
        &pointer,
        previous.replace(&uuid, "00000000-0000-4000-8000-000000000001"),
    )
    .unwrap();
    project
        .story()
        .args(["--project", slug, "resources", &id, "--json"])
        .assert()
        .failure()
        .stdout(predicates::str::contains("configured repository"));
}

#[test]
fn notification_location_keeps_recorded_server_without_selecting_the_active_pane() {
    let env = TestEnv::isolated();
    let project = env.project().with_local_origin().build();
    let id = project.new_story("notification location");
    let path = project.path().join("custom-agent-worktree");
    let branch = format!("worktree-{id}");
    git(
        &env,
        project.path(),
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            path.to_str().unwrap(),
            "HEAD",
        ],
    );
    let owner = PrivateTmux::new();
    let pane = owner.run(&[
        "new-session",
        "-d",
        "-s",
        "owned",
        "-n",
        &id,
        "-c",
        path.to_str().unwrap(),
        "-P",
        "-F",
        "#{pane_id}",
        "sleep 120",
    ]);
    owner.run(&["set-window-option", "-t", &pane, "automatic-rename", "off"]);
    let sibling = owner.run(&[
        "split-window",
        "-d",
        "-t",
        &pane,
        "-c",
        project.path().to_str().unwrap(),
        "-P",
        "-F",
        "#{pane_id}",
        "sleep 120",
    ]);
    owner.run(&["select-pane", "-t", &sibling]);
    let lease = lease(&project, &id, &path, &branch, &owner.socket());
    let ordinary = report(&project, &[&id, "--lease-json", &lease]);
    assert_eq!(
        ordinary["status"], "invalid",
        "cleanup selection remains unchanged: {ordinary}"
    );
    assert_eq!(ordinary["location_only"], false);
    let found = report(
        &project,
        &[
            &id,
            "--lease-json",
            &lease,
            "--location-only",
            "--tmux-socket",
            "/tmp/storyhook-unused-notify-caller.sock",
        ],
    );
    assert_eq!(found["status"], "resolved", "{found}");
    assert_eq!(found["location_only"], true);
    assert_eq!(found["socket_path"], owner.socket().to_str().unwrap());
    assert_eq!(found["worktree"], path.to_str().unwrap());
    assert_eq!(found["window_name"], id);
    assert!(
        found["pane"].is_null(),
        "location is not target authority: {found}"
    );
    // A second Git owner still makes even location-only discovery ambiguous.
    let duplicate = project.path().join(".codex/worktrees").join(&id);
    git(
        &env,
        project.path(),
        &[
            "worktree",
            "add",
            "-b",
            "second-owner",
            duplicate.to_str().unwrap(),
            "HEAD",
        ],
    );
    assert_ne!(
        report(&project, &[&id, "--location-only"])["status"],
        "resolved"
    );
    project
        .story()
        .args(["resources", &id, "--location-only", "--location-only"])
        .assert()
        .failure();
}

#[test]
fn c_locale_preserves_native_tmux_inventory_fields() {
    let env = TestEnv::isolated();
    let project = env.project().with_local_origin().build();
    let id = project.new_story("locale-independent pane identity");
    let path = project.path().join("owned-worktree");
    let branch = format!("worktree-{id}");
    git(
        &env,
        project.path(),
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            path.to_str().unwrap(),
            "HEAD",
        ],
    );
    let owner = PrivateTmux::new();
    let pane = owner.run(&[
        "new-session",
        "-d",
        "-s",
        "owned",
        "-n",
        &id,
        "-c",
        path.to_str().unwrap(),
        "-P",
        "-F",
        "#{pane_id}",
        "sleep 120",
    ]);
    owner.run(&["set-window-option", "-t", &pane, "automatic-rename", "off"]);
    owner.run(&[
        "set-window-option",
        "-t",
        &pane,
        "@storyhook-agent",
        "codex",
    ]);
    let lease = lease(&project, &id, &path, &branch, &owner.socket());
    let output = project
        .story()
        .env("LC_ALL", "C")
        .args(["resources", &id, "--lease-json", &lease, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let output: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let found = &output["resources"];
    assert_eq!(found["status"], "resolved", "{found}");
    assert_eq!(found["provider"], "codex", "{found}");
    assert_eq!(found["pane"]["pane_id"], pane, "{found}");
    assert_eq!(found["pane"]["cwd"], path.to_str().unwrap(), "{found}");
}
