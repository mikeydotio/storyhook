//! Black-box coverage for reusable gate-leg evidence.
//!
//! The release gate is fail-fast at the Makefile level: if e2e is red, the
//! aggregate `full` receipt is never written. That must not erase successful
//! evidence from earlier, unrelated batteries. These tests provoke the real
//! `scripts/leg.sh --reuse` wrapper in disposable git repositories and count
//! command executions. The fixtures reach the tracked scripts through symlinks,
//! so no receipt is forged, no implementation text is parsed, and no copy can
//! drift from the artifact that ships.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

fn checkout() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

struct Repo {
    root: TempDir,
    path: PathBuf,
}

#[test]
fn tracked_tree_changes_invalidate_binary_consumers_but_preserve_fmt_and_clippy() {
    let repo = Repo::new();
    repo.write("scripts/tracked-tree.sh", "# initial build identity\n");
    repo.git(&["add", "scripts/tracked-tree.sh"]);
    for leg in [
        "fmt",
        "clippy",
        "rust-suite",
        "rust-contracts",
        "build",
        "plugin",
        "e2e",
    ] {
        assert!(repo.run_leg(leg, true).status.success());
        assert!(repo.run_leg(leg, true).status.success());
        assert_eq!(repo.executions(leg), 1, "unchanged {leg} must reuse");
    }

    repo.write("scripts/tracked-tree.sh", "# changed build identity\n");
    for (leg, expected) in [
        ("fmt", 1),
        ("clippy", 1),
        ("rust-suite", 2),
        ("rust-contracts", 2),
        ("build", 2),
        ("plugin", 2),
        ("e2e", 2),
    ] {
        let result = repo.run_leg(leg, true);
        assert!(result.status.success(), "{result:?}");
        assert_eq!(repo.executions(leg), expected, "{leg} dependency mismatch");
    }
}

#[test]
fn compiler_adapter_changes_invalidate_compilation_and_test_evidence() {
    let repo = Repo::new();
    repo.write(
        "scripts/cargo_diagnostics.py",
        "# initial collector contract\n",
    );
    repo.git(&["add", "scripts/cargo_diagnostics.py"]);
    repo.git(&["commit", "-qm", "collector input"]);
    for leg in ["clippy", "build", "rust-suite"] {
        assert!(repo.run_leg(leg, true).status.success());
        assert!(repo.run_leg(leg, true).status.success());
        assert_eq!(repo.executions(leg), 1);
    }
    repo.write(
        "scripts/cargo_diagnostics.py",
        "# changed collector contract\n",
    );
    for leg in ["clippy", "build", "rust-suite"] {
        let result = repo.run_leg(leg, true);
        assert!(result.status.success(), "{result:?}");
        assert_eq!(
            repo.executions(leg),
            2,
            "{leg} reused stale collector evidence"
        );
    }
}

#[test]
fn orchestration_changes_invalidate_every_leg() {
    for input in [
        "scripts/gate-legs.sh",
        "scripts/gate-progress-writer.py",
        "scripts/gate_cost.py",
        "scripts/host-admission.py",
        "scripts/host-admit.py",
        "scripts/host_admission/authority.py",
        "scripts/host_admission/adapter.py",
        "scripts/progress_journal.py",
        "scripts/python-runtime.sh",
        "scripts/python-bin/python3",
    ] {
        let repo = Repo::new();
        repo.replace_with_tracked_copy(input, "\n# initial orchestration contract\n");
        repo.git(&["add", input]);
        repo.git(&["commit", "-qm", "orchestration input"]);
        for leg in [
            "fmt",
            "clippy",
            "rust-suite",
            "rust-contracts",
            "build",
            "plugin",
            "e2e",
        ] {
            assert!(repo.run_leg(leg, true).status.success());
            assert!(repo.run_leg(leg, true).status.success());
            assert_eq!(repo.executions(leg), 1);
        }
        repo.replace_with_tracked_copy(input, "\n# changed orchestration contract\n");
        for leg in [
            "fmt",
            "clippy",
            "rust-suite",
            "rust-contracts",
            "build",
            "plugin",
            "e2e",
        ] {
            let result = repo.run_leg(leg, true);
            assert!(result.status.success(), "{result:?}");
            assert_eq!(
                repo.executions(leg),
                2,
                "{leg} reused stale orchestration evidence after {input} changed"
            );
        }
    }
}

