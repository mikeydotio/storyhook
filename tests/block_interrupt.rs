//! Native provider interruption must release an owned gate only after quiescence.
use std::path::Path;
use std::process::Command;
use storyhook_test_support::scratch_dir;

#[test]
fn native_interrupt_quiesces_gate_and_preserves_session() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = scratch_dir();
    let init = storyhook::env::git_env::command(scratch.path())
        .args(["init", "-q"])
        .output()
        .unwrap();
    assert!(init.status.success(), "{:?}", init);
    let output = Command::new("python3")
        .arg(root.join("tests/support/block_interrupt.py"))
        .arg(root)
        .arg(scratch.path())
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
