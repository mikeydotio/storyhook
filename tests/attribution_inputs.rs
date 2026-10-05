//! Native trees plus syntax must prove a closed assertion and its isolated intervention.
use std::{
    fs,
    time::{Duration, Instant},
};
use storyhook::{
    daemon::verification::VerificationCancellation,
    service::attribution::{
        PreparedTrees, ProbeSide, RustCase, RustInputs, RustIntervention, RustTarget,
        TreeIntervention,
    },
    store::GateInputs,
};

const PATIENCE: Duration = Duration::from_secs(300);
const MANIFEST: &str = "[package]\nname='subject'\nversion='0.1.0'\nedition='2021'\nbuild=false\n";
const TEST: &str = "#[test] fn answer() { assert_eq!(subject::answer(), 42); }\n";

struct Pair {
    root: tempfile::TempDir,
    base: String,
}
impl Pair {
    fn new(test: &str, library: &str) -> Self {
        let mut pair = Self {
            root: tempfile::tempdir_in("/tmp").unwrap(),
            base: String::new(),
        };
        pair.git(&["init", "--quiet", "--initial-branch=fixture"]);
        pair.git(&["config", "user.name", "Fixture"]);
        pair.git(&["config", "user.email", "fixture@localhost"]);
        pair.git(&["config", "commit.gpgsign", "false"]);
        storyhook_test_support::approve_fixture_identity(
            pair.root.path(),
            "Fixture",
            "fixture@localhost",
        );
        pair.write("Cargo.toml", MANIFEST);
        pair.write(
            "Cargo.lock",
            "version = 4\n[[package]]\nname='subject'\nversion='0.1.0'\n",
        );
        pair.write("src/lib.rs", library);
        pair.write("tests/contract.rs", test);
        pair.write("fixtures/value.txt", "valid");
        pair.base = pair.commit();
        pair
    }
    fn git(&self, args: &[&str]) -> String {
        let out = storyhook::env::git_env::command(self.root.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().into()
    }
    fn write(&self, path: &str, value: &str) {
        let path = self.root.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, value).unwrap();
    }
    fn commit(&self) -> String {
        self.git(&["add", "."]);
        self.git(&["commit", "--quiet", "-m", "fixture"]);
        self.git(&["rev-parse", "HEAD"])
    }
    fn validate(&self) -> Result<RustInputs, String> {
        self.validate_with(TreeIntervention::Unchanged)
    }
    fn validate_with(&self, intervention: TreeIntervention) -> Result<RustInputs, String> {
        let head = self.commit();
        let trees = PreparedTrees::prepare(
            self.root.path(),
            &GateInputs {
                head: Some(head),
                base: Some(self.base.clone()),
                tree: Some(self.git(&["rev-parse", "HEAD^{tree}"])),
                ..Default::default()
            },
            &["Cargo.lock".into()],
            intervention,
            Instant::now() + storyhook_test_support::load_grace::graced_now(PATIENCE),
            &VerificationCancellation::default(),
        )
        .unwrap();
        let candidate = trees.materialize(ProbeSide::Candidate).unwrap();
        let control = trees.materialize(ProbeSide::Control).unwrap();
        let result = RustInputs::validate(
            &candidate,
            &control,
            &RustCase::new(
                "subject",
                RustTarget::Integration("contract".into()),
                "answer",
            )
            .unwrap(),
        );
        candidate.close().unwrap();
        control.close().unwrap();
        trees.close().unwrap();
        result
    }
}

#[test]
fn a_candidate_only_detector_requires_the_exact_native_transplant() {
    let mut pair = Pair::new(TEST, "pub fn answer() -> u32 { 42 }");
    fs::remove_file(pair.root.path().join("tests/contract.rs")).unwrap();
    pair.base = pair.commit();
    pair.write("tests/contract.rs", TEST);
    pair.write("src/lib.rs", "pub fn answer() -> u32 { 41 }");
    let inputs = pair
        .validate_with(TreeIntervention::Transplant(vec![
            "tests/contract.rs".into(),
        ]))
        .unwrap();
    assert_eq!(inputs.intervention(), &RustIntervention::Behavior);
}

