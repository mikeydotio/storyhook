//! Implementers run only the tests their story adds or changes (SH-864).
//!
//! # Why this file exists
//!
//! Every surface that told an implementer which tests to run said "new and
//! (directly) impacted tests", and agents read "impacted" as widely as they
//! liked: one lane ran 23 integration targets, another 13, while four other
//! lanes and the central gate shared the same machine (SH-863 measured the
//! gate's load and the false REDs it causes). The central verifier already
//! runs the full suite on the merge tree, so a lane-side sweep bought
//! contention, not safety. SH-864 replaces the judgment word with one rule,
//! [`IMPLEMENTER_TEST_SCOPE`], plus one narrow exception a council settled
//! for a failed gate, [`FAILED_GATE_RERUN_SCOPE`]: rerun exactly the test
//! cases the gate's log names as failing, by exact name, and nothing wider.
//!
//! # Two kinds of check
//!
//! **Presence, by name.** Each imperative surface — the scaffolded
//! instruction files, the help topic agents are told to read first, both
//! built-in dispatch charters, and the runtime return texts — carries the
//! rule verbatim, so no surface can quietly fall back to "test whatever seems
//! relevant". The shell charter cannot import a Rust constant; the constants
//! are charter-inert so it can carry their exact bytes, and this file checks
//! that it does.
//!
//! **Absence, derived.** No agent-facing source says "impacted". Unlike the
//! blanket `story new` scan `tests/scope_rubric.rs` tried and rejected, this
//! word has no other use in these sources, so the scan has no false positives
//! today; a future legitimate use takes one reviewed [`EXEMPT`] entry rather
//! than a narrower scan that would miss the next surface.

use std::collections::BTreeMap;
use std::path::Path;

use storyhook::help_topics::get_help_topic;
use storyhook::service::templates;
use storyhook::service::verification::{FAILED_GATE_RERUN_SCOPE, IMPLEMENTER_TEST_SCOPE};

/// Characters a built-in charter may not carry (SH-226; `plugins/story/bin/
/// story.sh` explains why and `plugins/story/tests/test-charter-inert.sh`
/// enforces it on the rendered prompt). Both constants are pasted into the
/// charters, so they must be inert themselves.
const CHARTER_SPECIAL: &[char] = &[
    '`', '$', ';', '&', '|', '<', '>', '!', '(', ')', '[', ']', '{', '}', '*', '?', '~', '#', '\\',
    '\'', '\n',
];

/// The retired wording. Matched case-insensitively as a substring, so
/// "impacted-test selector" and "Impacted" are caught too.
const RETIRED: &str = "impacted";

/// Tracked agent-facing sources the derived scan reads. The plugin's own
/// `tests/` directory is excluded: its fixtures quote historical text that
/// instructs nobody.
const SCANNED: &[&str] = &[
    "src",
    "plugins/story",
    "AGENTS.md",
    ":(exclude)plugins/story/tests",
];

/// Reviewed exceptions to the derived scan: `(path, reason)`. Empty because
/// no agent-facing source has a legitimate use of the retired word today.
const EXEMPT: &[(&str, &str)] = &[];

/// Runtime sources whose return or recovery text interpolates the rule, with
/// the exact number of inline-captured `{CONSTANT}` interpolations each makes
/// (an import of the name does not count). An exact count is a reviewed
/// inventory: a new return text that skips the rule, or one that adds it,
/// both have to come through here.
const RUNTIME_SITES: &[(&str, &str, usize)] = &[
    // The RED diagnosis, shared by single-story and batch-culprit returns.
    ("src/daemon/verification/repair_return.rs", RULE, 1),
    ("src/daemon/verification/repair_return.rs", RERUN, 1),
    // The CONFLICT return: no failed gate, so no rerun exception.
    ("src/daemon/verification.rs", RULE, 1),
    ("src/daemon/verification.rs", RERUN, 0),
    // The managed project-recovery delivery.
    ("src/daemon/project_recovery/transport.rs", RULE, 1),
    // The separate repair story's description and the in-place repair comment.
    ("src/service/project_recovery/decision_effects.rs", RULE, 2),
    // A project repair whose gate failed: the same exception as a RED.
    ("src/service/project_recovery/test_return.rs", RULE, 1),
    ("src/service/project_recovery/test_return.rs", RERUN, 1),
    // The managed resume after a certified repair lands.
    ("src/service/project_recovery/resume.rs", RULE, 1),
];

/// The base rule's name, as [`RUNTIME_SITES`] counts it in source.
const RULE: &str = "IMPLEMENTER_TEST_SCOPE";

/// The failed-gate exception's name, as [`RUNTIME_SITES`] counts it in source.
const RERUN: &str = "FAILED_GATE_RERUN_SCOPE";

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: &str) -> String {
    std::fs::read_to_string(repo_root().join(relative))
        .unwrap_or_else(|error| panic!("reading {relative}: {error}"))
}

