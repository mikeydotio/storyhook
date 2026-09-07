//! Explicit identity policy for repositories exercising the production Git hooks.

use std::path::Path;

/// Approve the caller's fixture identity in that fixture's local Git configuration.
///
/// The sanitized Git constructor prevents inherited repository targeting from
/// writing this approval into the developer's shared configuration (SH-572).
pub fn approve_fixture_identity(repo: &Path, name: &str, email: &str) {
    for (field, value) in [
        ("name", name),
        ("email", email),
        ("role", "both"),
        ("reason", "Isolated real-Git test fixture identity"),
    ] {
        let output = storyhook::env::git_env::command(repo)
            .args([
                "config",
                "--local",
                &format!("storyhookIdentity.fixture.{field}"),
                value,
            ])
            .output()
            .expect("configuring fixture identity approval");
        assert!(
            output.status.success(),
            "fixture identity approval failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
