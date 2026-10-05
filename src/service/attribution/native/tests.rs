use super::*;
mod proof;
use std::{
    fs,
    process::{Command, Stdio},
};

pub(super) const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/scripts/tests/attribution_native_fixture.py"
);
const TEST: &str = "#[test] fn answer() { assert_eq!(subject::answer(), 42); }\n";
const PATIENCE: Duration = Duration::from_millis(MAX_DIAGNOSIS_MS);

struct Fixture {
    directory: tempfile::TempDir,
    broker: storyhook_test_support::ChildGuard,
    config: std::path::PathBuf,
    base: String,
}
impl Fixture {
    fn new(fixture: bool) -> Self {
        let directory = tempfile::tempdir_in("/tmp").unwrap();
        let config = directory.path().join("authority.json");
        let mut broker = storyhook_test_support::ChildGuard::spawn(
            Command::new("python3")
                .args(["-B", FIXTURE, "serve"])
                .arg(&config)
                .stdin(Stdio::piped()),
        )
        .unwrap();
        let deadline = Instant::now() + storyhook_test_support::load_grace::graced_now(PATIENCE);
        while !config.exists() {
            assert!(broker.try_wait().is_none(), "broker exited before startup");
            assert!(
                Instant::now() < deadline,
                "broker startup exceeded fixture patience"
            );
            // Fixture broker samples every 20 ms; this only observes its readiness.
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut f = Self {
            directory,
            broker,
            config,
            base: String::new(),
        };
        f.git(&["init", "-q", "--template=", "-b", "fixture"]);
        f.git(&["config", "user.name", "Fixture"]);
        f.git(&["config", "user.email", "fixture@localhost"]);
        f.git(&["config", "commit.gpgsign", "false"]);
        storyhook_test_support::approve_fixture_identity(
            f.directory.path(),
            "Fixture",
            "fixture@localhost",
        );
        f.write(
            "Cargo.toml",
            "[package]\nname='subject'\nversion='0.1.0'\nedition='2021'\nbuild=false\n",
        );
        f.write(
            "Cargo.lock",
            "version=4\n[[package]]\nname='subject'\nversion='0.1.0'\n",
        );
        f.write(
            "src/lib.rs",
            if fixture {
                "pub fn answer() -> &'static str { \"valid\" }\n"
            } else {
                "pub fn answer() -> u32 { 42 }\n"
            },
        );
        f.write("tests/contract.rs", if fixture {"#[test] fn answer() { assert_eq!(include_str!(\"../fixtures/value.txt\"), subject::answer()); }\n"} else {TEST});
        f.write("fixtures/value.txt", "valid");
        // Broker evidence is outside the source tree.
        f.git(&[
            "add",
            "Cargo.toml",
            "Cargo.lock",
            "src",
            "tests",
            "fixtures",
        ]);
        f.git(&["commit", "-qm", "base"]);
        f.base = f.git(&["rev-parse", "HEAD"]);
        if fixture {
            f.write("fixtures/value.txt", "broken");
        } else {
            f.write("src/lib.rs", "pub fn answer() -> u32 { 41 }\n");
        }
        f.git(&["add", "src", "fixtures"]);
        f.git(&["commit", "-qm", "candidate"]);
        f
    }
    fn write(&self, path: &str, data: &str) {
        let path = self.directory.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, data).unwrap();
    }
    fn git(&self, args: &[&str]) -> String {
        let result = crate::env::git_env::command(self.directory.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap().trim().into()
    }
    fn comparison(&self) -> NativeRustComparison {
        let mut native = NativeRustComparison::prepare(
            self.directory.path(),
            &crate::store::GateInputs {
                head: Some(self.git(&["rev-parse", "HEAD"])),
                base: Some(self.base.clone()),
                tree: Some(self.git(&["rev-parse", "HEAD^{tree}"])),
                ..Default::default()
            },
            RustCase::new(
                "subject",
                RustTarget::Integration("contract".into()),
                "answer",
            )
            .unwrap(),
            TreeIntervention::Unchanged,
            Environment::at(self.directory.path()),
            Instant::now() + storyhook_test_support::load_grace::graced_now(PATIENCE),
            &Cancellation::default(),
        )
        .unwrap();
        native.fixture = Some(self.config.clone());
        native
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.broker.take_stdin();
        self.broker.wait_within(
            storyhook_test_support::load_grace::graced_now(PATIENCE),
            || "fixture broker cleanup".into(),
        );
    }
}

#[test]
fn actual_behavior_and_fixture_comparisons_retain_four_native_observations() {
    for fixture in [false, true] {
        let f = Fixture::new(fixture);
        let mut native = f.comparison();
        let evidence = proof::Evidence::new();
        let mut signatures = vec![];
        let mut environment = None;
        for (index, side) in [
            ProbeSide::Candidate,
            ProbeSide::Control,
            ProbeSide::Control,
            ProbeSide::Candidate,
        ]
        .into_iter()
        .enumerate()
        {
            let id = format!("probe-{index}");
            let journal = f.directory.path().join(format!("{id}.journal"));
            fs::write(&journal, "").unwrap();
            let output = f.directory.path().join(&id);
            if index == 1 {
                for (attempt, execution, request) in [
                    ("foreign", "new", "foreign-request"),
                    ("attempt", "probe-0", "duplicate-execution"),
                    ("attempt", "new", "probe-0"),
                ] {
                    assert!(
                        native
                            .execute(
                                side,
                                &NativeProbeBinding {
                                    project: "fixture",
                                    attempt,
                                    execution,
                                    generation: evidence.generation(),
                                    request,
                                    journal: &journal,
                                    output: &output,
                                    termination_grace: Duration::from_secs(1),
                                }
                            )
                            .is_err()
                    );
                    assert!(!output.exists(), "invalid identity launched a probe");
                }
            }
            let result = native
                .execute(
                    side,
                    &NativeProbeBinding {
                        project: "fixture",
                        attempt: "attempt",
                        execution: &id,
                        generation: evidence.generation(),
                        request: &id,
                        journal: &journal,
                        output: &output,
                        termination_grace: Duration::from_secs(1),
                    },
                )
                .unwrap();
            assert_eq!(result.executions, 1, "{result:?}");
            assert!(result.cleanup_complete);
            let observed = result.environment.clone().unwrap();
            assert!(observed.supported);
            let equivalent = (
                observed.toolchain,
                observed.fixtures,
                observed.resource_policy,
            );
            if let Some(expected) = &environment {
                assert_eq!(expected, &equivalent);
            } else {
                environment = Some(equivalent);
            }
            if side == ProbeSide::Control {
                assert_eq!(result.outcome, ProbeOutcome::Passed);
            } else if let ProbeOutcome::Failed { signature } = result.outcome {
                signatures.push(signature);
            } else {
                panic!("{result:?}");
            }
            assert!(output.join("run.stdout").is_file());
        }
        assert_eq!(signatures.len(), 2);
        assert_eq!(signatures[0], signatures[1]);
        assert_eq!(native.observations.len(), 4);
        proof::exercise(native, &f, evidence, fixture);
    }
}

#[test]
fn revoked_or_changed_inputs_never_start_a_native_probe() {
    let f = Fixture::new(false);
    for fault in ["order", "cancel", "deadline", "input"] {
        let mut native = f.comparison();
        let side = if fault == "order" {
            ProbeSide::Control
        } else {
            ProbeSide::Candidate
        };
        match fault {
            "cancel" => native.cancellation.cancel(),
            "deadline" => native.deadline = Instant::now(),
            "input" => fs::write(
                native.candidate.path().join("src/lib.rs"),
                "pub fn answer() -> u32 { 42 }",
            )
            .unwrap(),
            _ => {}
        }
        let output = f.directory.path().join(fault);
        let journal = f.directory.path().join("unopened-journal");
        assert!(
            native
                .execute(
                    side,
                    &NativeProbeBinding {
                        project: "fixture",
                        attempt: "a",
                        execution: "e",
                        generation: 7,
                        request: fault,
                        journal: &journal,
                        output: &output,
                        termination_grace: Duration::from_secs(1),
                    }
                )
                .is_err()
        );
        assert!(!output.exists());
        assert!(native.observations.is_empty());
        native.close().unwrap();
    }
}

#[test]
fn uncalibrated_workload_retains_an_unavailable_native_observation() {
    let f = Fixture::new(false);
    let mut config: serde_json::Value =
        serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
    config["policy"]["workloads"]
        .as_object_mut()
        .unwrap()
        .remove("causal-rust");
    let config_path = f.directory.path().join("missing-workload.json");
    fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let mut native = f.comparison();
    native.fixture = Some(config_path);
    let output = f.directory.path().join("unsupported");
    let journal = f.directory.path().join("unsupported-journal");
    fs::write(&journal, "").unwrap();
    let result = native
        .execute(
            ProbeSide::Candidate,
            &NativeProbeBinding {
                project: "fixture",
                attempt: "a",
                execution: "e",
                generation: 7,
                request: "uncalibrated",
                journal: &journal,
                output: &output,
                termination_grace: Duration::from_secs(1),
            },
        )
        .unwrap();
    assert_eq!(result.executions, 0);
    assert!(
        matches!(result.outcome, ProbeOutcome::Unavailable { ref detail } if detail.contains("workload-missing")),
        "{result:?}"
    );
    assert!(result.environment.is_none());
    assert!(
        !output.join("observation.json").exists(),
        "compiler pipeline started without measured workload"
    );
    assert_eq!(native.observations.len(), 1);
    native.close().unwrap();
}
