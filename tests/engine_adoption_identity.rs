//! SH-700: production inspection against real Git leases and a private tmux server.
use std::path::{Path, PathBuf};
use std::process::Command;
use storyhook::domain::{CLEANUP_LEASE_MARKER, StoryCleanupLease, TmuxCleanupTarget};
use storyhook::service::engine::adoption::{DispatchInspector, LiveDispatchInspector};
use storyhook_test_support::scratch_dir;

struct Dispatch {
    _root: tempfile::TempDir,
    repository: PathBuf,
    worktree: PathBuf,
    socket: PathBuf,
    marker: PathBuf,
    lease: StoryCleanupLease,
}

fn output(command: &mut Command) -> String {
    let out = command.output().unwrap();
    assert!(
        out.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}

impl Dispatch {
    fn tmux(&self) -> Command {
        let mut command = Command::new("tmux");
        command.env_remove("TMUX").args(["-S"]).arg(&self.socket);
        command
    }
    fn new(provider: &str) -> Self {
        let root = scratch_dir();
        let repository = root.path().join("repo");
        std::fs::create_dir(&repository).unwrap();
        let git = |args: &[&str]| output(Command::new("git").current_dir(&repository).args(args));
        git(&["init", "-b", "main"]);
        git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ]);
        let worktree = root.path().join("worktree");
        git(&[
            "worktree",
            "add",
            "-b",
            "worktree-SH-1",
            worktree.to_str().unwrap(),
        ]);
        let marker = PathBuf::from(output(
            Command::new("git")
                .current_dir(&worktree)
                .args(["rev-parse", "--absolute-git-dir"]),
        ))
        .join(CLEANUP_LEASE_MARKER);
        let socket = root.path().join("tmux.sock");
        let lease = StoryCleanupLease {
            version: 1,
            project_slug: "fixture".into(),
            story_id: "SH-1".into(),
            repository_path: repository.canonicalize().unwrap(),
            worktree_path: worktree.canonicalize().unwrap(),
            branch: "worktree-SH-1".into(),
            tmux: TmuxCleanupTarget {
                socket_path: socket.clone(),
            },
        };
        let dispatch = Self {
            _root: root,
            repository,
            worktree,
            socket,
            marker,
            lease,
        };
        let executable = dispatch._root.path().join(provider);
        let source = dispatch._root.path().join("agent.c");
        std::fs::write(
            &source,
            "#include <unistd.h>\nint main(void) { sleep(600); return 0; }\n",
        )
        .unwrap();
        output(Command::new("cc").arg(&source).arg("-o").arg(&executable));
        let launch = format!(
            "exec '{}' 600",
            executable.display().to_string().replace('\'', "'\\''")
        );
        output(
            dispatch
                .tmux()
                .args([
                    "-f",
                    "/dev/null",
                    "new-session",
                    "-d",
                    "-s",
                    "private",
                    "-n",
                    "SH-1",
                    "-c",
                ])
                .arg(&dispatch.worktree)
                .arg(launch),
        );
        output(dispatch.tmux().args([
            "set-option",
            "-w",
            "-t",
            "=private:=SH-1",
            "@storyhook-agent",
            provider,
        ]));
        output(dispatch.tmux().args([
            "set-option",
            "-w",
            "-t",
            "=private:=SH-1",
            "automatic-rename",
            "off",
        ]));
        dispatch.write_lease();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let command = output(dispatch.tmux().args([
                "display-message",
                "-p",
                "-t",
                "=private:=SH-1",
                "#{pane_current_command}",
            ]));
            if command == provider {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "agent executable never became visible: {command}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        dispatch
    }
    fn write_lease(&self) {
        std::fs::write(&self.marker, serde_json::to_vec(&self.lease).unwrap()).unwrap();
    }
    fn inspect(
        &self,
    ) -> Result<storyhook::service::engine::adoption::InspectedDispatch, storyhook::error::AppError>
    {
        LiveDispatchInspector.inspect(&self.repository, "fixture", "SH-1")
    }
}
impl Drop for Dispatch {
    fn drop(&mut self) {
        let result = self.tmux().arg("kill-server").output();
        if !std::thread::panicking() {
            assert!(
                result.unwrap().status.success(),
                "private tmux cleanup failed"
            );
        }
    }
}

