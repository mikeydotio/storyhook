//! SH-714: retry observability through the shipped verifier and real local Git.

#[path = "verification_retry_shell/callback.rs"]
mod callback;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use storyhook::api::http::TrustedHosts;
use storyhook::api::rest;
use storyhook::daemon::http1::{Header, Method};
use storyhook::daemon::lifecycle::InFlight;
use storyhook::daemon::verification::{
    ShellVerificationActuator, TickResult, VerificationActivity, journal_path, tick_with_activity,
};
use storyhook::daemon::verification_progress::{
    VerificationStatus, publish_once, status_snapshot_with_incident,
};
use storyhook::service::gate_progress::GATE_PROGRESS_PREFIX;
use storyhook::service::{Clock, NewStoryInput, PrLinkService, StoryService, VerificationQueue};
use storyhook::store::{ReadOps, Store, StoryNo, WriteOps};
use storyhook_test_support::{ChildGuard, ServiceFixture, daemon_containment, scratch_dir};

/// Only the remote GitHub response is substituted. A merge request creates an
/// actual two-parent commit in the fixture remote, enabling production landing
/// to check ancestry and the exact certified tree with ordinary Git.
const GH: &str = r#"#!/usr/bin/env python3
import json, os, pathlib, subprocess, sys
root = pathlib.Path(__file__).resolve().parent.parent
metadata = root / 'metadata.json'
data = json.loads(metadata.read_text())
if sys.argv[1:3] == ['pr', 'view']:
    print(json.dumps(data))
elif sys.argv[1:3] == ['pr', 'merge']:
    assert sys.argv[3] == '1' and '--merge' in sys.argv
    assert sys.argv[sys.argv.index('--match-head-commit') + 1] == data['headRefOid']
    remote = str(root / 'remote.git')
    def git(*args, input=None):
        env = dict(os.environ)
        for key in ['GIT_OBJECT_DIRECTORY', 'GIT_ALTERNATE_OBJECT_DIRECTORIES', 'GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE']:
            env.pop(key, None)
        return subprocess.check_output(['git', '--git-dir', remote, *args], input=input, text=True, env=env).strip()
    base = git('rev-parse', 'refs/heads/main')
    head = data['headRefOid']
    tree = git('merge-tree', '--write-tree', base, head)
    commit = git('commit-tree', tree, '-p', base, '-p', head, input='SH-714 fixture merge\n')
    git('update-ref', 'refs/heads/main', commit, base)
    data.update(state='MERGED', mergedAt='2026-01-01T00:00:00Z', mergeCommit={'oid': commit})
    metadata.write_text(json.dumps(data))
else:
    raise SystemExit('unexpected external GitHub request: ' + repr(sys.argv))
"#;

#[test]
fn real_head_convergence_retry_reports_running_then_preserves_red() {
    isolated_scenario("red");
}

#[test]
fn real_head_convergence_retry_reports_running_then_preserves_green() {
    isolated_scenario("green");
}

fn isolated_scenario(verdict: &str) {
    let root = scratch_dir();
    let bin = root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    fs::write(bin.join("gh"), GH).unwrap();
    fs::set_permissions(bin.join("gh"), fs::Permissions::from_mode(0o755)).unwrap();
    storyhook_test_support::install_git_endpoint(
        &bin,
        &[(
            "https://github.example.com/acme/widgets.git",
            &root.path().join("remote.git"),
        )],
    );
    let log_path = root.path().join("worker.log");
    let log = fs::File::create(&log_path).unwrap();
    // The child is the environment boundary: integration-test threads never
    // mutate PATH, and Cargo retains the real machine-wide compiler locks.
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--ignored", "--exact", "retry_shell_worker", "--nocapture"])
        .env_clear()
        .envs(daemon_containment())
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("HOME", root.path())
        .env("TMPDIR", "/tmp")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("STORYHOOK_LOCK_DIR", root.path().join("locks"))
        .env("STORYHOOK_VERIFIER_MIRROR", "0")
        .env("SH714_FIXTURE_ROOT", root.path())
        .env("SH714_VERDICT", verdict)
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log);
    let mut child = ChildGuard::spawn(&mut command).unwrap();
    let status = child.wait_within(Duration::from_secs(120), || {
        format!(
            "{verdict} worker exceeded deadline:\n{}",
            fs::read_to_string(&log_path).unwrap()
        )
    });
    assert!(
        status.success(),
        "{verdict} worker failed:\n{}",
        fs::read_to_string(&log_path).unwrap()
    );
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// The release marker is outside the judged checkout. Unwinding an assertion
/// releases the real gate before the scoped verifier thread must join.
struct GateRelease(PathBuf);

impl Drop for GateRelease {
    fn drop(&mut self) {
        if !self.0.exists() {
            fs::write(&self.0, "3").expect("release fixture gate during cleanup");
        }
    }
}

