//! `scripts/e2e-selection.sh`, **provoked** — the browser harness's listing
//! probe can no longer mistake a failed question for an empty answer (SH-625).
//!
//! # The defect
//!
//! `scripts/run-e2e.sh` asks Playwright `--list` whether a project selects any
//! test under the caller's filter before spending the real run on it. It used
//! to discard both stderr and the exit status and read the answer as text —
//! and Playwright prints the identical `Total: 0 tests in 0 files` when a
//! filter matches nothing and when any selected spec fails to load. A spec
//! that would not load, an unknown flag, or a config that failed to evaluate
//! was therefore reported as "selects no tests — skipping", the project's row
//! was emitted as `skipped`, and the run exited 0 having executed nothing.
//! SH-306's shape inside the browser harness, and the second time this one
//! script exited green for a run that proved nothing (SH-224 was the first).
//!
//! # The fix, and how it is proved
//!
//! Under `--pass-with-no-tests`, Playwright's own exit status separates the two:
//! 0 means "here is the selection, possibly empty", nonzero means "the
//! invocation failed" (the library's header carries the measured matrix). The
//! decision lives in a sourceable library so it can be driven here with a fake
//! `playwright` whose stdout, stderr and exit status are chosen per case — the
//! tracked library itself, reached by symlink in the `tests/orphan_check.rs`
//! convention, never a copy of its logic. A real run of `run-e2e.sh` needs a
//! `cargo build`, `e2e/node_modules`, two browsers and a daemon, which is why
//! the decision was extracted rather than tested by inspection.
//!
//! The wiring fences at the bottom are the other half: proving the library is
//! correct proves nothing if the runner still calls `--list` bare.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use storyhook_test_support::{run_bounded, scratch_dir};
use tempfile::TempDir;

/// A `bash` invocation of a sourced function that does no I/O beyond a fake
/// shim is over in milliseconds; this bound only exists so a wedged shell
/// reports itself instead of holding the suite (SH-528). It is a hang
/// detector, not an opinion about how fast a fork should be.
const SHELL_DEADLINE: Duration = Duration::from_secs(30);

/// The verdict codes `e2e-selection.sh` names, mirrored here from its own text
/// so a renumbering fails this file rather than silently changing the runner's
/// `case` arms.
const EMPTY: i32 = 3;
const UNREADABLE: i32 = 4;

fn checkout() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Reads a repository file, failing with the path rather than going quiet.
fn read_checkout_file(relative: &str) -> String {
    let path = checkout().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()))
}

/// A disposable root holding a symlink to the tracked library and a fake
/// `playwright` whose behaviour each case scripts.
struct Fixture {
    root: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = scratch_dir();
        fs::create_dir_all(root.path().join("scripts")).expect("creating scripts/");
        std::os::unix::fs::symlink(
            checkout().join("scripts/e2e-selection.sh"),
            root.path().join("scripts/e2e-selection.sh"),
        )
        .expect("symlinking the tracked library into the fixture");
        Self { root }
    }

    fn library(&self) -> PathBuf {
        self.root.path().join("scripts/e2e-selection.sh")
    }

    fn stderr_file(&self) -> PathBuf {
        self.root.path().join("playwright-list.stderr")
    }

    fn argv_log(&self) -> PathBuf {
        self.root.path().join("argv.log")
    }

    /// Writes a fake `playwright` that records its argv one per line, prints
    /// `stdout` and `stderr` verbatim, and exits `status`.
    fn fake_playwright(&self, stdout: &str, stderr: &str, status: i32) -> PathBuf {
        let path = self.root.path().join("playwright");
        let script = format!(
            "#!/usr/bin/env bash\nprintf '%s\\n' \"$@\" >{argv}\nprintf '%s' {out}\nprintf '%s' {err} >&2\nexit {status}\n",
            argv = shell_quote(&self.argv_log().to_string_lossy()),
            out = shell_quote(stdout),
            err = shell_quote(stderr),
        );
        fs::write(&path, script).expect("writing the fake playwright");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .expect("making the fake playwright executable");
        path
    }

    /// Runs `e2e_list_selection` from the sourced library against the fake at
    /// `fake`, passing `args` as the caller's own arguments (what run-e2e.sh
    /// passes: `test --project=… <filters>`).
    fn list_selection(&self, fake: &Path, args: &[&str]) -> Output {
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(". \"$1\"; shift; e2e_list_selection \"$@\"")
            .arg("e2e-selection-under-test")
            .arg(self.library())
            .arg(self.stderr_file())
            .arg(fake)
            .args(args)
            .current_dir(self.root.path());
        run_bounded(
            cmd,
            "e2e_list_selection against the fake playwright",
            SHELL_DEADLINE,
        )
    }

    /// Runs any one-liner with the library sourced.
    fn shell(&self, body: &str) -> Output {
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(format!(". \"$1\"; shift; {body}"))
            .arg("e2e-selection-under-test")
            .arg(self.library())
            .current_dir(self.root.path());
        run_bounded(cmd, body, SHELL_DEADLINE)
    }

    fn recorded_argv(&self) -> Vec<String> {
        fs::read_to_string(self.argv_log())
            .expect("the fake playwright records its argv")
            .lines()
            .map(str::to_string)
            .collect()
    }
}

