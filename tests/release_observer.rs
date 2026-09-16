//! Exercises the release observer against real disposable Git repositories.

use std::process::Command;

#[test]
fn release_observer_contracts() {
    let output = Command::new("python3")
        .args(["-B", "tests/support/release_observer.py"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("STORY_BIN", env!("CARGO_BIN_EXE_story"))
        .output()
        .expect("run release observer contracts");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