impl Repo {
    fn new() -> Self {
        let root = storyhook_test_support::scratch_dir();
        let path = root.path().join("main");
        fs::create_dir(&path).expect("creating fixture repository");
        let repo = Self { root, path };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.email", "gate-leg@example.test"]);
        repo.git(&["config", "user.name", "Gate Leg Test"]);

        for (path, contents) in [
            ("Cargo.toml", "[package]\nname='fixture'\nversion='0.1.0'\n"),
            ("Makefile", "test:\n\t@true\n"),
            ("src/lib.rs", "pub fn answer() -> u8 { 42 }\n"),
            ("src/web_dashboard.html", "<main>dashboard</main>\n"),
            ("tests/contract.rs", "#[test] fn contract() {}\n"),
            (
                "tests/dashboard_contract.rs",
                "const ROOT: &str = env!(\"CARGO_MANIFEST_DIR\");\n#[test] fn contract() { assert!(!ROOT.is_empty()); }\n",
            ),
            ("e2e/specs/board.spec.ts", "// browser fixture\n"),
        ] {
            repo.write(path, contents);
        }
        fs::create_dir_all(repo.path().join("scripts")).expect("creating fixture scripts/");
        for script in [
            "leg.sh",
            "activity-log.sh",
            "activity-run.py",
            "test_output.py",
            "gate-progress.sh",
            "gate-leg-fingerprint.sh",
            "rust-test-targets.sh",
            "python-runtime.sh",
            "python-bin/python3",
        ] {
            fs::create_dir_all(repo.path().join("scripts").join(script).parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(
                checkout().join("scripts").join(script),
                repo.path().join("scripts").join(script),
            )
            .unwrap_or_else(|e| panic!("linking the tracked {script}: {e}"));
        }
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "fixture"]);
        repo
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.path().join(relative);
        fs::create_dir_all(path.parent().expect("fixture path has parent"))
            .expect("creating fixture parent");
        fs::write(path, contents).expect("writing fixture file");
    }

    /// Makes `relative` a regular file holding the checkout's own copy plus
    /// `suffix`. A fixture script can be a symlink into the real checkout, and
    /// `write` would follow it and edit the tracked original.
    fn replace_with_tracked_copy(&self, relative: &str, suffix: &str) {
        let original = fs::read_to_string(checkout().join(relative))
            .unwrap_or_else(|e| panic!("reading the tracked {relative}: {e}"));
        let path = self.path().join(relative);
        if path.symlink_metadata().is_ok() {
            fs::remove_file(&path).unwrap_or_else(|e| panic!("unlinking fixture {relative}: {e}"));
        }
        self.write(relative, &format!("{original}{suffix}"));
        fs::set_permissions(
            &path,
            fs::metadata(checkout().join(relative))
                .unwrap()
                .permissions(),
        )
        .unwrap();
    }

    fn git(&self, args: &[&str]) -> Output {
        let out = Command::new("git")
            .args(args)
            .current_dir(self.path())
            .output()
            .expect("running git");
        assert!(
            out.status.success(),
            "git {args:?} failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn linked_worktree(&self, name: &str) -> PathBuf {
        let path = self.root.path().join(name);
        let out = Command::new("git")
            .args(["worktree", "add", "-q", "--detach"])
            .arg(&path)
            .arg("HEAD")
            .current_dir(self.path())
            .output()
            .expect("adding linked fixture worktree");
        assert!(
            out.status.success(),
            "adding linked fixture worktree failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        path
    }

    fn counter_at(&self, root: &Path, label: &str) -> PathBuf {
        root.join(format!("{label}.count"))
    }

    fn run_leg(&self, label: &str, succeeds: bool) -> Output {
        self.run_leg_at(self.path(), label, succeeds)
    }

    fn run_leg_at(&self, root: &Path, label: &str, succeeds: bool) -> Output {
        let script = if succeeds {
            "printf x >> \"$1\""
        } else {
            "printf x >> \"$1\"; exit 23"
        };
        Command::new("bash")
            .arg(root.join("scripts/leg.sh"))
            .args(["--reuse", label, "--", "bash", "-c", script, "gate-leg"])
            // The relative counter keeps argv byte-identical across linked
            // worktrees while recording which worktree actually executed.
            .arg(format!("{label}.count"))
            .current_dir(root)
            .output()
            .expect("running reusable leg")
    }

    fn executions(&self, label: &str) -> usize {
        self.executions_at(self.path(), label)
    }

    fn executions_at(&self, root: &Path, label: &str) -> usize {
        fs::read_to_string(self.counter_at(root, label))
            .unwrap_or_default()
            .len()
    }
}

#[test]
fn build_results_are_worktree_local_while_pure_results_remain_shared() {
    let repo = Repo::new();
    let sibling = repo.linked_worktree("sibling");

    let first_build = repo.run_leg("build", true);
    assert!(first_build.status.success(), "first build: {first_build:?}");
    let sibling_build = repo.run_leg_at(&sibling, "build", true);
    assert!(
        sibling_build.status.success(),
        "sibling build: {sibling_build:?}"
    );
    assert_eq!(repo.executions("build"), 1, "the first build did not run");
    assert_eq!(
        repo.executions_at(&sibling, "build"),
        1,
        "a sibling worktree reused build evidence without producing its artifact"
    );

    let first_build_retry = repo.run_leg("build", true);
    assert!(
        first_build_retry.status.success(),
        "first build retry: {first_build_retry:?}"
    );
    assert_eq!(
        repo.executions("build"),
        1,
        "returning to a worktree did not reuse its own build evidence"
    );
    assert!(
        String::from_utf8_lossy(&first_build_retry.stderr).contains("REUSED"),
        "worktree-local reuse was not reported: {}",
        String::from_utf8_lossy(&first_build_retry.stderr)
    );

    let first_fmt = repo.run_leg("fmt", true);
    assert!(first_fmt.status.success(), "first fmt: {first_fmt:?}");
    let sibling_fmt = repo.run_leg_at(&sibling, "fmt", true);
    assert!(sibling_fmt.status.success(), "sibling fmt: {sibling_fmt:?}");
    assert_eq!(repo.executions("fmt"), 1, "the first fmt did not run");
    assert_eq!(
        repo.executions_at(&sibling, "fmt"),
        0,
        "a pure result stopped being reusable across worktrees"
    );
    assert!(
        String::from_utf8_lossy(&sibling_fmt.stderr).contains("REUSED"),
        "cross-worktree pure reuse was not reported: {}",
        String::from_utf8_lossy(&sibling_fmt.stderr)
    );
}

#[test]
fn successful_results_are_reused_until_that_legs_inputs_change() {
    let repo = Repo::new();

    let first = repo.run_leg("fmt", true);
    assert!(first.status.success(), "first run: {first:?}");
    let retry = repo.run_leg("fmt", true);
    assert!(retry.status.success(), "retry: {retry:?}");
    assert_eq!(repo.executions("fmt"), 1, "an unchanged retry reran fmt");
    assert!(
        String::from_utf8_lossy(&retry.stderr).contains("REUSED"),
        "reuse was not reported: {}",
        String::from_utf8_lossy(&retry.stderr)
    );

    repo.write("e2e/specs/board.spec.ts", "// browser-only edit\n");
    let unrelated_edit = repo.run_leg("fmt", true);
    assert!(
        unrelated_edit.status.success(),
        "unrelated edit: {unrelated_edit:?}"
    );
    assert_eq!(
        repo.executions("fmt"),
        1,
        "a browser-only edit invalidated the Rust formatting result"
    );

    repo.write("src/lib.rs", "pub fn answer() -> u8 { 43 }\n");
    let relevant_edit = repo.run_leg("fmt", true);
    assert!(
        relevant_edit.status.success(),
        "relevant edit: {relevant_edit:?}"
    );
    assert_eq!(
        repo.executions("fmt"),
        2,
        "a Rust-source edit did not invalidate the Rust formatting result"
    );
}

#[test]
fn a_failed_leg_never_creates_reusable_evidence() {
    let repo = Repo::new();

    let first = repo.run_leg("e2e", false);
    assert_eq!(first.status.code(), Some(23), "first failure: {first:?}");
    let retry = repo.run_leg("e2e", false);
    assert_eq!(retry.status.code(), Some(23), "second failure: {retry:?}");
    assert_eq!(
        repo.executions("e2e"),
        2,
        "a failed browser result was reused instead of rerun"
    );
}

#[test]
fn a_browser_failure_does_not_invalidate_prior_green_batteries() {
    let repo = Repo::new();

    for label in [
        "fmt",
        "clippy",
        "rust-suite",
        "rust-contracts",
        "build",
        "plugin",
    ] {
        let out = repo.run_leg(label, true);
        assert!(out.status.success(), "seeding {label}: {out:?}");
    }
    let browser = repo.run_leg("e2e", false);
    assert_eq!(
        browser.status.code(),
        Some(23),
        "browser failure: {browser:?}"
    );

    for label in [
        "fmt",
        "clippy",
        "rust-suite",
        "rust-contracts",
        "build",
        "plugin",
    ] {
        let out = repo.run_leg(label, true);
        assert!(out.status.success(), "retrying {label}: {out:?}");
        assert_eq!(
            repo.executions(label),
            1,
            "browser failure invalidated unrelated successful battery {label}"
        );
    }
    assert_eq!(repo.executions("e2e"), 1);
}

#[test]
fn a_browser_edit_reruns_only_browser_and_checkout_contracts() {
    let repo = Repo::new();
    let labels = [
        "fmt",
        "clippy",
        "rust-suite",
        "rust-contracts",
        "build",
        "plugin",
        "e2e",
    ];
    for label in labels {
        let out = repo.run_leg(label, true);
        assert!(out.status.success(), "seeding {label}: {out:?}");
    }

    repo.write("e2e/specs/board.spec.ts", "// edited browser assertion\n");

    for label in labels {
        let out = repo.run_leg(label, true);
        assert!(out.status.success(), "retrying {label}: {out:?}");
        let expected = usize::from(matches!(label, "rust-contracts" | "e2e")) + 1;
        assert_eq!(
            repo.executions(label),
            expected,
            "browser edit invalidated the wrong battery: {label}"
        );
    }
}

#[test]
fn an_e2e_duration_helper_edit_invalidates_browser_evidence() {
    let repo = Repo::new();
    let helper = "scripts/e2e-durations.py";
    repo.write(helper, "# initial duration reader\n");
    repo.git(&["add", helper]);
    for label in ["e2e", "rust-contracts", "rust-suite"] {
        assert!(repo.run_leg(label, true).status.success());
    }
    repo.write(helper, "# changed duration reader\n");
    for (label, expected) in [("e2e", 2), ("rust-contracts", 2), ("rust-suite", 1)] {
        assert!(repo.run_leg(label, true).status.success());
        assert_eq!(repo.executions(label), expected, "{label}");
    }
}

/// The Rust batteries' pool driver (SH-783) decides how every binary runs,
/// so an edit to it must not let a cached Rust verdict stand -- and must not
/// throw away the verdicts it cannot affect.
#[test]
fn a_test_pool_edit_reruns_only_the_rust_batteries() {
    for path in ["scripts/test-pool.py", "scripts/test_discovery.py"] {
        let repo = Repo::new();
        repo.write(path, "# pool fixture\n");
        repo.git(&["add", path]);
        let labels = [
            "fmt",
            "clippy",
            "rust-suite",
            "rust-contracts",
            "build",
            "plugin",
            "e2e",
        ];
        for label in labels {
            let out = repo.run_leg(label, true);
            assert!(out.status.success(), "seeding {label}: {out:?}");
        }

        repo.write(path, "# edited pool fixture\n");

        for label in labels {
            let out = repo.run_leg(label, true);
            assert!(out.status.success(), "retrying {label}: {out:?}");
            let expected = usize::from(matches!(label, "rust-suite" | "rust-contracts")) + 1;
            assert_eq!(
                repo.executions(label),
                expected,
                "{path} invalidated the wrong battery: {label}"
            );
        }
    }
}

#[test]
fn a_dashboard_edit_reruns_only_contract_build_and_browser_batteries() {
    let repo = Repo::new();
    let labels = [
        "fmt",
        "clippy",
        "rust-suite",
        "rust-contracts",
        "build",
        "plugin",
        "e2e",
    ];
    for label in labels {
        let out = repo.run_leg(label, true);
        assert!(out.status.success(), "seeding {label}: {out:?}");
    }

    repo.write("src/web_dashboard.html", "<main>edited dashboard</main>\n");

    for label in labels {
        let out = repo.run_leg(label, true);
        assert!(out.status.success(), "retrying {label}: {out:?}");
        let expected = usize::from(matches!(label, "rust-contracts" | "build" | "e2e")) + 1;
        assert_eq!(
            repo.executions(label),
            expected,
            "dashboard edit invalidated the wrong battery: {label}"
        );
    }
}

#[test]
fn a_build_number_change_invalidates_every_compiled_identity_verdict() {
    let repo = Repo::new();
    repo.write("BUILD", "100\n");
    repo.git(&["add", "BUILD"]);
    let labels = [
        "fmt",
        "clippy",
        "rust-suite",
        "rust-contracts",
        "build",
        "plugin",
        "e2e",
    ];
    for label in labels {
        assert!(repo.run_leg(label, true).status.success());
    }
    repo.write("BUILD", "101\n");
    for label in labels {
        let result = repo.run_leg(label, true);
        assert!(result.status.success(), "{result:?}");
        assert_eq!(
            repo.executions(label),
            if label == "fmt" { 1 } else { 2 },
            "{label} must observe a changed compiled build identity"
        );
    }
}

#[test]
fn browser_reporter_regression_changes_invalidate_browser_evidence() {
    let repo = Repo::new();
    let path = "scripts/test-browser-launch-reporter.py";
    repo.write(path, "# original reporter regression\n");
    repo.git(&["add", path]);
    assert!(repo.run_leg("e2e", true).status.success());
    repo.write(path, "# changed reporter regression\n");
    assert!(repo.run_leg("e2e", true).status.success());
    assert_eq!(repo.executions("e2e"), 2);
}

#[test]
fn isolation_helper_changes_invalidate_browser_evidence() {
    let repo = Repo::new();
    let path = "scripts/e2e-isolation.py";
    repo.write(path, "# original isolation planner\n");
    repo.git(&["add", path]);
    for label in ["e2e", "rust-contracts", "rust-suite"] {
        assert!(repo.run_leg(label, true).status.success());
    }
    repo.write(path, "# changed isolation planner\n");
    for label in ["e2e", "rust-contracts", "rust-suite"] {
        assert!(repo.run_leg(label, true).status.success());
        assert_eq!(
            repo.executions(label),
            if label == "rust-suite" { 1 } else { 2 },
            "isolation helper changed the wrong evidence: {label}"
        );
    }
}

#[test]
fn a_contract_test_edit_does_not_invalidate_the_core_rust_battery() {
    let repo = Repo::new();
    let labels = [
        "fmt",
        "clippy",
        "rust-suite",
        "rust-contracts",
        "build",
        "plugin",
        "e2e",
    ];
    for label in labels {
        let out = repo.run_leg(label, true);
        assert!(out.status.success(), "seeding {label}: {out:?}");
    }

    repo.write(
        "tests/dashboard_contract.rs",
        "const ROOT: &str = env!(\"CARGO_MANIFEST_DIR\");\n#[test] fn contract_edited() { assert!(!ROOT.is_empty()); }\n",
    );

    for label in labels {
        let out = repo.run_leg(label, true);
        assert!(out.status.success(), "retrying {label}: {out:?}");
        let expected = usize::from(matches!(label, "fmt" | "clippy" | "rust-contracts")) + 1;
        assert_eq!(
            repo.executions(label),
            expected,
            "contract-test edit invalidated the wrong battery: {label}"
        );
    }
}

#[test]
fn rust_battery_classifier_is_disjoint_and_exhaustive() {
    fn target_names(mode: &str) -> BTreeSet<String> {
        let out = Command::new("bash")
            .args(["scripts/rust-test-targets.sh", mode])
            .current_dir(checkout())
            .output()
            .expect("running the Rust battery classifier");
        assert!(
            out.status.success(),
            "classifying {mode}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_owned)
            .collect()
    }

    let core = target_names("core");
    let contracts = target_names("contracts");
    assert!(
        core.is_disjoint(&contracts),
        "a Rust target belongs to both reusable batteries: {:?}",
        core.intersection(&contracts).collect::<Vec<_>>()
    );
    assert!(core.contains("storyhook"));
    assert!(core.contains("storyhook_test_support"));
    assert!(core.contains("ste_lint"));
    assert!(core.contains("lint"));
    assert!(contracts.contains("e2e_fixture_hygiene"));
    assert!(!core.contains("e2e_fixture_hygiene"));

    let metadata = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version=1"])
        .current_dir(checkout())
        .output()
        .expect("reading Cargo targets");
    assert!(
        metadata.status.success(),
        "cargo metadata failed: {metadata:?}"
    );
    let value: serde_json::Value =
        serde_json::from_slice(&metadata.stdout).expect("parsing cargo metadata");
    let expected: BTreeSet<String> = value["packages"]
        .as_array()
        .expect("packages array")
        .iter()
        .flat_map(|package| package["targets"].as_array().expect("targets array"))
        .filter(|target| {
            target["kind"]
                .as_array()
                .expect("target kind array")
                .iter()
                .any(|kind| matches!(kind.as_str(), Some("test" | "lib")))
        })
        .map(|target| target["name"].as_str().expect("target name").to_owned())
        .collect();
    assert_eq!(
        core.union(&contracts).cloned().collect::<BTreeSet<_>>(),
        expected,
        "the split silently omitted or invented a Cargo test target"
    );
}

/// Every reusable leg must re-run when any script its command executes
/// changes. `leg.sh --reuse` can only reuse evidence as safely as the leg's
/// fingerprint arm is complete, and an arm is a hand-kept list: SH-792 found
/// the e2e arm missing every helper `run-e2e.sh` sources, so an edit to
/// `e2e-selection.sh` could have been certified by a browser run that never
/// executed it. This derives each leg's command from the Makefile and the
/// scripts that command reaches from the scripts themselves, so a new helper
/// is covered the day it is sourced, not the day someone remembers the arm.
#[test]
fn every_script_a_reusable_leg_executes_is_an_input_to_that_leg() {
    let legs = reusable_leg_entries();
    for expected in [
        "fmt",
        "clippy",
        "rust-suite",
        "rust-contracts",
        "build",
        "plugin",
        "e2e",
    ] {
        assert!(
            legs.iter().any(|(label, _)| label == expected),
            "the Makefile parse lost the {expected} leg: {legs:?}"
        );
    }

    for (label, entry) in &legs {
        // `leg.sh` runs every leg and sources helpers of its own; the leg's
        // command script is the rest of what executes.
        let mut seeds = vec!["scripts/leg.sh".to_owned()];
        seeds.extend(entry.clone());
        if entry.as_deref() == Some("plugins/story/tests/run-tests.sh") {
            // The plugin runner executes every `test-*.sh` beside it through a
            // glob, and they source `lib.sh`; no reference parse can follow a
            // glob, so the directory's scripts are seeds in their own right.
            seeds.extend(tracked_scripts_in("plugins/story/tests"));
        }
        let scripts = executed_scripts(&seeds);

        let repo = Repo::new();
        for script in &scripts {
            repo.replace_with_tracked_copy(script, "");
        }
        repo.git(&["add", "."]);
        assert!(repo.run_leg(label, true).status.success());
        let mut runs = 1;
        for (probe, script) in scripts.iter().enumerate() {
            repo.replace_with_tracked_copy(script, &format!("\n# fingerprint probe {probe}\n"));
            let result = repo.run_leg(label, true);
            assert!(
                result.status.success(),
                "{label} after editing {script}: {result:?}"
            );
            runs += 1;
            assert_eq!(
                repo.executions(label),
                runs,
                "the {label} leg reused evidence after {script} changed, although \
                 {label}'s command executes it; add it to that leg's arm in \
                 scripts/gate-leg-fingerprint.sh"
            );
        }
    }
}

/// `(label, command script)` for every `leg.sh --reuse` call in the
/// Makefile. The script is `None` for a command that runs no repository
/// script (`cargo fmt`).
fn reusable_leg_entries() -> Vec<(String, Option<String>)> {
    let makefile = fs::read_to_string(checkout().join("Makefile")).expect("reading the Makefile");
    let mut legs: Vec<(String, Option<String>)> = Vec::new();
    for line in makefile.lines() {
        let Some((_, rest)) = line.split_once("scripts/leg.sh --reuse ") else {
            continue;
        };
        let label = rest
            .split_whitespace()
            .next()
            .expect("a leg label")
            .to_owned();
        let command = rest.split_once(" -- ").expect("a leg command").1;
        let tokens: Vec<&str> = command
            .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
            .filter(|token| !token.is_empty())
            .collect();
        let entry = tokens
            .windows(2)
            .find(|pair| is_interpreter(pair[0]) && is_script(pair[1]))
            .map(|pair| pair[1].to_owned());
        if !legs.iter().any(|(known, _)| *known == label) {
            legs.push((label, entry));
        }
    }
    legs
}

/// The seeds and every tracked script they run or source, transitively.
fn executed_scripts(seeds: &[String]) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut todo = seeds.to_vec();
    while let Some(path) = todo.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        let text = fs::read_to_string(checkout().join(&path))
            .unwrap_or_else(|e| panic!("reading {path}: {e}"));
        todo.extend(script_references(&path, &text));
    }
    seen
}

