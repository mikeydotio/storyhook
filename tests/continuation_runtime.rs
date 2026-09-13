//! Run the provider boundary regressions through real private Git/tmux fixtures.
#[test]
fn continuation_runtime_preserves_owned_process_and_git_evidence() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let result = std::process::Command::new("python3")
        .arg(root.join("plugins/story/tests/test_continuation_runtime.py"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .current_dir(root)
        .output()
        .expect("start private continuation runtime regressions");
    assert!(
        result.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&result.stderr).contains("skipped="),
        "required private runtime coverage was skipped"
    );
}