#[test]
fn both_providers_are_bound_to_the_exact_lease_socket_and_pane() {
    for provider in ["codex", "claude"] {
        let dispatch = Dispatch::new(provider);
        let found = dispatch.inspect().unwrap();
        assert_eq!(found.lease, dispatch.lease);
        assert_eq!(found.identity.provider.as_str(), provider);
        assert!(found.pane_id.starts_with('%'));
        assert!(found.identity.pane_pid > 0);
    }
}

#[test]
fn missing_malformed_and_contradictory_leases_are_refused() {
    let mut dispatch = Dispatch::new("codex");
    std::fs::remove_file(&dispatch.marker).unwrap();
    assert!(dispatch.inspect().is_err());
    std::fs::write(&dispatch.marker, "not json").unwrap();
    assert!(dispatch.inspect().is_err());
    dispatch.lease.branch = "different".into();
    dispatch.write_lease();
    assert!(dispatch.inspect().is_err());
    dispatch.lease.branch = "worktree-SH-1".into();
    dispatch.lease.story_id = "SH-2".into();
    dispatch.write_lease();
    assert!(dispatch.inspect().is_err());
}

#[test]
fn provider_mismatch_duplicate_window_and_wrong_socket_are_refused() {
    let mut dispatch = Dispatch::new("codex");
    output(dispatch.tmux().args([
        "set-option",
        "-w",
        "-t",
        "=private:=SH-1",
        "@storyhook-agent",
        "claude",
    ]));
    assert!(dispatch.inspect().is_err());
    output(dispatch.tmux().args([
        "set-option",
        "-w",
        "-t",
        "=private:=SH-1",
        "@storyhook-agent",
        "codex",
    ]));
    let duplicate = output(dispatch.tmux().args([
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{window_id}",
        "-t",
        "private",
        "-n",
        "SH-1",
        "sleep 600",
    ]));
    assert!(dispatch.inspect().is_err());
    output(dispatch.tmux().args(["kill-window", "-t", &duplicate]));
    assert!(dispatch.inspect().is_ok());
    dispatch.lease.tmux.socket_path = Path::new("/tmp/nonexistent-sh700-socket").into();
    dispatch.write_lease();
    assert!(dispatch.inspect().is_err());
}

#[test]
fn production_invocation_canonicalizes_adoption_and_detects_a_respawned_pane() {
    use storyhook::service::engine::EngineService;
    use storyhook::service::engine::{Dispatcher, ShellDispatcher, StartRequest, WindowProbe};
    use storyhook::service::{NewStoryInput, StoryService};
    use storyhook::store::{EngineAgent, EngineScope, Store, WriteOps};
    let dispatch = Dispatch::new("codex");
    let fixture = storyhook_test_support::ServiceFixture::new();
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), Some(&dispatch.repository)))
        .unwrap();
    let ctx = fixture.ctx();
    let stories = StoryService::new(&ctx);
    let story = stories
        .create(&NewStoryInput {
            title: "existing manual dispatch".into(),
            ..Default::default()
        })
        .unwrap();
    stories.claim_story(&story.id, None).unwrap();
    let fake = storyhook_test_support::FakeDispatcher::default();
    let service = EngineService::new(&ctx, &fake);
    let run = service
        .start(StartRequest {
            scope: EngineScope::Project,
            lanes: 1,
            agent: EngineAgent::Claude,
            model: None,
            effort: None,
            speed: None,
        })
        .unwrap();
    service.pause(&run.id).unwrap();
    let invocation =
        storyhook::cli::parse_invocation(&["engine", "adopt", "1"].map(str::to_owned)).unwrap();
    let response = storyhook::invoke::dispatch(&ctx, invocation).unwrap();
    let storyhook::output::Response::EngineRun(view) = response else {
        panic!("expected engine run")
    };
    assert_eq!(view.lanes[0].story.as_deref(), Some("SH-1"));
    assert_eq!(
        view.lanes[0].adopted_identity.as_ref().unwrap().provider,
        EngineAgent::Codex
    );
    let lane = service
        .status(Some(&run.id))
        .unwrap()
        .remove(0)
        .lanes
        .remove(0);
    let observer = ShellDispatcher::new("unused-helper", fixture.env().clone());
    let pane = lane.pane_id.as_deref().unwrap();
    assert!(matches!(
        observer.probe_lane(&lane, pane),
        WindowProbe::Alive { .. }
    ));
    let executable = dispatch._root.path().join("codex");
    let launch = format!("exec '{}'", executable.display());
    output(
        dispatch
            .tmux()
            .args(["respawn-pane", "-k", "-t", pane, &launch]),
    );
    assert!(matches!(
        observer.probe_lane(&lane, pane),
        WindowProbe::Gone { .. }
    ));
}

