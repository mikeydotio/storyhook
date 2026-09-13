//! Native provider interruption must release an owned gate only after quiescence.
use std::path::Path;
use std::process::Command;
use storyhook_test_support::{TestEnv, git};

#[test]
fn native_interrupt_quiesces_gate_and_preserves_session() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let env = TestEnv::isolated();
    let project = env.project().with_local_origin().build();
    assert_eq!(project.new_story("native interruption"), "SH-1");
    let worktree = project.path().join("interrupt-lane");
    git(
        &env,
        project.path(),
        &[
            "worktree",
            "add",
            "-b",
            "worktree-SH-1",
            worktree.to_str().unwrap(),
            "HEAD",
        ],
    );
    let mut command = Command::new("python3");
    env.apply(&mut command);
    let output = command
        .arg(root.join("tests/support/block_interrupt.py"))
        .arg(root)
        .arg(&worktree)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
