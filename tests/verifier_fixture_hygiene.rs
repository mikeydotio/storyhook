//! SH-699: direct verifier scripts must have containment at their command door.
//!
//! This textual fence follows a file's `run` helper once. It deliberately does
//! not infer arbitrary Rust dataflow; behavioral tests prove propagation through
//! the actuator, and private-server tests prove the opt-in fixture's teardown.

use std::path::Path;
use storyhook_test_support::without_rust_comments;

fn functions(text: &str) -> Vec<&str> {
    let mut starts = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let line_start = line.trim_start();
        if line_start.starts_with("fn ") || line_start.starts_with("pub fn ") {
            starts.push(offset);
        }
        offset += line.len();
    }
    starts
        .iter()
        .enumerate()
        .map(|(index, start)| &text[*start..starts.get(index + 1).copied().unwrap_or(text.len())])
        .collect()
}

fn contained(block: &str) -> bool {
    let compact: String = block.chars().filter(|c| !c.is_whitespace()).collect();
    compact.contains(".envs(daemon_containment())")
        || compact.contains(".envs(storyhook_test_support::daemon_containment())")
        || compact.contains(".env(\"STORYHOOK_VERIFIER_MIRROR\",\"0\")")
        // Recording tmux fixtures explicitly select their executable directory.
        || (compact.contains(".env(\"PATH\",")
            && (compact.contains("tmux_dir") || compact.contains("bin.join(\"tmux\")")))
        || compact.contains(".apply_mirror(&mutcommand)")
        || (compact.contains(".env(\"PATH\",\"/bin\")")
            && compact.contains("!Path::new(\"/bin/tmux\").exists()"))
}

fn violations(text: &str) -> Vec<String> {
    let stripped = without_rust_comments(text);
    let blocks = functions(&stripped);
    let contained_run = blocks
        .iter()
        .any(|block| block.trim_start().starts_with("fn run(") && contained(block));
    blocks
        .into_iter()
        .filter(|block| {
            let script = ["-pr.sh", "-window.sh", "-batch.sh"]
                .iter()
                .any(|suffix| block.contains(&format!("scripts/verify{suffix}")))
                || block.contains(&["scripts/", "verification-view.py"].concat());
            let opt_in = block.contains("\"STORYHOOK_VERIFIER_MIRROR\", \"1\"")
                || block.contains("env_remove(\"STORYHOOK_VERIFIER_MIRROR\")");
            let direct_launch = block.contains(".output()")
                || block.contains("ChildGuard::spawn")
                || block.contains("-> Command");
            let helper_call = calls_run(block);
            let launches = direct_launch || helper_call;
            (script || opt_in)
                && launches
                && !contained(block)
                && !(contained_run && helper_call && !direct_launch)
        })
        .map(|block| block.lines().next().unwrap_or_default().trim().to_owned())
        .collect()
}

fn calls_run(block: &str) -> bool {
    block.match_indices("run(").any(|(at, _)| {
        let before = &block[..at];
        !before.trim_end().ends_with("fn")
            && !before
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | ':'))
    })
}

#[test]
fn verifier_commands_are_contained_at_their_function_or_command_helper() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            "tests/*.rs",
            "tests/support/*.rs",
        ])
        .output()
        .expect("list tracked fixture sources");
    assert!(
        listed.status.success(),
        "cannot inspect fixture corpus: {listed:?}"
    );
    let mut failures = Vec::new();
    let mut saw_queue = false;
    let mut saw_live = false;
    for entry in listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let relative = std::str::from_utf8(entry).expect("UTF-8 tracked path");
        saw_queue |= relative == "tests/verification_queue.rs";
        saw_live |= relative == "tests/verify_window.rs";
        let path = root.join(relative);
        if !path.exists() {
            continue;
        }
        let source = std::fs::read_to_string(path).expect("read tracked fixture");
        failures.extend(
            violations(&source)
                .into_iter()
                .map(|name| format!("{relative}: {name}")),
        );
    }
    assert!(
        saw_queue && saw_live,
        "the known hazard corpus must be scanned"
    );
    assert!(
        failures.is_empty(),
        "uncontained verifier fixture commands:\n{}",
        failures.join("\n")
    );
}