/// Collapses every run of whitespace to one space, so a sentence that a help
/// topic or template wraps across lines still matches the constant.
fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The one-based line numbers of `text` that contain the retired word.
fn retired_lines(text: &str) -> Vec<usize> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| line.to_lowercase().contains(RETIRED))
        .map(|(index, _)| index + 1)
        .collect()
}

/// Every tracked file the derived scan covers, keyed relative to the root.
fn scanned_files() -> BTreeMap<String, String> {
    let listed = std::process::Command::new("git")
        .current_dir(repo_root())
        .args(["ls-files", "-z", "--"])
        .args(SCANNED)
        .output()
        .expect("listing this repository's tracked agent-facing sources");
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so this scan proved nothing: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let files: BTreeMap<String, String> = listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|path| {
            let relative = std::str::from_utf8(path)
                .expect("tracked paths must be UTF-8")
                .to_string();
            let bytes = std::fs::read(repo_root().join(&relative))
                .unwrap_or_else(|error| panic!("reading tracked {relative}: {error}"));
            (relative, String::from_utf8_lossy(&bytes).into_owned())
        })
        .collect();
    for required in [
        "src/service/templates.rs",
        "plugins/story/bin/story.sh",
        "AGENTS.md",
    ] {
        assert!(
            files.contains_key(required),
            "the scan must cover {required}; the pathspecs no longer reach it"
        );
    }
    files
}

/// The value of one `NAME="..."` assignment in `story.sh`, by line prefix.
fn charter_assignment<'a>(script: &'a str, name: &str) -> &'a str {
    let prefix = format!("{name}=");
    let mut matches = script.lines().filter(|line| line.starts_with(&prefix));
    let line = matches
        .next()
        .unwrap_or_else(|| panic!("plugins/story/bin/story.sh no longer assigns {name}"));
    assert!(
        matches.next().is_none(),
        "plugins/story/bin/story.sh assigns {name} more than once"
    );
    line
}

#[test]
fn both_rules_are_charter_inert_and_say_who_owns_the_rest() {
    for (name, rule) in [
        ("IMPLEMENTER_TEST_SCOPE", IMPLEMENTER_TEST_SCOPE),
        ("FAILED_GATE_RERUN_SCOPE", FAILED_GATE_RERUN_SCOPE),
    ] {
        let special: Vec<char> = rule
            .chars()
            .filter(|c| CHARTER_SPECIAL.contains(c))
            .collect();
        assert!(
            special.is_empty(),
            "{name} carries {special:?}, which a built-in charter may not hold: {rule}"
        );
        assert!(
            !rule.to_lowercase().contains(RETIRED),
            "{name} must not reintroduce the retired wording: {rule}"
        );
    }
    assert!(
        IMPLEMENTER_TEST_SCOPE.contains("adds or changes"),
        "{IMPLEMENTER_TEST_SCOPE}"
    );
    assert!(
        IMPLEMENTER_TEST_SCOPE.contains("central verifier"),
        "{IMPLEMENTER_TEST_SCOPE}"
    );
    assert!(
        IMPLEMENTER_TEST_SCOPE.contains("release gates"),
        "{IMPLEMENTER_TEST_SCOPE}"
    );
    for needle in [
        "names as failing",
        "exact name only",
        "Never rerun a whole target",
        "Never edit or weaken a test",
        "change no code for it",
    ] {
        assert!(
            FAILED_GATE_RERUN_SCOPE.contains(needle),
            "the failed-gate exception lost '{needle}': {FAILED_GATE_RERUN_SCOPE}"
        );
    }
}

#[test]
fn every_scaffolded_instruction_file_states_the_rule() {
    let legacy_root = storyhook_test_support::scratch_dir();
    storyhook::storage::init_project(legacy_root.path(), None).expect("seeding a legacy tree");
    let legacy = std::fs::read_to_string(legacy_root.path().join(".storyhook/CLAUDE.md"))
        .expect("the legacy tree carries its instruction file");
    for (name, text) in [
        ("templates::agents_md", templates::agents_md("SH", "done")),
        ("templates::cursor_rules", templates::cursor_rules()),
        ("this repository's AGENTS.md", read("AGENTS.md")),
        ("the legacy .storyhook/CLAUDE.md", legacy),
    ] {
        assert!(
            squash(&text).contains(IMPLEMENTER_TEST_SCOPE),
            "{name} does not state the implementer test rule verbatim:\n{text}"
        );
    }
}

#[test]
fn the_agent_guide_states_the_rule() {
    let guide = get_help_topic("agent-guide").expect("the agent guide must ship");
    assert!(
        squash(guide).contains(IMPLEMENTER_TEST_SCOPE),
        "story help agent-guide does not state the implementer test rule:\n{guide}"
    );
}