/// Single-quotes `s` for embedding in a bash script.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn code_of(output: &Output) -> i32 {
    output
        .status
        .code()
        .unwrap_or_else(|| panic!("the shell was killed by a signal: {}", stderr_of(output)))
}

const FOUR_FILES: &str = "Listing tests:\n  [chromium] › column-visibility.spec.ts:12:5 › a\n  [chromium] › detail-panel.spec.ts:40:5 › b\nTotal: 34 tests in 4 files\n";
const NOTHING: &str = "Listing tests:\nTotal: 0 tests in 0 files\n";
const LOAD_ERROR: &str = "\nError: DASHBOARD_NAMED_TOKEN is not set — run this suite through scripts/run-e2e.sh\n\n   at support.ts:149\n    at Object.<anonymous> (e2e/specs/dispatch.spec.ts:54:42)\n";

#[test]
fn a_selection_with_tests_is_returned_verbatim_and_asked_for_with_the_discriminating_flag() {
    let fx = Fixture::new();
    let fake = fx.fake_playwright(FOUR_FILES, "load-grace: disabled\n", 0);
    let out = fx.list_selection(
        &fake,
        &[
            "test",
            "--project=chromium",
            "specs/column-visibility.spec.ts",
            "specs/detail-panel.spec.ts",
        ],
    );
    assert_eq!(code_of(&out), 0, "stderr: {}", stderr_of(&out));
    assert_eq!(
        stdout_of(&out),
        FOUR_FILES,
        "the listing reaches the caller unchanged"
    );
    assert_eq!(
        stderr_of(&out),
        "",
        "a successful listing says nothing on the harness's own stderr (Playwright's own \
         stderr went to the file)"
    );
    assert_eq!(
        fs::read_to_string(fx.stderr_file()).expect("the stderr file is written"),
        "load-grace: disabled\n"
    );

    // The caller's arguments first, in order, then the three flags that make
    // the exit status mean what this library says it means. Deleting
    // `--pass-with-no-tests` from the library is the single edit that reopens
    // SH-625 (an empty selection and a load error exit 1 alike without it),
    // and this is the assertion that edit fails.
    assert_eq!(
        fx.recorded_argv(),
        vec![
            "test",
            "--project=chromium",
            "specs/column-visibility.spec.ts",
            "specs/detail-panel.spec.ts",
            "--list",
            "--reporter=list",
            "--pass-with-no-tests",
        ]
    );
}

#[test]
fn a_listing_that_happened_and_selected_nothing_is_the_empty_verdict() {
    let fx = Fixture::new();
    let fake = fx.fake_playwright(NOTHING, "", 0);
    let out = fx.list_selection(&fake, &["test", "--project=chromium", "specs/nope.spec.ts"]);
    assert_eq!(code_of(&out), EMPTY, "stderr: {}", stderr_of(&out));
    assert_eq!(
        stdout_of(&out),
        "",
        "an empty verdict prints no listing to mistake for one"
    );
}

/// The story's exact defect: Playwright printed `Total: 0 tests` because a
/// selected spec threw at module load, and the wrapper read it as a skip.
#[test]
fn a_load_error_is_refused_by_name_never_skipped() {
    let fx = Fixture::new();
    let fake = fx.fake_playwright(NOTHING, LOAD_ERROR, 1);
    let out = fx.list_selection(
        &fake,
        &[
            "test",
            "--project=chromium",
            "specs/column-visibility.spec.ts",
            "specs/dispatch.spec.ts",
        ],
    );
    assert_eq!(
        code_of(&out),
        1,
        "a failed listing is a failure, not an empty selection"
    );
    assert_eq!(
        stdout_of(&out),
        "",
        "nothing on stdout, so a caller cannot read the refusal as a listing"
    );
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains(LOAD_ERROR),
        "Playwright's own stderr is replayed verbatim so the spec that would not load is \
         named; got:\n{stderr}"
    );
    assert!(
        stderr.contains("refusing, not skipping") && stderr.contains("(exit 1)"),
        "the refusal names itself and Playwright's status; got:\n{stderr}"
    );
    assert!(
        !stderr.contains("selects no tests"),
        "the phrase the defect reported must not appear on this path; got:\n{stderr}"
    );
}

#[test]
fn an_invocation_that_printed_nothing_is_still_a_failure() {
    let fx = Fixture::new();
    let fake = fx.fake_playwright("", "error: unknown option '--nope'\n", 1);
    let out = fx.list_selection(&fake, &["test", "--project=chromium", "--nope"]);
    assert_eq!(code_of(&out), 1);
    assert!(
        stderr_of(&out).contains("unknown option '--nope'"),
        "got:\n{}",
        stderr_of(&out)
    );
}

