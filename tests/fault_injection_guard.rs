//! Wiring fence for SH-534's feature-gated environment guard.
//!
//! Cargo test always enables `fault-injection`, so it cannot exercise the
//! featureless branch through the compiled `story` binary. The pure decision
//! tests cover both branches; this test proves production wires that decision
//! ahead of every side effect by keeping its call first in `main`.

use std::path::PathBuf;

fn checkout_file(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
}

#[test]
fn the_guard_is_the_first_executable_statement_in_main() {
    let source = checkout_file("src/main.rs");
    let main = source
        .split_once("fn main() {")
        .expect("src/main.rs must define main")
        .1;
    let first_statement = main
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("//"))
        .expect("main must contain an executable statement");

    assert_eq!(
        first_statement, "refuse_unsupported_fault_injection_environment();",
        "the guard must remain ahead of argument dispatch and every side effect"
    );
}

#[test]
fn the_guarded_set_is_the_complete_current_feature_gated_environment_set() {
    assert_eq!(
        storyhook::fault_injection_guard::GUARDED_VARIABLES,
        ["STORYHOOK_FAULT", "STORYHOOK_TEST_PANIC"]
    );
}