#[test]
fn both_built_in_charters_state_the_rule_and_the_failed_gate_exception() {
    let script = read("plugins/story/bin/story.sh");
    // The charters share each clause through one shell variable, the way they
    // already share OBVIATION_REVIEW_CLAUSE, so the two cannot drift apart.
    for (clause, constant) in [
        ("TEST_SCOPE_CLAUSE", IMPLEMENTER_TEST_SCOPE),
        ("FAILED_GATE_RERUN_CLAUSE", FAILED_GATE_RERUN_SCOPE),
    ] {
        assert_eq!(
            charter_assignment(&script, clause),
            format!("{clause}=\"{constant}\""),
            "{clause} must carry the Rust constant's exact bytes"
        );
    }
    for name in ["PROMPT_TPL", "AUTO_PROMPT_TAIL"] {
        let charter = charter_assignment(&script, name);
        let rule = charter.find("$TEST_SCOPE_CLAUSE");
        let repair = charter.find("If verification returns");
        let exception = charter.find("$FAILED_GATE_RERUN_CLAUSE");
        for (what, needle) in [
            ("the implementer test rule", "$TEST_SCOPE_CLAUSE"),
            ("the failed-gate exception", "$FAILED_GATE_RERUN_CLAUSE"),
            ("the repair clause", "If verification returns"),
        ] {
            assert_eq!(
                charter.matches(needle).count(),
                1,
                "{name} must state {what} exactly once:\n{charter}"
            );
        }
        assert!(
            rule < repair && repair < exception,
            "{name} must state the rule before the repair clause, so it governs both, \
             and scope the failed-gate exception to the repair clause:\n{charter}"
        );
    }
}

#[test]
fn runtime_return_texts_interpolate_the_rule() {
    for (path, constant, expected) in RUNTIME_SITES {
        let source = read(path);
        assert_eq!(
            source.matches(&format!("{{{constant}}}")).count(),
            *expected,
            "{path} should interpolate {{{constant}}} {expected} time(s); a return or \
             recovery text was added or lost without updating this reviewed inventory"
        );
    }
}

#[test]
fn no_agent_facing_source_tells_agents_to_run_impacted_tests() {
    let offenders: Vec<String> = scanned_files()
        .iter()
        .filter(|(path, _)| !EXEMPT.iter().any(|(exempt, _)| exempt == path))
        .flat_map(|(path, text)| {
            retired_lines(text)
                .into_iter()
                .map(move |line| format!("{path}:{line}"))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "agent-facing sources still say \"{RETIRED}\"; state IMPLEMENTER_TEST_SCOPE instead \
         (SH-864), or add a reviewed EXEMPT entry with its reason:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn the_scan_flags_the_retired_wording_in_any_case() {
    let sample = "Fix the branch.\nRun new and impacted tests.\nImpacted-test selector.\nCommit.";
    assert_eq!(retired_lines(sample), vec![2, 3]);
    assert!(retired_lines(IMPLEMENTER_TEST_SCOPE).is_empty());
}

#[test]
fn every_exemption_names_a_scanned_file_and_a_reason() {
    let files = scanned_files();
    for (path, reason) in EXEMPT {
        assert!(
            files.contains_key(*path),
            "EXEMPT names {path}, which the scan does not read"
        );
        assert!(
            !reason.trim().is_empty(),
            "EXEMPT entry {path} gives no reason"
        );
    }
}

/// Stopped policy changes the central gate, never the implementer permission.
#[test]
fn stopped_verification_guidance_preserves_the_implementer_test_contract() {
    let sentence = "When verification is stopped, eligible submissions still publish and merge without tests. Release gates provide fallback coverage.";
    let legacy_root = storyhook_test_support::scratch_dir();
    storyhook::storage::init_project(legacy_root.path(), None).unwrap();
    for text in [
        templates::agents_md("SH", "done"),
        templates::cursor_rules(),
        read("AGENTS.md"),
        get_help_topic("agent-guide").unwrap().to_owned(),
        std::fs::read_to_string(legacy_root.path().join(".storyhook/CLAUDE.md")).unwrap(),
    ] {
        assert!(squash(&text).contains(sentence), "{text}");
        assert!(squash(&text).contains(IMPLEMENTER_TEST_SCOPE), "{text}");
    }
    let script = read("plugins/story/bin/story.sh");
    assert_eq!(
        charter_assignment(&script, "TEST_SCOPE_CLAUSE"),
        format!("TEST_SCOPE_CLAUSE=\"{IMPLEMENTER_TEST_SCOPE}\"")
    );
    assert_eq!(
        charter_assignment(&script, "STOPPED_VERIFICATION_CLAUSE"),
        format!("STOPPED_VERIFICATION_CLAUSE=\"{sentence}\"")
    );
    for name in ["PROMPT_TPL", "AUTO_PROMPT_TAIL"] {
        assert_eq!(
            charter_assignment(&script, name)
                .matches("$STOPPED_VERIFICATION_CLAUSE")
                .count(),
            1
        );
    }
}