#[test]
fn a_successful_listing_with_no_readable_count_is_refused_not_read_as_zero() {
    let fx = Fixture::new();
    let fake = fx.fake_playwright("Listing tests:\n  [chromium] › a.spec.ts:1:1 › a\n", "", 0);
    let out = fx.list_selection(&fake, &["test", "--project=chromium"]);
    assert_eq!(code_of(&out), UNREADABLE, "stderr: {}", stderr_of(&out));
    assert_eq!(stdout_of(&out), "");
    assert!(
        stderr_of(&out).contains("no \"Total: N tests\" line"),
        "the refusal names the shape it could not read; got:\n{}",
        stderr_of(&out)
    );
}

#[test]
fn the_total_parser_reads_plural_singular_and_refuses_junk() {
    let fx = Fixture::new();
    for (listing, expected) in [
        ("Listing tests:\nTotal: 34 tests in 4 files\n", "34\n"),
        ("Total: 1 test in 1 file\n", "1\n"),
        ("Total: 0 tests in 0 files\n", "0\n"),
    ] {
        let out = fx.shell(&format!("e2e_selection_total {}", shell_quote(listing)));
        assert_eq!(code_of(&out), 0, "{listing:?}: {}", stderr_of(&out));
        assert_eq!(stdout_of(&out), expected, "{listing:?}");
    }
    let out = fx.shell("e2e_selection_total 'Listing tests:\n  nothing summarised'");
    assert_eq!(
        code_of(&out),
        1,
        "a listing without the summary line is not a count"
    );
    assert_eq!(stdout_of(&out), "");
}

/// The matrix in the library's header was measured against one Playwright.
/// The drift it cannot see at run time — a future Playwright exiting 0 on a
/// load error — would quietly reopen SH-625, so an upgrade of the pin has to
/// re-run the matrix and say so by updating the library's own record.
#[test]
fn the_measured_playwright_version_is_the_pinned_one() {
    let fx = Fixture::new();
    let measured = stdout_of(&fx.shell("e2e_selection_measured_playwright"));
    let package: serde_json::Value = serde_json::from_str(&read_checkout_file("e2e/package.json"))
        .expect("e2e/package.json parses");
    let pinned = package["devDependencies"]["@playwright/test"]
        .as_str()
        .expect("e2e/package.json pins @playwright/test");
    assert_eq!(
        measured.trim(),
        pinned,
        "e2e/package.json pins @playwright/test {pinned} but scripts/e2e-selection.sh's matrix \
         was measured on {}. Re-run the matrix in that file's header against the new pin \
         (in particular: does a spec that fails to load still exit nonzero under \
         --pass-with-no-tests?) and update e2e_selection_measured_playwright.",
        measured.trim()
    );
}

// --- Wiring: the runner actually goes through the library. -----------------

/// `text` with shell comments removed: a line whose first non-blank character
/// is `#` is dropped whole, and a trailing ` # …` is cut. Coarse on purpose —
/// nothing this file looks for sits inside a quoted `#`.
fn without_shell_comments(text: &str) -> String {
    text.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .map(|line| match line.find(" #") {
            Some(at) => &line[..at],
            None => line,
        })
        .map(|line| format!("{line}\n"))
        .collect()
}

fn offset_of(haystack: &str, needle: &str) -> usize {
    haystack
        .find(needle)
        .unwrap_or_else(|| panic!("scripts/run-e2e.sh must contain {needle:?}"))
}

#[test]
fn the_runner_lists_through_the_library_and_never_bare() {
    let runner = without_shell_comments(&read_checkout_file("scripts/run-e2e.sh"));

    assert!(
        runner.contains(". \"$repo_root/scripts/e2e-selection.sh\""),
        "scripts/run-e2e.sh must source scripts/e2e-selection.sh"
    );
    assert_eq!(
        runner.matches("e2e_list_selection ").count(),
        1,
        "exactly one listing probe, and it goes through the library"
    );
    // `|| list_status=$?`, never `if ! …` (SH-224, tests/shell_negated_status.rs),
    // and never `|| true`, which is the exact discard SH-625 was filed on.
    assert!(
        runner.contains("|| list_status=$?"),
        "the probe's exit status is captured, not discarded"
    );
    assert!(
        !runner.contains("--list"),
        "no non-comment line of scripts/run-e2e.sh may invoke --list itself; the flags \
         that make its exit status meaningful live in the library"
    );
    for arm in ["\"$E2E_SELECTION_EMPTY\")", "exit \"$list_status\""] {
        assert!(
            runner.contains(arm),
            "the runner branches on the library's verdict: missing {arm:?}"
        );
    }
    assert!(
        runner.contains("known_total=\"$(e2e_selection_total \"$list_output\")\""),
        "the checklist total is read through the same parser the verdict used"
    );
}

/// The comment stripper, provoked: it must drop the prose that mentions
/// `--list` and keep the code that does not.
#[test]
fn the_comment_stripper_can_tell_prose_from_code() {
    let stripped = without_shell_comments(
        "  # `--list` runs no global setup\n  x=1 # trailing --list\n  y=\"--list\"\n",
    );
    assert_eq!(stripped, "  x=1\n  y=\"--list\"\n");
}