#[test]
#[ignore = "invoked only by a parent with an isolated subprocess environment"]
fn retry_shell_worker() {
    let root =
        PathBuf::from(std::env::var_os("SH714_FIXTURE_ROOT").expect("isolated parent required"));
    let verdict = std::env::var("SH714_VERDICT").unwrap();
    fs::write(root.join(".gitconfig"), "[user]\n name = SH714 Fixture\n email = fixture@example.test\n[protocol \"file\"]\n allow = always\n").unwrap();
    git(
        &root,
        &[
            "init",
            "--bare",
            "-q",
            "--initial-branch=main",
            "remote.git",
        ],
    );
    let checkout = root.join("checkout");
    fs::create_dir(&checkout).unwrap();
    git(&checkout, &["init", "-q", "--initial-branch=main"]);
    git(
        &checkout,
        &[
            "remote",
            "add",
            "origin",
            "https://github.example.com/acme/widgets.git",
        ],
    );
    fs::write(checkout.join(".storyhook.toml"), "schema = 1\nuuid = \"fixture-uuid\"\nprefix = \"SH\"\n[verify]\ngate = \"python3 gate.py\"\n").unwrap();
    let gate = format!(
        r#"import os, pathlib, subprocess, time
root = pathlib.Path({root})
receipt = os.environ['STORYHOOK_GATE_RECEIPT']
subprocess.run(['bash', receipt, 'preflight'], check=True)
(root / 'gate-ready').write_text('ready\n')
deadline = time.monotonic() + 45
while not (root / 'gate-release').exists():
    if time.monotonic() >= deadline:
        raise SystemExit('fixture gate release deadline expired')
    time.sleep(0.01)
status = int((root / 'gate-release').read_text())
print('test sh714_actual_gate ... ' + ('ok' if status == 0 else 'FAILED'), flush=True)
if status == 0:
    subprocess.run(['bash', receipt, 'postlude', 'gate'], check=True)
raise SystemExit(status)
"#,
        root = serde_json::to_string(root.to_str().unwrap()).unwrap()
    );
    fs::write(checkout.join("gate.py"), gate).unwrap();
    git(&checkout, &["add", "."]);
    git(&checkout, &["commit", "-qm", "base fixture"]);
    let base = git(&checkout, &["rev-parse", "HEAD"]);
    git(&checkout, &["checkout", "-qb", "feature"]);
    fs::write(checkout.join("candidate.txt"), "candidate\n").unwrap();
    git(&checkout, &["add", "candidate.txt"]);
    git(&checkout, &["commit", "-qm", "candidate fixture"]);
    let head = git(&checkout, &["rev-parse", "HEAD"]);
    git(&checkout, &["push", "-q", "origin", "main", "feature"]);
    git(
        &checkout,
        &["push", "-q", "origin", &format!("{base}:refs/pull/1/head")],
    );
    git(&checkout, &["checkout", "-q", "main"]);
    fs::write(
        root.join("metadata.json"),
        serde_json::json!({
            "number": 1, "state": "OPEN", "isDraft": false, "isCrossRepository": false,
            "baseRefName": "main", "headRefName": "feature", "headRefOid": head,
            "mergeCommit": null,
        })
        .to_string(),
    )
    .unwrap();

    let f = ServiceFixture::new();
    f.link_origin("https://github.example.com/acme/widgets");
    f.store()
        .write(|tx| tx.set_checkout_path(f.project(), Some(&checkout)))
        .unwrap();
    for number in 1..=2 {
        let id = StoryService::new(&f.ctx())
            .create(&NewStoryInput {
                title: format!("real retry {number}"),
                ..Default::default()
            })
            .unwrap()
            .id;
        PrLinkService::new(&f.ctx())
            .link(
                &id,
                &format!("https://github.example.com/acme/widgets/pull/{number}"),
                true,
            )
            .unwrap();
        StoryService::new(&f.ctx())
            .set_state(&id, "verifying", None, None, None)
            .unwrap();
    }
    let activity = VerificationActivity::new();
    let callback = callback::Callback::start(&root, &f, activity.clone());
    // No control helper exists: RED tests real absent-agent parking, and
    // GREEN retains the production pending-cleanup result for an unleased row.
    let actuator = ShellVerificationActuator::with_paths_and_timing(
        f.env().clone(),
        root.join("absent-agent-helper"),
        callback.executable.clone(),
        Duration::from_secs(60),
        Duration::from_secs(5),
        Duration::from_secs(5),
    )
    .with_activity(activity.clone());
    fs::create_dir_all(f.env().daemon_state_dir()).unwrap();
    let inflight = InFlight::new(f.env().clone());
    let tick = || {
        tick_with_activity(
            f.store(),
            f.env(),
            &actuator,
            &activity,
            &inflight,
            f.project(),
        )
        .unwrap()
    };
    let refused = Command::new(&callback.executable)
        .args([
            "--project",
            "fixture",
            "verifier",
            "repair-admit",
            "SH-1",
            "foreign-owner",
            "1",
            &"a".repeat(40),
            &"b".repeat(40),
            &"c".repeat(40),
            &"d".repeat(40),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stdout).contains("live uncancelled verifier owner"));
    assert_eq!(tick(), TickResult::RetryLater);
    let incident = f
        .store()
        .read(|tx| tx.verification_incident(f.project()))
        .unwrap()
        .unwrap();
    assert_eq!(incident.attempts, 1);
    assert!(!incident.halted);
    assert!(
        incident.detail.contains("head has not converged"),
        "{incident:?}"
    );
    assert!(!root.join("gate-ready").exists());
    git(
        &checkout,
        &["push", "-q", "origin", &format!("{head}:refs/pull/1/head")],
    );

    let result = thread::scope(|scope| {
        let running = scope.spawn(tick);
        let release = GateRelease(root.join("gate-release"));
        let deadline = Instant::now() + Duration::from_secs(40);
        while !root.join("gate-ready").exists() {
            if running.is_finished() {
                panic!(
                    "retry finished before real gate readiness: {:?}; incident: {:?}",
                    running.join().unwrap(),
                    f.store()
                        .read(|tx| tx.verification_incident(f.project()))
                        .unwrap()
                );
            }
            assert!(
                Instant::now() < deadline,
                "real retry gate never became ready"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let ordered = VerificationQueue::new(f.store())
            .ordered_for(f.project())
            .unwrap();
        let active = activity.active_for(f.project()).unwrap();
        let journal = fs::read_to_string(journal_path(f.env(), &ordered[0])).unwrap();
        assert!(journal.contains(&active.attempt_id), "{journal}");
        let now = f.env().now();
        let statuses =
            status_snapshot_with_incident(&ordered, Some(&active), Some(&incident), f.env(), &now);
        assert!(
            matches!(statuses[0].2, VerificationStatus::Running { .. }),
            "real gate is executing but story projection says {:?}; journal:\n{journal}",
            statuses[0].2
        );
        assert!(
            matches!(
                statuses[1].2,
                VerificationStatus::Queued {
                    position: 1,
                    blocked_by: None,
                    ..
                }
            ),
            "{:?}",
            statuses[1].2
        );
        let status = activity
            .status(&f.ctx().clock(Clock::Fixed(now.clone())))
            .unwrap();
        assert!(status.held_stories.is_empty(), "{status:?}");
        assert!(status.warning.is_none(), "{status:?}");
        assert_eq!(status.incident.as_ref(), Some(&incident));
        let routed = rest::route_with_activity(
            f.store(),
            f.env(),
            &activity,
            rest::RouteRequest::new(
                &Method::Get,
                "/api/repos/fixture/data",
                &[Header::from_bytes("Host", "127.0.0.1:3456").unwrap()],
                "",
            ),
            &TrustedHosts::default(),
        );
        assert_eq!(routed.reply.status, 200);
        let data: serde_json::Value =
            serde_json::from_str(routed.reply.text_body().unwrap()).unwrap();
        let stories = data["stories"].as_array().unwrap();
        assert_eq!(
            stories
                .iter()
                .find(|row| row["story"]["id"] == "SH-1")
                .unwrap()["verification"]["status"],
            "running"
        );
        assert!(
            stories
                .iter()
                .find(|row| row["story"]["id"] == "SH-2")
                .unwrap()["verification"]
                .get("blocked_by")
                .is_none()
        );
        publish_once(f.store(), f.env(), &now, &activity).unwrap();
        let row = f
            .store()
            .read(|tx| tx.story(f.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap();
        let progress = row
            .snapshot
            .comments
            .iter()
            .find(|comment| comment.text.starts_with(GATE_PROGRESS_PREFIX))
            .unwrap();
        assert!(
            !progress.text.contains("RETRYING INFRASTRUCTURE"),
            "{}",
            progress.text
        );
        assert!(progress.text.contains("release gate"), "{}", progress.text);
        assert_eq!(
            f.store()
                .read(|tx| tx.verification_incident(f.project()))
                .unwrap()
                .as_ref(),
            Some(&incident)
        );
        fs::write(&release.0, if verdict == "green" { "0" } else { "3" }).unwrap();
        running.join().unwrap()
    });
    assert!(activity.active_for(f.project()).is_none());
    assert!(storyhook::daemon::lifecycle::read_owned_processes(f.env()).is_empty());
    assert!(
        f.store()
            .read(|tx| tx.verification_incident(f.project()))
            .unwrap()
            .is_none()
    );
    let row = f
        .store()
        .read(|tx| tx.story(f.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    if verdict == "green" {
        assert_eq!(result, TickResult::Completed);
        assert!(
            row.snapshot
                .comments
                .iter()
                .any(|comment| comment.text.contains("CENTRAL VERIFICATION GREEN")),
            "{:?}",
            row.snapshot.comments
        );
        let metadata: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(root.join("metadata.json")).unwrap()).unwrap();
        assert_eq!(metadata["state"], "MERGED");
        let merged = metadata["mergeCommit"]["oid"].as_str().unwrap();
        assert_eq!(git(&checkout, &["rev-parse", "origin/main"]), merged);
    } else {
        assert_eq!(result, TickResult::Returned);
        assert!(
            row.snapshot
                .comments
                .iter()
                .any(|comment| comment.text.contains("sh714_actual_gate")),
            "{:?}",
            row.snapshot.comments
        );
        assert_eq!(git(&checkout, &["rev-parse", "origin/main"]), base);
    }
    assert!(callback.calls() >= 2, "real gate never requested admission");
}
