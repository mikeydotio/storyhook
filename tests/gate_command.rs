//! The merge-gate command a project's committed pointer names (SH-649).
//!
//! `.storyhook.toml` may carry `[verify] gate = "…"`; absent, the gate is
//! `GateCommand::DEFAULT`. The value is an **argv**, never a shell string:
//! every hop from the daemon to `merge-watch.sh --speculative-run` execs it
//! word for word, so a shell metacharacter in it would be passed literally
//! and never do what its author meant. The rule is a positive allowlist and a
//! value outside it is refused **by name** — the offending character and the
//! word it sits in — the SH-357 doctrine applied to a config value. An
//! unknown key under `[verify]` is refused the same way: a key that lands
//! nowhere must not silently run the default.
//!
//! The reader never fails open. A pointer that cannot be parsed surfaces
//! `read_pointer`'s own error naming the file, because a verifier that ran
//! `make test` over a typo would certify a tree against a gate nobody chose.

use std::path::Path;
use storyhook::error::AppError;
use storyhook::service::gate_command::{GateCommand, gate_command_for};
use storyhook::service::project::pointer_path;
use storyhook_test_support::scratch_dir;

/// A complete pointer — `schema`, `uuid`, `prefix` are required by
/// `read_pointer`, so a fixture that wrote only `[verify]` would fail to parse
/// for the wrong reason and pass a refusal test vacuously.
fn write_pointer(root: &Path, tail: &str) {
    std::fs::write(
        pointer_path(root),
        format!(
            "schema = 1\nuuid = \"291ea25f-3363-4b5d-9051-66636c1066f9\"\nprefix = \"SH\"\n{tail}"
        ),
    )
    .expect("writing the fixture pointer");
}

fn argv(command: &GateCommand) -> Vec<&str> {
    command.argv().iter().map(String::as_str).collect()
}

fn refusal(value: &str) -> String {
    GateCommand::parse(value).expect_err("must be refused")
}

#[test]
fn the_default_is_make_test_as_two_argv_words() {
    let command = GateCommand::parse(GateCommand::DEFAULT).expect("the default parses");
    assert_eq!(argv(&command), ["make", "test"]);
    assert_eq!(command.display(), "make test");
}

#[test]
fn a_plain_argv_is_split_on_spaces_and_kept_verbatim() {
    let cases: [(&str, &[&str]); 6] = [
        ("make test-full", &["make", "test-full"]),
        ("cargo test --workspace", &["cargo", "test", "--workspace"]),
        ("npm run test:ci", &["npm", "run", "test:ci"]),
        (
            "./scripts/gate.sh -v VERBOSE=1",
            &["./scripts/gate.sh", "-v", "VERBOSE=1"],
        ),
        ("make  test", &["make", "test"]),
        (" make test ", &["make", "test"]),
    ];
    for (value, expected) in cases {
        let command = GateCommand::parse(value)
            .unwrap_or_else(|reason| panic!("`{value}` must parse: {reason}"));
        assert_eq!(argv(&command), expected, "`{value}`");
    }
    assert_eq!(
        GateCommand::parse("make  test").unwrap().display(),
        "make test",
        "display is the normalized single-space form"
    );
}

#[test]
fn every_shell_metacharacter_is_refused_naming_it_and_its_word() {
    let cases = [
        ("make test && true", '&', "&&"),
        ("make test | tee log", '|', "|"),
        ("make test; rm -rf /", ';', "test;"),
        ("make test > log", '>', ">"),
        ("make test < in", '<', "<"),
        ("echo $HOME", '$', "$HOME"),
        ("make `date`", '`', "`date`"),
        ("make \"quoted arg\"", '"', "\"quoted"),
        ("make 'quoted'", '\'', "'quoted'"),
        ("cat *.rs", '*', "*.rs"),
        ("cat ?.rs", '?', "?.rs"),
        ("cat [a].rs", '[', "[a].rs"),
        ("echo {a,b}", '{', "{a,b}"),
        ("cat ~/x", '~', "~/x"),
        ("make test #comment", '#', "#comment"),
        ("make test!", '!', "test!"),
        ("make (test)", '(', "(test)"),
        ("make test\\", '\\', "test\\"),
        ("make\ttest", '\t', "make\ttest"),
        ("make test\n", '\n', "test\n"),
        ("make tëst", 'ë', "tëst"),
    ];
    for (value, character, word) in cases {
        let reason = refusal(value);
        assert!(
            reason.contains(&format!("`{character}`")),
            "`{value:?}` must name the character {character:?}: {reason}"
        );
        assert!(
            reason.contains(&format!("`{word}`")),
            "`{value:?}` must name the word {word:?}: {reason}"
        );
        assert!(
            reason.contains("never through a shell"),
            "the remedy is part of the refusal: {reason}"
        );
    }
}