#[test]
fn dead_panes_and_unrelated_working_directories_are_refused() {
    let dispatch = Dispatch::new("codex");
    output(dispatch.tmux().args([
        "set-option",
        "-w",
        "-t",
        "=private:=SH-1",
        "remain-on-exit",
        "on",
    ]));
    output(
        dispatch
            .tmux()
            .args(["respawn-pane", "-k", "-t", "=private:=SH-1", "exit 0"]),
    );
    assert!(dispatch.inspect().is_err());
    let executable = dispatch._root.path().join("codex");
    let launch = format!("exec '{}'", executable.display());
    output(
        dispatch
            .tmux()
            .args(["respawn-pane", "-k", "-t", "=private:=SH-1", "-c"])
            .arg(&dispatch.repository)
            .arg(launch),
    );
    assert!(dispatch.inspect().is_err());
}

#[test]
fn real_cli_adopts_manual_work_and_configures_the_same_paused_run() {
    use storyhook::store::{
        EngineAgent, EngineLaneRecord, EngineLaneState, EngineRunRecord, EngineRunState,
        EngineScope, ReadOps, SqliteStore, Store, WriteOps,
    };
    let mut dispatch = Dispatch::new("codex");
    let env = storyhook_test_support::TestEnv::isolated();
    let _daemon = storyhook_test_support::DaemonGuard::new(&env, &dispatch.repository);
    let project = env.project().prefix("SH").build();
    let slug = project.slug();
    let id = project.new_story("manually dispatched");
    project.run(&["claim", &id]).success();
    dispatch.lease.project_slug = slug.clone();
    dispatch.write_lease();
    let store = SqliteStore::open(env.store_path()).unwrap();
    let project_id = store
        .read(|tx| Ok(tx.project_by_slug(&slug)?.unwrap().id))
        .unwrap();
    let now = "2026-01-01T00:00:00Z".to_string();
    let run = EngineRunRecord {
        id: "adoption-cli".into(),
        project_slug: slug.clone(),
        scope: EngineScope::Project,
        lanes: 1,
        agent: EngineAgent::Claude,
        model: Some("original-model".into()),
        effort: Some("high".into()),
        speed: None,
        state: EngineRunState::Paused,
        consecutive_hard_stops: 0,
        recent_quarantines: Vec::new(),
        stop_reason: None,
        acknowledged_at: None,
        created_at: now.clone(),
        updated_at: now.clone(),
    };
    let lane = EngineLaneRecord {
        run_id: run.id.clone(),
        lane_index: 0,
        state: EngineLaneState::Idle,
        story_id: None,
        pane_id: None,
        window_name: None,
        worktree_path: None,
        cleanup_lease: None,
        adopted_identity: None,
        dispatched_at: None,
        last_observed_at: now,
        last_progress_seq: None,
        last_progress_at: None,
        outcome: None,
        outcome_detail: None,
        probe_detail: None,
    };
    store
        .write(|tx| {
            tx.set_checkout_path(project_id, Some(&dispatch.repository))?;
            tx.create_engine_run(&run)?;
            tx.put_engine_lane(&lane)
        })
        .unwrap();
    let invoke = |args: &[&str]| {
        let out = env
            .story(&dispatch.repository)
            .args(["--project", &slug])
            .args(args)
            .arg("--json")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()
    };
    let adopted = invoke(&["engine", "adopt", "1"]);
    assert_eq!(adopted["run"]["id"], "adoption-cli");
    assert_eq!(adopted["run"]["lanes"][0]["story"], "SH-1");
    assert_eq!(
        adopted["run"]["lanes"][0]["adopted_identity"]["provider"],
        "codex"
    );
    let configured = invoke(&["engine", "configure", "--lanes", "2"]);
    assert_eq!(configured["run"]["id"], "adoption-cli");
    assert_eq!(configured["run"]["model"], "original-model");
    assert_eq!(configured["run"]["lanes"].as_array().unwrap().len(), 2);
    let before_retry = store.read(|tx| tx.engine_lanes(&run.id)).unwrap();
    let repeated = invoke(&["engine", "adopt", "SH-1"]);
    assert_eq!(repeated["run"]["lanes"].as_array().unwrap().len(), 2);
    // Rendered elapsed times advance naturally; idempotence concerns durable bindings.
    assert_eq!(
        store.read(|tx| tx.engine_lanes(&run.id)).unwrap(),
        before_retry
    );
}
