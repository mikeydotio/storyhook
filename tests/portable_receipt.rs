//! SH-665: foreign gates certify through the daemon's portable receipt writer.
//!
//! Real Git, the materialized production bundle, and the production merge
//! reader exercise the contract without copying StoryHook hooks into a project.

use std::fs;
use std::path::PathBuf;
use std::process::Output;

#[path = "support/foreign_repo.rs"]
mod foreign_repo;

use foreign_repo::{ForeignRepo, git, output, success};

/// The receipt writer's own view of the shared foreign project.
impl ForeignRepo {
    fn gate(&self, body: &str, args: &[&str]) -> Output {
        output(
            self.speculative_run(body, args)
                .env("STORYHOOK_GATE_RECEIPT", "/inherited/invalid/receipt"),
        )
    }

    fn writer(&self, args: &[&str]) -> Output {
        output(self.command("tree-receipt.sh", &self.repo).args(args))
    }

    fn receipt(&self) -> PathBuf {
        self.repo
            .join(".git/storyhook/gate-receipts")
            .join(&self.tree)
    }
}

#[test]
fn bundled_writer_certifies_foreign_merge_without_enrolling_hooks() {
    for hooks in [None, Some("custom hooks")] {
        for tier in ["gate", "full"] {
            let fixture = ForeignRepo::new();
            if let Some(hooks) = hooks {
                git(&fixture.repo, &["config", "core.hooksPath", hooks]);
            }
            let config_before = fs::read(fixture.repo.join(".git/config")).unwrap();
            success(fixture.gate(
                r#"
test "$STORYHOOK_GATE_RECEIPT" = "$1"
test ! -e scripts
test ! -e .githooks
"$STORYHOOK_GATE_RECEIPT" preflight
test "$(cat base)" = base
test "$(cat main)" = main
test "$(cat feature)" = feature
"$STORYHOOK_GATE_RECEIPT" postlude "$2"
"#,
                &[
                    fixture.bundle.join("tree-receipt.sh").to_str().unwrap(),
                    tier,
                ],
            ));
            assert_eq!(success(fixture.preflight()), fixture.tree);
            assert!(
                fs::read_to_string(fixture.receipt())
                    .unwrap()
                    .contains(&format!("tier {tier}\n"))
            );
            assert_eq!(
                fs::read(fixture.repo.join(".git/config")).unwrap(),
                config_before
            );
            fixture.assert_restored();
        }
    }
}

#[test]
fn a_failed_or_unbracketed_foreign_gate_never_certifies() {
    for (body, status) in [
        ("true", 0),
        ("\"$STORYHOOK_GATE_RECEIPT\" preflight", 0),
        ("\"$STORYHOOK_GATE_RECEIPT\" postlude gate", 1),
        (
            "\"$STORYHOOK_GATE_RECEIPT\" preflight\nfalse\n\"$STORYHOOK_GATE_RECEIPT\" postlude gate",
            1,
        ),
    ] {
        let fixture = ForeignRepo::new();
        let result = fixture.gate(body, &[]);
        assert_eq!(result.status.code(), Some(status), "{body}: {result:?}");
        assert!(!fixture.receipt().exists());
        assert_eq!(fixture.preflight().status.code(), Some(1));
        fixture.assert_restored();
    }
}

#[test]
fn portable_changed_receipt_cannot_certify_a_merge() {
    let fixture = ForeignRepo::new();
    success(fixture.writer(&["preflight"]));
    success(fixture.writer(&["postlude", "gate"]));
    let base_tree = git(&fixture.repo, &["rev-parse", "HEAD^{tree}"]);
    success(fixture.gate("\"$STORYHOOK_GATE_RECEIPT\" preflight\n\"$STORYHOOK_GATE_RECEIPT\" postlude changed \"$1\"", &[&base_tree]));
    assert!(
        fs::read_to_string(fixture.receipt())
            .unwrap()
            .contains("tier changed\n")
    );
    assert_eq!(fixture.preflight().status.code(), Some(1));
    fixture.assert_restored();
}

#[test]
fn portable_writer_rejects_drift_and_keeps_receipts_project_local() {
    let fixture = ForeignRepo::new();
    success(fixture.writer(&["preflight"]));
    fs::write(fixture.repo.join("base"), "changed during gate\n").unwrap();
    let result = fixture.writer(&["postlude", "gate"]);
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("tracked content changed"),
        "{result:?}"
    );
    assert!(!fixture.repo.join(".git/storyhook/gate-receipts").exists());
    assert!(!fixture.repo.join(".git/storyhook-gate-preflight").exists());
    fs::write(fixture.repo.join("base"), "base\n").unwrap();
    success(fixture.gate(
        "\"$STORYHOOK_GATE_RECEIPT\" preflight\n\"$STORYHOOK_GATE_RECEIPT\" postlude gate",
        &[],
    ));
    assert_eq!(success(fixture.preflight()), fixture.tree);
    let other = ForeignRepo::new();
    assert_eq!(
        other.tree, fixture.tree,
        "identical content, separate projects"
    );
    assert_eq!(other.preflight().status.code(), Some(1));
    assert!(!other.receipt().exists());
}
