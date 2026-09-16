//! Substitute remote endpoints while retaining real Git protocol behavior.
use std::path::Path;
use std::process::Command;

/// Installs a Git executable adapter mapping explicit HTTPS URLs to local remotes.
/// Local config reads and URL-rewrite validation always reach unmodified Git.
/// Only network commands are redirected, after production routing has run.
pub fn install_git_endpoint(bin: &Path, mappings: &[(&str, &Path)]) {
    let mapping: std::collections::BTreeMap<_, _> = mappings.iter().copied().collect();
    let output = Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/test-git-endpoint.py"))
        .arg(bin)
        .arg(serde_json::to_string(&mapping).unwrap())
        .output()
        .expect("installing Git endpoint adapter");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
