//! Contracts for file-isolation planning, durable evidence and scheduled execution.

use std::process::Command;
use std::time::Duration;

use storyhook_test_support::{load_grace, run_bounded};

#[test]
fn file_isolation_contracts() {
    let mut command = Command::new("python3");
    command
        .args(["-B", "tests/support/e2e_isolation.py"])
        .current_dir(env!("CARGO_MANIFEST_DIR"));
    let output = run_bounded(
        command,
        "file isolation contracts",
        load_grace::graced_now(Duration::from_secs(120)),
    );
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn isolation_watch_contracts() {
    let mut command = Command::new("python3");
    command
        .args(["-B", "tests/support/e2e_isolation_watch.py"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("STORY_BIN", storyhook_test_support::story_binary());
    let output = run_bounded(
        command,
        "isolation watcher contracts",
        load_grace::graced_now(Duration::from_secs(180)),
    );
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
