//! The retired member directory must not return as a command.
use storyhook_test_support::{TestEnv, scratch_dir};

#[test]
fn member_and_assignment_commands_are_unavailable() {
    let dir = scratch_dir();
    for args in [
        vec!["member", "add", "Ada <ada@example.com>"],
        vec!["member", "add", "-g", "ada"],
        vec!["assign", "SH-1", "ada"],
    ] {
        TestEnv::shared()
            .story(dir.path())
            .args(args)
            .assert()
            .failure()
            .code(2);
    }
}
