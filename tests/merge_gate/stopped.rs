//! SH-882: full shell chain, real Git, mocked remote endpoint.
use super::*;
impl MergeRepo {
    fn stopped_landing_phase(&self, mode: &str, head: &str, tree: &str) -> Output {
        let path = format!(
            "{}:{}",
            self.path().join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new("bash")
            .arg(checkout().join("scripts/verify-pr.sh"))
            .args([
                "--landing",
                mode,
                "https://github.com/acme/widgets/pull/42",
                head,
                tree,
            ])
            .arg(self.path().join("landing.attempted"))
            .arg("skipped-admission")
            .current_dir(self.path())
            .env("PATH", path)
            .env("STORY_BIN", storyhook_test_support::story_binary())
            .env("STORYHOOK_LOCK_DIR", self.path().join("locks"))
            .env("STORYHOOK_ACTIVITY_LOG_DIR", self.path().join("activity"))
            .envs(storyhook_test_support::daemon_containment())
            .env_remove("STORYHOOK_MACHINE_LOCKS")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
            .output()
            .unwrap()
    }
}

fn prepared() -> (MergeRepo, String, String) {
    let repo = MergeRepo::new();
    let (_, head) = reconciled_feature(&repo);
    repo.publish_origin(42, &head);
    repo.fake_gh();
    converge_public_head(&repo, &head);
    repo.fake_make("echo unexpected gate >&2; exit 99");
    let payload = public_payload(&repo.verify_phase(&["--prepare-without-verification"], true));
    assert_eq!(payload["result"], "prepared", "{payload}");
    assert_eq!(payload["head"], head);
    let tree = payload["tree"].as_str().unwrap().to_owned();
    assert!(
        !repo
            .common_dir()
            .join("storyhook/gate-receipts")
            .join(&tree)
            .exists()
    );
    (repo, head, tree)
}

#[test]
fn stopped_shell_prepares_lands_and_recovers_without_a_gate_receipt() {
    let (repo, head, tree) = prepared();
    repo.enable_fake_merge_endpoint();
    let payload = public_payload(&repo.stopped_landing_phase("attempt", &head, &tree));
    assert_eq!(payload["result"], "merged", "{payload}");
    assert!(repo.path().join("landing.attempted").exists());
    let payload = public_payload(&repo.stopped_landing_phase("recover", &head, &tree));
    assert_eq!(payload["result"], "merged", "{payload}");
    assert!(
        !repo
            .common_dir()
            .join("storyhook/gate-receipts")
            .join(&tree)
            .exists()
    );
    let progress = fs::read_to_string(repo.path().join("gate-progress.ndjson")).unwrap();
    assert!(progress.contains("skipped"), "{progress}");
}

#[test]
fn stopped_shell_refuses_changed_authority_before_sending_a_merge() {
    let (repo, head, tree) = prepared();
    repo.enable_fake_merge_endpoint();
    for (head, tree) in [("0".repeat(40), tree), (head, "0".repeat(40))] {
        let payload = public_payload(&repo.stopped_landing_phase("attempt", &head, &tree));
        assert_eq!(payload["result"], "not-attempted", "{payload}");
        assert!(!repo.path().join("landing.attempted").exists());
    }
}

#[test]
fn stopped_shell_rejects_a_changed_base_tree_before_merge() {
    let (repo, head, tree) = prepared();
    repo.enable_fake_merge_endpoint();
    assert_ok(&repo.git(&["checkout", "-q", "main"]), "checkout base");
    repo.write("later-base", "base advanced after preparation\n");
    assert_ok(&repo.git(&["add", "later-base"]), "stage base movement");
    assert_ok(
        &repo.git(&["commit", "-qm", "advance base"]),
        "advance base",
    );
    let payload = public_payload(&repo.stopped_landing_phase("attempt", &head, &tree));
    assert_eq!(payload["result"], "not-attempted", "{payload}");
    assert!(
        payload["detail"].as_str().unwrap().contains("tree changed"),
        "{payload}"
    );
    assert!(!repo.path().join("landing.attempted").exists());
}

#[test]
fn stopped_shell_reports_conflict_without_running_a_gate() {
    let repo = MergeRepo::new();
    let head = repo.branch("feature", "main", "f", "feature edit\n");
    assert_ok(&repo.git(&["checkout", "-q", "main"]), "checkout base");
    repo.write("f", "different base edit\n");
    assert_ok(&repo.git(&["add", "f"]), "stage conflict");
    assert_ok(
        &repo.git(&["commit", "-qm", "conflicting base"]),
        "commit conflict",
    );
    repo.publish_origin(42, &head);
    repo.fake_gh();
    converge_public_head(&repo, &head);
    let payload = public_payload(&repo.verify_phase(&["--prepare-without-verification"], true));
    assert_eq!(payload["result"], "conflict", "{payload}");
    assert!(!repo.path().join("landing.attempted").exists());
}
