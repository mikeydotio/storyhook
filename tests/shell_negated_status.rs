//! No tracked shell script inverts a command's exit status with `if !` and
//! then reads `$?` inside the same then-block (SH-224).
//!
//! Under `!`, bash inverts the command's exit status, so `$?` inside the
//! then-branch of `if ! cmd; then` is the *inverted* value — always 0 — never
//! the command's own. `scripts/run-e2e.sh` shipped exactly this shape:
//! `echo "run-e2e.sh: the suite failed." >&2` printed on a failing Playwright
//! run, immediately followed by `exit "$status"` exiting 0, because `$?`
//! inside the negated `if` could never be anything else. `make test`'s only
//! browser-suite leg exited green for a `2 failed` Playwright run.
//!
//! The correct idiom, used by the fix, is `cmd || status=$?` — `||` does not
//! invert, so the assignment only runs (with the command's own status) when
//! the command fails.
//!
//! Derived from `git ls-files` rather than a hand-maintained list, the same
//! reasoning `tests/store_isolation.rs`'s harness scan documents: a list is
//! exactly the kind of thing that drifts out from under a check like this
//! one, and a scan cannot drift because it re-reads the tree it is asserting
//! about.

use std::path::Path;

/// Every tracked `*.sh` file, paired with its lines.
fn tracked_shell_scripts(root: &Path) -> Vec<(String, Vec<String>)> {
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z", "--", "*.sh"])
        .output()
        .expect("listing this repository's tracked shell scripts");
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so this scan proved nothing: {}",
        String::from_utf8_lossy(&listed.stderr)
    );

    let scripts: Vec<(String, Vec<String>)> = listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|path| {
            let relative = std::str::from_utf8(path).expect("a UTF-8 path").to_string();
            let text = std::fs::read_to_string(root.join(&relative))
                .unwrap_or_else(|e| panic!("reading {relative}: {e}"));
            let lines = text.lines().map(str::to_string).collect();
            (relative, lines)
        })
        .collect();

    // A scan that matches nothing passes every assertion built on top of it,
    // which would make a broken glob indistinguishable from a clean tree.
    assert!(
        scripts.len() >= 20,
        "this scan is supposed to find every tracked shell script, and it \
         found {}. The `git ls-files -- '*.sh'` pattern is broken, not the \
         scripts.",
        scripts.len()
    );
    scripts
}

/// Whether `line`'s first word is `if` or `elif`, immediately negated with
/// `!` — the shape that inverts its command's exit status.
fn opens_a_negated_conditional(line: &str) -> bool {
    let mut words = line.split_whitespace();
    matches!(words.next(), Some("if") | Some("elif")) && words.next() == Some("!")
}

/// Whether `word` ends the then-block a negated conditional opened, at the
/// same nesting depth: a sibling `elif`/`else`, or the `fi` that closes it.
fn ends_the_current_branch(word: &str) -> bool {
    matches!(word, "elif" | "else" | "fi")
}

/// Line numbers (1-indexed) of every `if !`/`elif !` in `lines` whose
/// then-block — up to its next `elif`/`else`/`fi` at the same nesting depth —
/// reads `$?`.
fn negated_conditionals_reading_exit_status(lines: &[String]) -> Vec<usize> {
    let mut findings = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if !opens_a_negated_conditional(line) {
            continue;
        }
        let mut depth = 0i32;
        for rest in &lines[i + 1..] {
            let word = rest.split_whitespace().next().unwrap_or("");
            if depth == 0 && ends_the_current_branch(word) {
                break;
            }
            if word == "if" {
                depth += 1;
            } else if word == "fi" {
                depth -= 1;
            } else if rest.contains("$?") {
                findings.push(i + 1);
                break;
            }
        }
    }
    findings
}

#[test]
fn no_negated_conditional_reads_its_own_inverted_exit_status() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    let offenders: Vec<String> = tracked_shell_scripts(root)
        .into_iter()
        .flat_map(|(relative, lines)| {
            negated_conditionals_reading_exit_status(&lines)
                .into_iter()
                .map(move |line_no| format!("{relative}:{line_no}"))
        })
        .collect();

    assert!(
        offenders.is_empty(),
        "{offenders:?} open an `if !`/`elif !` block and then read `$?` \
         inside it. `!` inverts bash's exit status, so `$?` there is always \
         0 regardless of whether the negated command actually failed \
         (SH-224 — scripts/run-e2e.sh exited 0 on a failing Playwright run). \
         Capture the real status with `cmd || status=$?` instead."
    );
}

#[test]
fn the_fixed_shape_does_not_trip_the_scan() {
    let lines: Vec<String> = [
        "status=0",
        "npx playwright test \"$@\" || status=$?",
        "if [ \"$status\" -ne 0 ]; then",
        "  exit \"$status\"",
        "fi",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();

    assert!(negated_conditionals_reading_exit_status(&lines).is_empty());
}

#[test]
fn the_buggy_shape_trips_the_scan() {
    let lines: Vec<String> = [
        "if ! npx playwright test \"$@\"; then",
        "  status=$?",
        "  exit \"$status\"",
        "fi",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();

    assert_eq!(negated_conditionals_reading_exit_status(&lines), vec![1]);
}

#[test]
fn a_negated_conditional_that_never_reads_status_is_fine() {
    let lines: Vec<String> = [
        "if ! tmux has-session -t \"$TARGET_SESSION\" 2>/dev/null; then",
        "  fail \"could not create session\"",
        "fi",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();

    assert!(negated_conditionals_reading_exit_status(&lines).is_empty());
}

#[test]
fn a_status_read_in_a_sibling_elif_branch_does_not_count() {
    let lines: Vec<String> = vec![
        "if ! cmd_a; then".to_string(),
        "  handle_a".to_string(),
        "elif ! cmd_b; then".to_string(),
        "  status=$?".to_string(),
        "  echo \"$status\"".to_string(),
        "fi".to_string(),
    ];

    // Only the second (`elif !`) branch reads its own status — the first
    // (`if !`) branch never does, and the two must not be conflated.
    assert_eq!(negated_conditionals_reading_exit_status(&lines), vec![3]);
}
