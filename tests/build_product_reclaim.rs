//! SH-835: real store generations, private Git worktrees and configured hooks.
use serde_json::{Value, json};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use storyhook::{
    domain::{StoryCleanupLease, TmuxCleanupTarget},
    service::{Ctx, NewStoryInput, StoryService, build_products::reclaim_handoff},
    store::{ReadOps, Store, StoryNo, WriteOps},
};
use storyhook_test_support::{ServiceFixture, run_bounded};

struct Fixture {
    service: ServiceFixture,
    lane: PathBuf,
    private: PathBuf,
    lease: StoryCleanupLease,
    expected: (i64, StoryCleanupLease),
}
fn git(cwd: &Path, args: &[&str]) -> String {
    let mut c = Command::new("git");
    c.current_dir(cwd)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    let out = run_bounded(
        c,
        "SH835 private Git fixture",
        storyhook_test_support::load_grace::graced_now(Duration::from_secs(30)),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}
fn identity(path: &Path) -> Value {
    let m = path.symlink_metadata().unwrap();
    json!({"dev":m.dev(), "ino":m.ino()})
}
fn private_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
impl Fixture {
    fn new(hook: Vec<String>) -> Self {
        let service = ServiceFixture::new();
        let root = service.cwd().canonicalize().unwrap();
        git(&root, &["init", "-q"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--allow-empty",
                "-qm",
                "fixture",
            ],
        );
        let lane = root.join("lane");
        git(
            &root,
            &[
                "worktree",
                "add",
                "-qb",
                "fixture-lane",
                lane.to_str().unwrap(),
            ],
        );
        let private = PathBuf::from(git(&lane, &["rev-parse", "--absolute-git-dir"]));
        let slug = service
            .store()
            .read(|tx| Ok(tx.project(service.project())?.unwrap().slug))
            .unwrap();
        let lease = StoryCleanupLease {
            version: 1,
            project_slug: slug,
            story_id: "SH-1".into(),
            repository_path: root,
            worktree_path: lane.clone(),
            branch: "fixture-lane".into(),
            tmux: TmuxCleanupTarget {
                socket_path: lane.join("absent.sock"),
                revivify: None,
            },
        };
        let config = json!({"enabled":true,"path":"products", "managed_entry":"scripts/managed-cargo.sh", "hook":hook,"timeout_seconds":120});
        let encoded: storyhook::service::build_products::Config =
            serde_json::from_value(config.clone()).unwrap();
        fs::write(
            lane.join(".storyhook.toml"),
            format!("[build_products]\n{}", toml::to_string(&encoded).unwrap()),
        )
        .unwrap();
        private_json(
            &private.join("storyhook-cleanup-lease-v1.json"),
            &serde_json::to_value(&lease).unwrap(),
        );
        private_json(
            &private.join("storyhook-products-enrollment-v1.json"),
            &json!({"version":1,"lease":lease,"config":config,"worktree":identity(&lane),"private_git":identity(&private)}),
        );
        let custody = private.join("storyhook-build-products-v1");
        fs::create_dir(&custody).unwrap();
        fs::write(custody.join("products.lock"), "").unwrap();
        fs::set_permissions(
            custody.join("products.lock"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let token = "0123456789abcdef0123456789abcdef";
        let owner = custody.join(format!("build-{token}"));
        fs::create_dir(&owner).unwrap();
        private_json(
            &owner.join("record.json"),
            &json!({"version":1,"id":token,"token":token,"state":"finished","executions":[],"command":["fixture"],"owner":{"pid":42,"start":"fixture:1","boot":"fixture"},"settled_execution":{"id":"fixture","session":43,"guard":format!("lease-{token}.lock")}}),
        );
        let ctx = Ctx::new(
            service.store(),
            service.project(),
            &lane,
            service.env().clone(),
        )
        .no_hooks(true);
        StoryService::new(&ctx)
            .create(&NewStoryInput {
                title: "managed products".into(),
                ..Default::default()
            })
            .unwrap();
        StoryService::new(&ctx)
            .set_state("SH-1", "verifying", None, None, None)
            .unwrap();
        let seq = service
            .store()
            .read(|tx| {
                let events =
                    tx.events_for(service.project(), StoryNo::parse_id("SH", "SH-1").unwrap())?;
                Ok(events
                    .iter()
                    .rev()
                    .find(|e| e.kind == "StoryStateChanged")
                    .unwrap()
                    .global_seq
                    .get())
            })
            .unwrap();
        fs::create_dir(lane.join("products")).unwrap();
        fs::write(lane.join("products/old"), "old artifact").unwrap();
        Self {
            service,
            lane,
            private,
            expected: (seq, lease.clone()),
            lease,
        }
    }
    fn ctx(&self) -> Ctx<'_, storyhook::store::SqliteStore> {
        Ctx::new(
            self.service.store(),
            self.service.project(),
            &self.lane,
            self.service.env().clone(),
        )
    }
    fn reclaim(&self) -> Result<(), storyhook::error::AppError> {
        reclaim_handoff(&self.ctx(), "SH-1", self.expected.clone())
    }
    fn original(&self) -> PathBuf {
        self.lane.join("products")
    }
    fn journal(&self) -> PathBuf {
        fs::read_dir(self.private.join("storyhook-detached-products-v1"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("journal.json")
    }
    fn move_to(&self, state: &str) {
        StoryService::new(&self.ctx().no_hooks(true))
            .set_state("SH-1", state, None, None, None)
            .unwrap();
    }
}
fn no_purge() -> Vec<String> {
    vec!["/usr/bin/true".into()]
}
#[test]
fn configured_handoff_detaches_and_keeps_source_and_ownership_evidence() {
    let f = Fixture::new(no_purge());
    f.reclaim().unwrap();
    assert!(!f.original().exists());
    let journal = f.journal();
    assert!(journal.parent().unwrap().join("products/old").exists());
    assert!(f.lane.join(".storyhook.toml").exists());
    assert!(
        f.private
            .join("storyhook-products-enrollment-v1.json")
            .exists()
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(journal).unwrap()).unwrap()["generation"],
        f.expected.0
    );
}
#[test]
fn live_whole_build_lock_defers_without_detachment() {
    let f = Fixture::new(no_purge());
    let lock = fs::File::open(f.private.join("storyhook-build-products-v1/products.lock")).unwrap();
    lock.lock_shared().unwrap();
    f.reclaim().unwrap();
    assert!(f.original().join("old").exists());
    drop(lock);
    f.reclaim().unwrap();
    assert!(!f.original().exists());
}
#[test]
fn unfinished_and_incomplete_finished_records_retain_products() {
    let f = Fixture::new(no_purge());
    let path = f
        .private
        .join("storyhook-build-products-v1/build-0123456789abcdef0123456789abcdef/record.json");
    for state in ["running", "finished"] {
        private_json(&path, &json!({"version":1,"state":state,"executions":[]}));
        assert!(f.reclaim().is_err());
        assert!(f.original().join("old").exists());
    }
}
#[test]
fn legacy_worktree_is_not_enrolled_by_handoff() {
    let f = Fixture::new(no_purge());
    fs::remove_file(f.private.join("storyhook-products-enrollment-v1.json")).unwrap();
    f.reclaim().unwrap();
    assert!(f.original().exists());
}
#[test]
fn stale_callback_cannot_adopt_a_new_verifying_generation() {
    let f = Fixture::new(no_purge());
    f.move_to("in-progress");
    f.move_to("verifying");
    f.reclaim().unwrap();
    assert!(f.original().join("old").exists());
    assert!(!f.private.join("storyhook-detached-products-v1").exists());
}
#[test]
fn returned_story_products_are_never_detached() {
    let f = Fixture::new(no_purge());
    f.move_to("in-progress");
    f.reclaim().unwrap();
    assert!(f.original().exists());
}
#[test]
fn symlink_and_changed_marker_are_retained() {
    let f = Fixture::new(no_purge());
    let actual = f.lane.join("retained");
    fs::rename(f.original(), &actual).unwrap();
    symlink(&actual, f.original()).unwrap();
    assert!(f.reclaim().is_err());
    assert!(actual.join("old").exists());
    fs::remove_file(f.original()).unwrap();
    fs::rename(actual, f.original()).unwrap();
    let mut lease = f.lease.clone();
    lease.branch = "other".into();
    private_json(
        &f.private.join("storyhook-cleanup-lease-v1.json"),
        &serde_json::to_value(lease).unwrap(),
    );
    assert!(f.reclaim().is_err());
    assert!(f.original().exists());
}
#[test]
fn failed_hook_retains_detached_journal_and_return_can_rebuild() {
    let f = Fixture::new(vec!["/usr/bin/false".into()]);
    assert!(f.reclaim().is_err());
    let journal = f.journal();
    f.move_to("in-progress");
    fs::create_dir(f.original()).unwrap();
    fs::write(f.original().join("new"), "replacement").unwrap();
    assert!(journal.parent().unwrap().join("products/old").exists());
    assert!(f.original().join("new").exists());
}
#[test]
fn disabled_no_hooks_and_reenabled_old_generation_cannot_reclaim() {
    let f = Fixture::new(no_purge());
    reclaim_handoff(&f.ctx().no_hooks(true), "SH-1", f.expected.clone()).unwrap();
    assert!(f.original().exists());
    f.service
        .store()
        .write(|tx| {
            let mut s = tx.settings(f.service.project())?;
            s.automations_enabled = Some(false);
            tx.put_settings(f.service.project(), &s)
        })
        .unwrap();
    f.reclaim().unwrap();
    assert!(f.original().exists());
    f.service
        .store()
        .write(|tx| {
            let mut s = tx.settings(f.service.project())?;
            s.automations_enabled = Some(true);
            s.automations_after = Some(f.expected.0);
            tx.put_settings(f.service.project(), &s)
        })
        .unwrap();
    f.reclaim().unwrap();
    assert!(f.original().exists());
}

#[test]
fn unleased_set_fields_submission_retains_products() {
    let f = Fixture::new(no_purge());
    f.move_to("in-progress");
    StoryService::new(&f.ctx())
        .set_fields(
            "SH-1",
            &storyhook::service::FieldEdits {
                state: Some("verifying".into()),
                ..Default::default()
            },
        )
        .unwrap();
    f.reclaim().unwrap();
    assert!(f.original().join("old").exists());
}
#[test]
fn real_managed_entry_record_is_accepted_by_native_reclaimer() {
    let f = Fixture::new(no_purge());
    fs::remove_dir_all(
        f.private
            .join("storyhook-build-products-v1/build-0123456789abcdef0123456789abcdef"),
    )
    .unwrap();
    let mut command = Command::new("bash");
    command.arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/python-runtime.sh")).arg("--")
        .arg("python3")
        .arg("-c").arg(format!("import sys;sys.path.insert(0,{:?});import build_products;raise SystemExit(build_products.run_managed([sys.executable,'-c','print(123)'],cwd={:?}))",Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts").display().to_string(),f.lane.display().to_string()));
    let result = run_bounded(
        command,
        "SH835 managed native fixture",
        storyhook_test_support::load_grace::graced_now(Duration::from_secs(30)),
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    f.reclaim().unwrap();
    assert!(!f.original().exists());
}
#[test]
fn reset_and_new_build_survive_a_purge_paused_after_detachment() {
    let f = Fixture::new(no_purge());
    let ready = f.lease.repository_path.join("purge-ready");
    let release = f.lease.repository_path.join("purge-release");
    let script = f.lease.repository_path.join("pause-purge.sh");
    // Fixed fixture argv; no host paths or provider processes are targeted.
    fs::write(&script,format!("#!/bin/bash\nset -eu\ntouch '{}'\nfor ((i=0;i<1200;i++)); do [ ! -e '{}' ] || break; sleep .1; done\n[ -e '{}' ] || exit 70\nexec bash '{}' \"$1\"\n",ready.display(),release.display(),release.display(),Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/purge-detached-products.sh").display())).unwrap();
    let config_path = f.lane.join(".storyhook.toml");
    let mut config:Value=serde_json::to_value(toml::from_str::<toml::Value>(&fs::read_to_string(&config_path).unwrap()).unwrap()["build_products"].clone()).unwrap();
    config["hook"] = json!(["bash", script]);
    config["timeout_seconds"] = json!(180);
    let c: storyhook::service::build_products::Config =
        serde_json::from_value(config.clone()).unwrap();
    fs::write(
        config_path,
        format!("[build_products]\n{}", toml::to_string(&c).unwrap()),
    )
    .unwrap();
    let enrollment = f.private.join("storyhook-products-enrollment-v1.json");
    let mut row: Value = serde_json::from_slice(&fs::read(&enrollment).unwrap()).unwrap();
    row["config"] = config;
    private_json(&enrollment, &row);
    struct Release(PathBuf);
    impl Drop for Release {
        fn drop(&mut self) {
            let _ = fs::write(&self.0, "release");
        }
    }
    std::thread::scope(|scope| {
        let pending = scope.spawn(|| f.reclaim());
        let _release = Release(release.clone());
        let deadline = std::time::Instant::now()
            + storyhook_test_support::load_grace::graced_now(Duration::from_secs(30));
        while !ready.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            ready.exists(),
            "configured purge never reached detached phase"
        );
        assert!(!f.original().exists());
        let ctx = f.ctx().no_hooks(true);
        let reset = storyhook::service::story_reset::StoryResetService::new(&ctx)
            .with_workspace_patience(Duration::ZERO);
        let reserved = reset.reserve("SH-1", "SH-1").unwrap();
        let result = reset.execute("SH-1", &reserved.token, || Ok(())).unwrap();
        assert!(result.completed);
        fs::create_dir_all(f.original()).unwrap();
        fs::write(f.original().join("new"), "new generation").unwrap();
        fs::write(&release, "release").unwrap();
        let outcome = pending.join().unwrap();
        // Reset may retain the dirty lane or retire its Git admin; either
        // outcome must leave the recreated original outside purge authority.
        if let Err(error) = outcome {
            assert!(
                error.to_string().contains("purge") || error.to_string().contains("No such file"),
                "{error}"
            );
        }
        assert_eq!(
            fs::read_to_string(f.original().join("new")).unwrap(),
            "new generation"
        );
    });
    assert_eq!(
        storyhook::service::story_reset::WORKSPACE_PATIENCE,
        Duration::from_secs(60)
    );
}

#[test]
fn ordinary_move_handoff_runs_configured_detachment() {
    let f = Fixture::new(no_purge());
    f.move_to("in-progress");
    StoryService::new(&f.ctx())
        .set_state("SH-1", "verifying", None, None, None)
        .unwrap();
    assert!(!f.original().exists());
    assert!(f.journal().parent().unwrap().join("products/old").exists());
}

#[test]
fn replayed_generation_retains_a_recreated_original() {
    let f = Fixture::new(no_purge());
    f.reclaim().unwrap();
    fs::create_dir(f.original()).unwrap();
    fs::write(f.original().join("replacement"), "keep").unwrap();
    assert!(
        f.reclaim()
            .unwrap_err()
            .to_string()
            .contains("already has a detachment job")
    );
    assert_eq!(
        fs::read_to_string(f.original().join("replacement")).unwrap(),
        "keep"
    );
    assert_eq!(
        fs::read_dir(f.private.join("storyhook-detached-products-v1"))
            .unwrap()
            .count(),
        1
    );
}
#[test]
fn absent_managed_build_evidence_retains_existing_products() {
    let f = Fixture::new(no_purge());
    fs::remove_dir_all(
        f.private
            .join("storyhook-build-products-v1/build-0123456789abcdef0123456789abcdef"),
    )
    .unwrap();
    assert!(
        f.reclaim()
            .unwrap_err()
            .to_string()
            .contains("no completed managed build")
    );
    assert!(f.original().exists());
}