/// The tracked scripts `path` executes: the operand of `.`, `source`, `bash`
/// or `python3` on any non-comment line. Only operation words count, so a
/// script that merely NAMES another as data (the fingerprint's own case arms,
/// a usage message) adds nothing.
fn script_references(path: &str, text: &str) -> Vec<String> {
    let dir = Path::new(path).parent().expect("script has a directory");
    let mut found = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        let tokens: Vec<&str> = line
            .split(|c: char| c.is_whitespace() || ";&|()".contains(c))
            .filter(|token| !token.is_empty())
            .collect();
        for pair in tokens.windows(2) {
            if !matches!(pair[0], "." | "source") && !is_interpreter(pair[0]) {
                continue;
            }
            let operand = pair[1].trim_matches(|c| c == '"' || c == '\'');
            if !is_script(operand) {
                continue;
            }
            // Repository-rooted spellings (`$repo_root/scripts/x`,
            // `$TESTS_DIR/../../../scripts/x`, `scripts/x`) and the
            // script-relative `$script_dir/x`.
            let candidate = if let Some(at) = operand.find("scripts/") {
                operand[at..].to_owned()
            } else if let Some(rest) = operand.strip_prefix("$script_dir/") {
                dir.join(rest).to_string_lossy().into_owned()
            } else {
                continue;
            };
            if checkout().join(&candidate).is_file() {
                found.push(candidate);
            }
        }
    }
    found
}

fn is_interpreter(token: &str) -> bool {
    matches!(
        token.trim_matches('"'),
        "bash" | "python3" | "$STORYHOOK_PYTHON" | "$${STORYHOOK_PYTHON}"
    )
}

fn is_script(token: &str) -> bool {
    let token = token.trim_matches(|c| c == '"' || c == '\'');
    token.ends_with(".sh") || token.ends_with(".py")
}

fn tracked_scripts_in(dir: &str) -> Vec<String> {
    let out = Command::new("git")
        .args(["ls-files", "--", dir])
        .current_dir(checkout())
        .output()
        .expect("listing tracked scripts");
    assert!(out.status.success(), "git ls-files failed: {out:?}");
    String::from_utf8(out.stdout)
        .expect("utf-8 paths")
        .lines()
        .filter(|path| is_script(path))
        .map(str::to_owned)
        .collect()
}
