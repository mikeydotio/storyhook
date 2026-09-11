//! SH-685: exercise the real Cargo collector and its stream boundaries.

use std::process::Command;

#[test]
fn compiler_collection_preserves_provenance_and_cargo_execution() {
    let output = Command::new("python3")
        .args(["-B", "tests/support/cargo_diagnostics.py"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run compiler collector contracts");
    assert!(
        output.status.success(),
        "collector contracts failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
