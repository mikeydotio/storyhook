//! The old permanent-deletion spelling is retired by SH-498. It is a refusal,
//! not an alias, so scripts cannot silently keep relying on an obsolete
//! lifecycle model.

use storyhook_test_support::TestEnv;

#[test]
fn purge_refuses_and_names_delete_with_or_without_force() {
    let project = TestEnv::shared()
        .project()
        .seed_story("Created in error")
        .build();

    for args in [
        ["purge", "SH-1"].as_slice(),
        ["purge", "SH-1", "--force"].as_slice(),
    ] {
        project
            .story()
            .args(args)
            .assert()
            .failure()
            .code(2)
            .stderr(predicates::str::contains(
                "`story purge` is retired; use `story delete <id> [--force]`",
            ));
    }

    project.run(&["show", "SH-1"]).success();
}
