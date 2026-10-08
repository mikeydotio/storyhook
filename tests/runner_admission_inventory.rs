//! Every production runner launch is accounted for by host admission (SH-869).
//!
//! The host admission authority (SH-868) bounds machine load only for work
//! that asks it. This census pins every production file that can launch a
//! build or test runner and requires a decision for each: the file enters a
//! named admission entry, Cargo's own wrapper and runner admit the launch, it
//! runs a Makefile tier whose legs are admitted, or it is an explicit
//! exception with its reason. A new runner launch cannot appear without that
//! decision. The entries themselves live in `scripts/host_admission/entries.py`;
//! every entry must have a production caller, and every caller must name a
//! real entry. Derived over tracked files, comment and message lines
//! stripped, in the style of `tests/tmux_server_start_sites.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use regex::Regex;

/// How one file's runner launches of one kind are accounted for.
#[derive(Clone, Copy, Debug)]
enum Coverage {
    /// The repository-local whole-Cargo entry owns every build phase. Compiler
    /// and binary hooks alone do not cover cached build scripts (SH-835).
    ManagedCargo,
    /// The file itself enters this admission entry before it launches.
    Entry(&'static str),
    /// It runs a Makefile gate tier; each leg's runners are admitted.
    Legs,
    /// Text that names a runner without launching one.
    Prose(&'static str),
    /// Deliberate artifact inspection: it reads a built binary and runs no
    /// test, so it is not admitted.
    Inspection(&'static str),
    /// Deliberately outside this host's admission, with the reason.
    External(&'static str),
}

/// Runner signatures by kind. Each regex is applied to code lines only.
fn signatures() -> Vec<(&'static str, Regex)> {
    vec![
        ("cargo", Regex::new(r"managed-cargo\.sh").unwrap()),
        (
            "cargo",
            Regex::new(
                r#"(^|[\s"'(/\[])cargo["']?,?\s+["']?(test|build|clippy|check|run|bench|nextest)\b"#,
            )
            .unwrap(),
        ),
        (
            "cargo",
            Regex::new(r#"\bcargo\s*\+\s*\[\s*["'](test|build|clippy|check|run|bench|nextest)["']"#)
                .unwrap(),
        ),
        ("playwright", Regex::new(r"playwright\s+test\b").unwrap()),
        // A test binary executed directly to list its cases, outside Cargo.
        ("test-listing", Regex::new(r#"\[\s*\w+,\s*"--list""#).unwrap()),
        (
            "make-gate",
            Regex::new(r"\bmake\s+(-\S+\s+)*test(-full|-changed)?\b").unwrap(),
        ),
    ]
}

/// Every (file, kind) with a runner launch, and how it is accounted for.
const SITES: &[(&str, &str, Coverage)] = &[
    ("Makefile", "cargo", Coverage::ManagedCargo),
    (
        "scripts/attribution-rust.py",
        "cargo",
        Coverage::Entry("causal-rust"),
    ),
    (
        "scripts/build-release-assets.sh",
        "cargo",
        Coverage::ManagedCargo,
    ),
    (
        "scripts/capture-baseline.sh",
        "cargo",
        Coverage::ManagedCargo,
    ),
    ("scripts/capture-baseline.sh", "make-gate", Coverage::Legs),
    (
        "scripts/cargo_diagnostics.py",
        "cargo",
        Coverage::ManagedCargo,
    ),
    ("scripts/coverage-map.sh", "cargo", Coverage::ManagedCargo),
    (
        "scripts/coverage-map.sh",
        "make-gate",
        Coverage::Prose("a refusal message naming the gate to run first"),
    ),
    ("scripts/coverage-watch.sh", "make-gate", Coverage::Legs),
    ("scripts/browser-watch.sh", "make-gate", Coverage::Legs),
    (
        "scripts/browser-status.sh",
        "make-gate",
        Coverage::Prose("a status message naming the browser tier"),
    ),
    (
        "scripts/release-linux.sh",
        "cargo",
        Coverage::External(
            "runs inside the Lima guest, a separate kernel; the host reserves the guest \
             through the enclosing release entry",
        ),
    ),
    ("scripts/release.sh", "make-gate", Coverage::Legs),
    ("scripts/run-e2e.sh", "cargo", Coverage::ManagedCargo),
    (
        "scripts/run-e2e.sh",
        "playwright",
        Coverage::Entry("browser-pool"),
    ),
    ("scripts/run-tests.sh", "cargo", Coverage::ManagedCargo),
    ("scripts/scratch-env.sh", "cargo", Coverage::ManagedCargo),
    (
        "scripts/test-delta.sh",
        "cargo",
        Coverage::Prose("a usage message naming its input"),
    ),
    ("scripts/test-pool.py", "cargo", Coverage::ManagedCargo),
    (
        "scripts/test-system-attributes.py",
        "cargo",
        Coverage::ManagedCargo,
    ),
    ("scripts/test_discovery.py", "cargo", Coverage::ManagedCargo),
    (
        "scripts/test_discovery.py",
        "test-listing",
        Coverage::Inspection("lists a built test binary's cases with --list; runs no test"),
    ),
    (
        "plugins/story/bin/story.sh",
        "make-gate",
        Coverage::Prose("agent prompt text about test scope"),
    ),
];

/// Files that enter an admission entry, whether or not a signature finds a
/// launch in them: the pools, the supervisors and the wrappers.
const ENTRY_FILES: &[(&str, &str)] = &[
    ("scripts/host_admission/diagnosis.py", "causal-rust"),
    (".cargo/config.toml", "cargo-test-binary"),
    ("scripts/capture-baseline.sh", "cargo-test-binary"),
    ("scripts/rustc-slot.py", "rustc"),
    ("scripts/cargo-managed.py", "cargo-managed"),
    ("scripts/run-tests.sh", "rust-pool"),
    ("plugins/story/tests/run-tests.sh", "plugin-pool"),
    ("plugins/story/tests/lib.sh", "plugin-script"),
    ("scripts/run-e2e.sh", "browser-pool"),
    (
        "scripts/tests/run_verifier_tests.py",
        "verifier-python-workers",
    ),
    ("scripts/verifier-owner.py", "verifier-gate"),
    ("scripts/release.sh", "release"),
    ("scripts/release-observer.py", "release-observer"),
];

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Tracked production sources that can launch runners: shipped scripts, the
/// plugin payload and runner, the Makefile and Cargo's configuration. Test
/// fixtures under `scripts/tests/` launch runners deliberately and are out of
/// scope, except the case runner that `ENTRY_FILES` names explicitly.
fn production_sources(root: &Path) -> Vec<(String, String)> {
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            "scripts",
            "plugins/story/bin",
            "plugins/story/lib",
            "plugins/story/hooks",
            "plugins/story/tests/run-tests.sh",
            "plugins/story/tests/lib.sh",
            "Makefile",
            ".cargo/config.toml",
        ])
        .output()
        .expect("listing this repository's production sources");
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so this scan proved nothing: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|path| std::str::from_utf8(path).expect("a UTF-8 path").to_string())
        .filter(|path| {
            (!path.starts_with("scripts/tests/") || path == "scripts/tests/run_verifier_tests.py")
                && !path.starts_with("scripts/_vendor/")
                && !path.contains("__pycache__")
        })
        .filter_map(|path| {
            std::fs::read_to_string(root.join(&path))
                .ok()
                .map(|text| (path, text))
        })
        .collect()
}

/// The first word of a line that only prints or reports text.
const MESSAGE_WORDS: &[&str] = &[
    "echo", "printf", "note", "say", "die", "warn", "info", "step", "fail", "usage", "error",
    "log", "print(",
];

/// The code lines of `text`: comment lines, message lines and Python
/// docstrings dropped. A launch never hides on those lines; a mention does.
fn code_lines(path: &str, text: &str) -> Vec<String> {
    let python = path.ends_with(".py");
    let mut lines = Vec::new();
    let mut docstring: Option<&str> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if python {
            if let Some(quote) = docstring {
                if trimmed.contains(quote) {
                    docstring = None;
                }
                continue;
            }
            if let Some(quote) = ["\"\"\"", "'''"]
                .into_iter()
                .find(|quote| trimmed.starts_with(quote))
            {
                if trimmed[3..].contains(quote) {
                    continue;
                }
                docstring = Some(quote);
                continue;
            }
        }
        if trimmed.starts_with('#') {
            continue;
        }
        let first = trimmed.split_whitespace().next().unwrap_or("");
        if MESSAGE_WORDS
            .iter()
            .any(|word| first == *word || first.starts_with(word) && word.ends_with('('))
        {
            continue;
        }
        lines.push(line.to_string());
    }
    lines
}

/// Every (file, kind) whose code lines carry a runner signature.
fn found_sites(sources: &[(String, String)]) -> BTreeSet<(String, &'static str)> {
    let signatures = signatures();
    let mut found = BTreeSet::new();
    for (path, text) in sources {
        if path == "tests/runner_admission_inventory.rs" {
            continue;
        }
        for line in code_lines(path, text) {
            for (kind, signature) in &signatures {
                if signature.is_match(&line) {
                    found.insert((path.clone(), *kind));
                }
            }
        }
    }
    found
}

/// The entry identities declared in `scripts/host_admission/entries.py`.
fn declared_entries(root: &Path) -> BTreeSet<String> {
    let text = std::fs::read_to_string(root.join("scripts/host_admission/entries.py"))
        .expect("the admission inventory exists");
    let entry = Regex::new(r#"Entry\(\s*"([a-z0-9-]+)""#).unwrap();
    entry
        .captures_iter(&text)
        .map(|capture| capture[1].to_string())
        .collect()
}

/// Whether `text` enters `entry` through the adapter: its command line, its
/// Python API, or Cargo's runner array.
fn enters(text: &str, entry: &str) -> bool {
    let patterns = [
        format!("--entry {entry}"),
        format!("--entry \"{entry}\""),
        format!("\"--entry\", \"{entry}\""),
        format!("'--entry', '{entry}'"),
        format!("admit(\"{entry}\""),
        format!("Reservation(\"{entry}\""),
        format!("ENTRY = \"{entry}\""),
    ];
    patterns.iter().any(|pattern| text.contains(pattern))
}

#[test]
fn every_runner_launch_is_admitted_or_an_explicit_exception() {
    let root = repo_root();
    let sources = production_sources(root);
    assert!(
        sources
            .iter()
            .any(|(path, _)| path == "scripts/run-tests.sh"),
        "the scan found no runner sources, so it proved nothing"
    );
    let found = found_sites(&sources);
    let expected: BTreeSet<(String, &str)> = SITES
        .iter()
        .map(|(path, kind, _)| ((*path).to_string(), *kind))
        .collect();
    let unclassified: Vec<_> = found.difference(&expected).collect();
    let stale: Vec<_> = expected.difference(&found).collect();
    assert!(
        unclassified.is_empty(),
        "a production file launches a runner without an admission decision: {unclassified:?}. \
         Enter a host admission entry (scripts/host-admit.py --entry <id>), launch it through \
         Cargo, or add it to SITES with its reason"
    );
    assert!(
        stale.is_empty(),
        "SITES pins launches that no longer exist; remove them: {stale:?}"
    );
    let texts: BTreeMap<&str, &str> = sources
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    for (path, kind, coverage) in SITES {
        if let Coverage::ManagedCargo = coverage {
            assert!(
                texts[path].contains("managed-cargo.sh"),
                "{path} lacks the whole-Cargo ownership entry"
            );
        }
        if let Coverage::Entry(entry) = coverage {
            assert!(
                enters(texts[path], entry),
                "{path} launches {kind} but does not enter the {entry} admission entry"
            );
        }
        if let Coverage::Prose(reason) | Coverage::External(reason) | Coverage::Inspection(reason) =
            coverage
        {
            assert!(
                !reason.trim().is_empty(),
                "{path}: an exception needs a reason"
            );
        }
    }
}

#[test]
fn every_admission_entry_has_a_production_caller_and_every_caller_a_real_entry() {
    let root = repo_root();
    let sources = production_sources(root);
    let texts: BTreeMap<&str, &str> = sources
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    let declared = declared_entries(root);
    assert!(
        declared.contains("rustc") && declared.contains("verifier-gate"),
        "the admission inventory could not be read: {declared:?}"
    );
    for (path, entry) in ENTRY_FILES {
        assert!(
            declared.contains(*entry),
            "{path} names {entry}, which scripts/host_admission/entries.py does not declare"
        );
        let text = texts
            .get(path)
            .unwrap_or_else(|| panic!("{path} is pinned as an admission caller but is missing"));
        assert!(
            enters(text, entry),
            "{path} no longer enters the {entry} admission entry"
        );
    }
    let called: BTreeSet<String> = ENTRY_FILES
        .iter()
        .map(|(_, entry)| (*entry).to_string())
        .collect();
    let uncalled: Vec<_> = declared.difference(&called).collect();
    assert!(
        uncalled.is_empty(),
        "admission entries without a production caller: {uncalled:?}"
    );
}

#[test]
fn cargo_launches_stay_routed_through_the_admission_wrapper_and_runner() {
    let config = std::fs::read_to_string(repo_root().join(".cargo/config.toml"))
        .expect("the Cargo configuration exists");
    let lines = code_lines(".cargo/config.toml", &config);
    assert!(
        lines
            .iter()
            .any(|line| line.trim() == r#"rustc-wrapper = "scripts/rustc-slot.py""#),
        "Cargo's compiles must stay behind the admission-aware wrapper"
    );
    assert!(
        lines
            .iter()
            .any(|line| line.trim() == "[target.'cfg(all())']"),
        "Cargo's test binaries need a runner table for every target"
    );
    assert!(
        lines.iter().any(|line| {
            line.trim_start().starts_with("runner")
                && line.contains("\"scripts/host-admit.py\"")
                && line.contains("\"cargo-test-binary\"")
        }),
        "Cargo's test binaries must run through scripts/host-admit.py --entry cargo-test-binary"
    );
}

#[test]
fn the_census_sees_launches_and_ignores_mentions() {
    let text = "\
# cargo test in a comment is not a launch
echo \"run cargo test first\"
die \"make test-full first\"
cargo test --workspace
\"$toolchain/bin/cargo\" build --release
npx playwright test --project=chromium
run github_without_credentials make test-full
";
    let sources = vec![("scripts/new-runner.sh".to_string(), text.to_string())];
    let found = found_sites(&sources);
    let kinds: BTreeSet<&str> = found.iter().map(|(_, kind)| *kind).collect();
    assert_eq!(
        kinds,
        BTreeSet::from(["cargo", "playwright", "make-gate"]),
        "the census must see every launch in a new file"
    );
    let python = "\
\"\"\"A module docstring that says cargo test.\"\"\"
def f():
    \"\"\"
    cargo build is mentioned here
    \"\"\"
    print(\"cargo test\")
";
    let found = found_sites(&[("scripts/x.py".to_string(), python.to_string())]);
    assert!(
        found.is_empty(),
        "docstrings and messages are mentions: {found:?}"
    );
    for python in [
        "subprocess.run([\"cargo\", \"test\", \"--no-run\"])\n",
        "subprocess.Popen(['cargo', 'build'])\n",
        "self.successful('build', cargo + ['test', '--no-run'])\n",
    ] {
        let found = found_sites(&[("scripts/x.py".to_string(), python.to_string())]);
        assert_eq!(
            found.len(),
            1,
            "a Python argv launch is a site: {python} {found:?}"
        );
    }
    assert!(enters(
        "exec \"$py\" host-admit.py --entry rust-pool -- x",
        "rust-pool"
    ));
    assert!(!enters(
        "exec host-admit.py --entry rust-pool-x",
        "rust-pool-y"
    ));
    assert!(enters(
        "Reservation(\"verifier-gate\", project=p)",
        "verifier-gate"
    ));
    let listing = "output = self.run([executable, \"--list\", *args], \"listing tests\")\n";
    let found = found_sites(&[("scripts/y.py".to_string(), listing.to_string())]);
    assert_eq!(
        found.iter().map(|(_, kind)| *kind).collect::<Vec<_>>(),
        ["test-listing"],
        "a direct test-binary listing is a site that needs a decision"
    );
}
