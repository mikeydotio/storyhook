use super::*;

fn selected() -> RustCase {
    RustCase::new(
        "subject",
        RustTarget::Integration("contract".into()),
        "answer",
    )
    .unwrap()
}

#[test]
fn closure_requires_an_unconditional_builtin_assertion_and_literal_owned_inputs() {
    let library = "pub fn answer() -> u32 { 42 }";
    for test in [
        "#[test] #[ignore] fn answer() { assert_eq!(subject::answer(), 42); }",
        "#[test] #[should_panic] fn answer() { assert_eq!(subject::answer(), 42); }",
        "#[test] fn answer() { if false { assert_eq!(subject::answer(), 42); } }",
        "#[test] fn answer() { return; assert_eq!(subject::answer(), 42); }",
        "#[test] fn answer() { assert_eq!(subject::answer(), 42, std::env::var(\"X\").unwrap()); }",
        "#[test] fn answer() { assert_eq!(include_str!(\"/tmp/outside\"), \"x\"); }",
        "#[test] fn answer() { assert_eq!(include_str!(\"../fixtures/../../outside\"), \"x\"); }",
        "#[test] fn answer() { assert_eq!(include_str!(concat!(\"../fixtures/\", \"value\")), \"x\"); }",
        "#[test] fn answer() { assert_eq!(std::process::id(), 42); }",
        "#[test] fn answer() { assert_eq!(subject::answer(), { return; 42 }); }",
        "#![no_std]\n#[test] fn answer() { assert_eq!(subject::answer(), 42); }",
        "#[test] fn answer() { assert_eq!(subject::answer(), 42); } fn helper() {}",
        "#[test] async fn answer() { assert_eq!(subject::answer(), 42); }",
    ] {
        assert!(
            syntax::check(library, test, &selected()).is_err(),
            "accepted {test}"
        );
    }
}

#[test]
fn closure_checks_unused_functions_types_attributes_and_all_branches() {
    let assertion = "#[test] fn answer() { assert_eq!(subject::answer(), 42); }";
    for library in [
        "pub fn answer() -> u32 { 42 } fn unused() -> u32 { std::process::id() }",
        "pub fn answer() -> u32 { if false { std::process::id() } else { 42 } }",
        "pub fn answer() -> u32 { let callback = other; callback() } fn other() -> u32 { 42 }",
        "#[cfg(unix)] pub fn answer() -> u32 { 42 }",
        "pub fn answer<T>() -> u32 { 42 }",
        "pub fn answer() -> u32 { 42 } fn consume(x: impl Drop) {}",
        "pub fn answer() -> u32 { 42 } fn raw(x: *const u32) {}",
        "pub fn answer() -> u32 { 42 } static INPUT: u32 = 1;",
        "use std::process; pub fn answer() -> u32 { process::id() }",
        "pub fn answer() -> u32 { #[cfg(unix)] 42 }",
        "pub fn answer() -> u32 { let mut x = 42; x += 1; x }",
    ] {
        assert!(
            syntax::check(library, assertion, &selected()).is_err(),
            "accepted {library}"
        );
    }
    let valid = "pub fn answer() -> u32 { let x: u32 = helper(40); if x == 40 { x + 2 } else { 0 } } fn helper(x: u32) -> u32 { return x; }";
    assert!(
        syntax::check(valid, assertion, &selected())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn dependency_and_compiler_extensibility_are_not_hidden_in_manifests_or_locks() {
    let package = "[package]\nname='subject'\nversion='0.1.0'\nedition='2021'\nbuild=false\n";
    let lock = "version=4\n[[package]]\nname='subject'\nversion='0.1.0'\n";
    assert!(manifest(package, lock, "subject").is_ok());
    for addition in [
        "[dependencies]\n",
        "[workspace]\n",
        "[features]\ndefault=[]\n",
        "[profile.test]\npanic='abort'\n",
        "[lib]\nharness=false\n",
        "[[test]]\nname='contract'\npath='elsewhere.rs'\n",
    ] {
        assert!(
            manifest(&format!("{package}{addition}"), lock, "subject").is_err(),
            "accepted {addition}"
        );
    }
    for changed in [
        package.replace("build=false", "build='custom.rs'"),
        package.replace("name='subject'", "name='other'"),
        package.replace("edition='2021'", "edition.workspace=true"),
    ] {
        assert!(
            manifest(&changed, lock, "subject").is_err(),
            "accepted {changed}"
        );
    }
    for changed in [
        format!("{lock}dependencies=['foreign']\n"),
        format!("{lock}[[package]]\nname='foreign'\nversion='1.0.0'\n"),
        lock.replace("version='0.1.0'", "version='0.2.0'"),
        lock.replace("version=4", "version=9"),
    ] {
        assert!(
            manifest(package, &changed, "subject").is_err(),
            "accepted {changed}"
        );
    }
}
