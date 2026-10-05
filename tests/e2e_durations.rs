//! SH-812: duration scheduling and receipt validation without a Node dependency.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

#[test]
fn duration_cache_and_weighted_planner_contracts() {
    let mut command = Command::new("python3");
    command
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/tests/test_e2e_durations.py"))
        .args(["PlannerTests", "CacheTests", "RunnerJoinTests"])
        .env("PYTHONDONTWRITEBYTECODE", "1");
    let output = storyhook_test_support::run_bounded(
        command,
        "e2e duration history and planner contracts",
        storyhook_test_support::load_grace::graced_now(Duration::from_secs(120)),
    );
    assert!(output.status.success(), "{output:?}");
}