#[test]
fn an_empty_gate_and_a_flag_with_no_command_are_refused() {
    for value in ["", "   "] {
        let reason = GateCommand::parse(value).expect_err("blank must be refused");
        assert!(reason.contains("names no command"), "`{value:?}`: {reason}");
    }
    let reason = refusal("--workspace cargo test");
    assert!(
        reason.contains("`--workspace`") && reason.contains("must be a command"),
        "{reason}"
    );
}

#[test]
fn no_pointer_no_table_and_no_key_all_mean_the_default() {
    let absent = scratch_dir();
    let command = gate_command_for(absent.path()).expect("no pointer at all");
    assert_eq!(command.display(), GateCommand::DEFAULT);

    let no_table = scratch_dir();
    write_pointer(
        no_table.path(),
        "\n[github]\napi_url = \"https://api.example.test\"\n",
    );
    let command = gate_command_for(no_table.path()).expect("a pointer with no [verify]");
    assert_eq!(command.display(), GateCommand::DEFAULT);

    let no_key = scratch_dir();
    write_pointer(no_key.path(), "\n[verify]\n");
    let command = gate_command_for(no_key.path()).expect("an empty [verify] table");
    assert_eq!(command.display(), GateCommand::DEFAULT);
}

#[test]
fn a_configured_gate_is_read_from_the_checkouts_pointer() {
    let root = scratch_dir();
    write_pointer(root.path(), "\n[verify]\ngate = \"make test-full\"\n");
    let command = gate_command_for(root.path()).expect("a configured gate");
    assert_eq!(argv(&command), ["make", "test-full"]);
}

fn validation_message(result: Result<GateCommand, AppError>, root: &Path) -> String {
    match result {
        Err(AppError::Validation(message)) => {
            assert!(
                message.contains("[verify].gate"),
                "the refusal names the key: {message}"
            );
            assert!(
                message.contains(&pointer_path(root).display().to_string()),
                "the refusal names the file: {message}"
            );
            message
        }
        other => panic!("expected a validation refusal, got {other:?}"),
    }
}

#[test]
fn a_key_that_lands_nowhere_is_refused_rather_than_defaulted() {
    let root = scratch_dir();
    write_pointer(root.path(), "\n[verify]\ngaet = \"make test\"\n");
    let message = validation_message(gate_command_for(root.path()), root.path());
    assert!(message.contains("gaet"), "names the unknown key: {message}");
}

#[test]
fn a_gate_that_is_not_a_string_is_refused() {
    let root = scratch_dir();
    write_pointer(root.path(), "\n[verify]\ngate = 3\n");
    validation_message(gate_command_for(root.path()), root.path());
}

#[test]
fn a_verify_value_that_is_not_a_table_is_refused() {
    let root = scratch_dir();
    write_pointer(root.path(), "\nverify = \"make test\"\n");
    validation_message(gate_command_for(root.path()), root.path());
}

#[test]
fn a_gate_that_is_not_a_plain_argv_is_refused_by_name_from_the_pointer() {
    let root = scratch_dir();
    write_pointer(
        root.path(),
        "\n[verify]\ngate = \"make test && rm -rf /\"\n",
    );
    let message = validation_message(gate_command_for(root.path()), root.path());
    assert!(message.contains("`&`"), "names the character: {message}");
    assert!(
        message.contains("`make test && rm -rf /`"),
        "names the value: {message}"
    );
}

#[test]
fn a_broken_pointer_never_falls_open_to_the_default() {
    let root = scratch_dir();
    std::fs::write(pointer_path(root.path()), "schema = \n").expect("writing a broken pointer");
    match gate_command_for(root.path()) {
        Err(error) => assert!(
            error
                .to_string()
                .contains(&pointer_path(root.path()).display().to_string()),
            "the error names the file: {error}"
        ),
        Ok(command) => panic!("a broken pointer must not read as `{}`", command.display()),
    }
}

#[test]
fn a_hand_authored_verify_table_survives_a_pointer_rewrite() {
    use storyhook::service::project::{read_pointer, write_pointer as write_typed};
    let root = scratch_dir();
    write_pointer(root.path(), "\n[verify]\ngate = \"make test-full\"\n");
    let pointer = read_pointer(root.path()).expect("reads").expect("exists");
    write_typed(root.path(), &pointer).expect("rewriting the pointer");
    let command = gate_command_for(root.path()).expect("the table survived");
    assert_eq!(command.display(), "make test-full");
}