fn probe(containment: &str) -> String {
    let script = ["scripts/verify", "-pr.sh"].concat();
    format!(
        "fn unsafe_command() {{ Command::new(\"bash\").arg(\"{script}\"){containment}.output(); }}"
    )
}

#[test]
fn a_sibling_function_cannot_launder_an_uncontained_launch() {
    let isolated = probe(".envs(daemon_containment())").replace("unsafe_command", "safe_command");
    assert_eq!(violations(&format!("{isolated}\n{}", probe(""))).len(), 1);
    assert!(violations(&isolated).is_empty());
    assert_eq!(
        violations(&format!("// daemon_containment()\n{}", probe(""))).len(),
        1
    );
    let viewer = probe("").replace("verify-pr.sh", "verification-view.py");
    assert_eq!(violations(&viewer).len(), 1);
}

#[test]
fn a_command_helper_must_itself_apply_containment() {
    let script = ["scripts/verify", "-pr.sh"].concat();
    let source = format!(
        "fn caller() {{ run(\"{script}\"); }}\nfn run() {{ Command::new(\"bash\").output(); }}"
    );
    assert_eq!(violations(&source).len(), 1);
    assert!(
        violations(&source.replace(".output()", ".envs(daemon_containment()).output()")).is_empty()
    );
}

#[test]
fn a_helper_name_or_unrelated_safe_call_cannot_hide_a_direct_launch() {
    let safe = "fn run() { Command::new(\"bash\").envs(daemon_containment()).output(); }";
    let unsafe_run = probe("").replace("unsafe_command", "unsafe_run");
    assert_eq!(violations(&format!("{safe}\n{unsafe_run}")).len(), 1);
    let mixed = probe("").replace("Command::new", "run(\"harmless\"); Command::new");
    assert_eq!(violations(&format!("{safe}\n{mixed}")).len(), 1);
    assert!(!calls_run("fn outer() { other_run(1); object.run(2); }"));
}

/// SH-822: whether a fixture source turns the Verifier Agent pane on without
/// a fake provider of its own. The shared test environment sets
/// `STORYHOOK_VERIFIER_AGENT=0`; a fixture that sets it back to `1` would
/// otherwise let the verification window start the real `claude` on PATH —
/// a paid, network-bound session inside a gate run.
fn enables_agent_without_fake_provider(source: &str) -> bool {
    let compact: String = source.chars().filter(|c| !c.is_whitespace()).collect();
    let enables = compact.contains("\"STORYHOOK_VERIFIER_AGENT\",\"1\"")
        || compact.contains("[\"STORYHOOK_VERIFIER_AGENT\"]=\"1\"")
        || compact.contains("STORYHOOK_VERIFIER_AGENT=\"1\"");
    enables && !compact.contains("\"claude\"")
}

#[test]
fn a_fixture_that_enables_the_verifier_agent_supplies_a_fake_provider() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            "tests/*.rs",
            "tests/support/*",
            "scripts/tests/*.py",
            "plugins/story/tests/*",
        ])
        .output()
        .expect("list tracked fixture sources");
    assert!(
        listed.status.success(),
        "cannot inspect fixtures: {listed:?}"
    );
    let offenders: Vec<String> = listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            std::str::from_utf8(entry)
                .expect("UTF-8 tracked path")
                .to_owned()
        })
        .filter(|relative| relative != "tests/verifier_fixture_hygiene.rs")
        .filter(|relative| {
            std::fs::read_to_string(root.join(relative))
                .is_ok_and(|source| enables_agent_without_fake_provider(&source))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "these fixtures enable the Verifier Agent without a fake `claude`: {offenders:?}"
    );
}

#[test]
fn the_agent_fence_recognizes_each_opt_in_spelling() {
    for opt_in in [
        "command.env(\"STORYHOOK_VERIFIER_AGENT\", \"1\");",
        "self.env[\"STORYHOOK_VERIFIER_AGENT\"] = \"1\"",
        "env.update(STORYHOOK_VERIFIER_AGENT=\"1\")",
    ] {
        assert!(enables_agent_without_fake_provider(opt_in), "{opt_in}");
        let with_fake = format!("{opt_in}\nbin.join(\"claude\")");
        assert!(!enables_agent_without_fake_provider(&with_fake), "{opt_in}");
    }
    assert!(!enables_agent_without_fake_provider(
        "command.env(\"STORYHOOK_VERIFIER_AGENT\", \"0\");"
    ));
}