#[test]
fn native_primitive_behavior_contrast_retains_the_assertion() {
    let pair = Pair::new(
        TEST,
        "pub fn answer() -> u32 { helper(40) + 2 } fn helper(x: u32) -> u32 { x }",
    );
    pair.write(
        "src/lib.rs",
        "pub fn answer() -> u32 { helper(40) + 1 } fn helper(x: u32) -> u32 { x }",
    );
    let inputs = pair.validate().unwrap();
    assert_eq!(inputs.intervention(), &RustIntervention::Behavior);
    assert!(inputs.detector().starts_with("rust-inputs-v1:"));
}

#[test]
fn native_literal_fixture_contrast_keeps_production_and_assertion_fixed() {
    let pair = Pair::new(
        "#[test] fn answer() { assert_eq!(include_str!(\"../fixtures/value.txt\"), subject::answer()); }",
        "pub fn answer() -> &'static str { \"valid\" }",
    );
    pair.write("fixtures/value.txt", "broken");
    let inputs = pair.validate().unwrap();
    assert_eq!(
        inputs.intervention(),
        &RustIntervention::Fixture(vec!["fixtures/value.txt".into()])
    );
}

#[test]
fn changed_assertions_manifests_and_unreferenced_inputs_are_not_controls() {
    for (path, value) in [
        (
            "tests/contract.rs",
            "#[test] fn answer() { assert_eq!(subject::answer(), 41); }",
        ),
        (
            "Cargo.toml",
            "[package]\nname='subject'\nversion='0.1.0'\nedition='2024'\nbuild=false\n",
        ),
        ("fixtures/value.txt", "unreferenced change"),
        ("README.md", "unclassified input"),
    ] {
        let pair = Pair::new(TEST, "pub fn answer() -> u32 { 42 }");
        pair.write(path, value);
        assert!(pair.validate().is_err(), "accepted {path}");
    }
}

#[test]
fn hidden_execution_sources_and_mixed_interventions_stay_unsupported() {
    for (path, value) in [
        ("build.rs", "fn main() {}"),
        (".cargo/config.toml", "[env]\nHIDDEN='1'"),
        ("src/other.rs", "pub fn hidden() {}"),
        ("rust-toolchain.toml", "[toolchain]\nchannel='nightly'"),
    ] {
        let pair = Pair::new(TEST, "pub fn answer() -> u32 { 42 }");
        pair.write("src/lib.rs", "pub fn answer() -> u32 { 41 }");
        pair.write(path, value);
        assert!(pair.validate().is_err(), "accepted {path}");
    }
    let pair = Pair::new(
        "#[test] fn answer() { assert_eq!(include_str!(\"../fixtures/value.txt\"), subject::answer()); }",
        "pub fn answer() -> &'static str { \"valid\" }",
    );
    pair.write(
        "src/lib.rs",
        "pub fn answer() -> &'static str { \"different\" }",
    );
    pair.write("fixtures/value.txt", "broken");
    assert!(pair.validate().is_err());
}

#[test]
fn environmental_recursive_and_extensible_rust_is_not_deterministic_input_proof() {
    for library in [
        "pub fn answer() -> u32 { std::process::id() }",
        "pub fn answer() -> u32 { std::env::var(\"ANSWER\").unwrap().parse().unwrap() }",
        "pub fn answer() -> u32 { answer() }",
        "pub fn answer() -> u32 { other() } fn other() -> u32 { answer() }",
        "pub fn answer() -> u32 { loop {} }",
        "pub fn answer() -> u32 { unsafe { 41 } }",
        "pub fn answer() -> u32 { include!(\"other.rs\") }",
        "pub fn answer() -> u32 { env!(\"ANSWER\").parse().unwrap() }",
        "pub fn answer() -> u32 { 41 } #[no_mangle] pub extern \"C\" fn hidden() {}",
        "macro_rules! hidden { () => { 41 } } pub fn answer() -> u32 { hidden!() }",
        "pub fn answer() -> u32 { let f = || 41; f() }",
    ] {
        let pair = Pair::new(TEST, "pub fn answer() -> u32 { 42 }");
        pair.write("src/lib.rs", library);
        assert!(pair.validate().is_err(), "accepted {library}");
    }
}
