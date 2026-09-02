//! `scripts/select-tests.sh`, `gate-receipt.sh`'s `changed` tier, and the
//! wiring between them — provoked against real git, **not** inspected —
//! SH-429.
//!
//! # Why these tests are shaped this way
//!
//! Same doctrine `tests/push_gate.rs` and `tests/merge_gate.rs` already
//! state for their own scripts: every case here drives a **real git
//! repository**, invokes the **tracked scripts by absolute path** (never a
//! copy pasted into this file), and reads their **stdout/stderr contract**
//! rather than re-deriving the logic in Rust. Receipts are written through
//! the **production `gate-receipt.sh` writer** (`preflight`/`postlude`),
//! never hand-forged — the SH-297 anti-vacuity doctrine this project's other
//! gate suites already follow. Coverage maps are the one deliberate
//! exception: `coverage-map.sh`'s own production path requires an
//! instrumented build of a real cargo workspace, far too heavy for a fast
//! test fixture built from throwaway `.rs`-named text files, so maps here are
//! hand-written in the exact flat-TSV format `coverage-map.sh` itself
//! produces (verified by hand against a real capture during this story's own
//! development, recorded in `docs/spec/selective-testing.md`).
//!
//! No symlinking is needed for any of these scripts, unlike `push_gate.rs`'s
//! `.githooks` symlink: `select-tests.sh` is invoked by its real absolute
//! path in this checkout, so `${BASH_SOURCE[0]}`'s own directory — which it
//! uses to find `tracked-tree.sh` — resolves to the real `scripts/`
//! directory regardless of which fixture repository is the current working
//! directory.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use storyhook_test_support::scratch_dir;
use tempfile::TempDir;

#[cfg(target_os = "macos")]
struct ImmutableObjects {
    path: PathBuf,
    armed: bool,
}

#[cfg(target_os = "macos")]
impl ImmutableObjects {
    fn freeze(path: PathBuf) -> Self {
        let out = Command::new("chflags")
            .args(["-R", "uchg"])
            .arg(&path)
            .output()
            .expect("running chflags to freeze source objects");
        assert_ok(&out, "freezing source objects");
        Self { path, armed: true }
    }

    fn restore(&mut self) {
        let out = Command::new("chflags")
            .args(["-R", "nouchg"])
            .arg(&self.path)
            .output()
            .expect("running chflags to restore source objects");
        if out.status.success() {
            self.armed = false;
        }
        assert_ok(&out, "restoring source objects");
    }
}

#[cfg(target_os = "macos")]
impl Drop for ImmutableObjects {
    fn drop(&mut self) {
        if self.armed {
            let _ = Command::new("chflags")
                .args(["-R", "nouchg"])
                .arg(&self.path)
                .output();
        }
    }
}

fn checkout() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn script(name: &str) -> String {
    checkout().join("scripts").join(name).display().to_string()
}

struct SelectRepo {
    work: TempDir,
}

impl SelectRepo {
    /// A repo on `main` with one commit, no receipts, no maps — the fresh-
    /// clone state.
    fn new() -> Self {
        let repo = Self {
            work: scratch_dir(),
        };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.email", "t@t"]);
        repo.git(&["config", "user.name", "t"]);
        // `gate-receipt.sh preflight` refuses to enrol a worktree with no
        // `.githooks/pre-push` of its own — a symlink to the tracked
        // directory, the same fixture shape `tests/push_gate.rs` and
        // `tests/merge_gate.rs` already use, so the artifact under test is
        // the one that ships, never a copy.
        std::os::unix::fs::symlink(checkout().join(".githooks"), repo.path().join(".githooks"))
            .expect("fixture: linking the tracked hooks directory");
        repo.write("src/a.rs", "fn a() {}\n");
        repo.write("src/b.rs", "fn b() {}\n");
        repo.write(
            "tests/scanner_test.rs",
            "#[test] fn t() { std::fs::read_to_string(env!(\"CARGO_MANIFEST_DIR\")).ok(); }\n",
        );
        repo.write("README.md", "readme\n");
        repo.git(&["add", "-A"]);
        repo.git(&["commit", "-qm", "init"]);
        repo
    }

    fn path(&self) -> &Path {
        self.work.path()
    }

    fn write(&self, relative: &str, body: &str) {
        let full = self.path().join(relative);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("fixture: creating parent dirs");
        }
        std::fs::write(full, body).expect("fixture: writing a tracked file");
    }

    fn git(&self, args: &[&str]) -> Output {
        run(self.path(), "git", args)
    }

    fn commit(&self, relative: &str, body: &str, message: &str) {
        self.write(relative, body);
        self.git(&["add", "-A"]);
        self.git(&["commit", "-qm", message]);
    }

    fn tree(&self) -> String {
        String::from_utf8_lossy(&self.git(&["rev-parse", "HEAD^{tree}"]).stdout)
            .trim()
            .to_string()
    }

    fn common_dir(&self) -> PathBuf {
        // No linked worktrees in this fixture, so `--git-common-dir` is
        // just `.git`.
        self.path().join(".git")
    }

    /// The production receipt writer — `gate-receipt.sh preflight` then
    /// `postlude <tier> [<base>]` — certifying whatever is currently checked
    /// out.
    fn certify(&self, tier: &str, base: Option<&str>) -> Output {
        assert_ok(
            &run(
                self.path(),
                "bash",
                &[&script("gate-receipt.sh"), "preflight"],
            ),
            "fixture: enrolling",
        );
        let mut args = vec![
            script("gate-receipt.sh"),
            "postlude".to_string(),
            tier.to_string(),
        ];
        if let Some(b) = base {
            args.push(b.to_string());
        }
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run(self.path(), "bash", &arg_refs)
    }

    /// Hand-writes a coverage map in `coverage-map.sh`'s own flat-TSV format
    /// — the one deliberate exception to "production writer only", explained
    /// in this file's own module doc.
    fn write_map(&self, for_tree: &str, entries: &[(&str, &str)]) {
        let dir = self.common_dir().join("storyhook").join("coverage-maps");
        std::fs::create_dir_all(&dir).expect("fixture: creating coverage-maps dir");
        let mut lines: Vec<String> = entries
            .iter()
            .map(|(binary, file)| format!("{binary}\t{file}"))
            .collect();
        lines.sort();
        std::fs::write(dir.join(for_tree), lines.join("\n") + "\n")
            .expect("fixture: writing a coverage map");
    }

    fn select_tests(&self) -> Output {
        run(self.path(), "bash", &[&script("select-tests.sh")])
    }

    #[cfg(target_os = "macos")]
    fn select_tests_with_tmpdir(&self, tmpdir: &Path) -> Output {
        Command::new("bash")
            .arg(script("select-tests.sh"))
            .current_dir(self.path())
            .env("TMPDIR", tmpdir)
            .output()
            .expect("running select-tests.sh with an isolated temporary root")
    }
}

fn run(cwd: &Path, program: &str, args: &[&str]) -> Output {
    Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("running {program}: {e}"))
}

fn assert_ok(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} should have succeeded\nstdout: {}\nstderr: {}",
        stdout(out),
        stderr(out)
    );
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// Lines of stdout after the first (`BASELINE ...`), trimmed of blanks.
fn selection_lines(out: &Output) -> Vec<String> {
    stdout(out)
        .lines()
        .skip(1)
        .map(str::to_string)
        .filter(|l| !l.is_empty())
        .collect()
}

fn baseline_line(out: &Output) -> String {
    stdout(out).lines().next().unwrap_or_default().to_string()
}

// ---------------------------------------------------------------------------
// select-tests.sh: the three escape hatches
// ---------------------------------------------------------------------------

/// No ancestor of `HEAD` carries a `gate`/`full` receipt at all — the
/// fresh-clone state. `BASELINE NONE` and `ALL`, exactly, is the whole
/// contract for this case.
#[test]
fn no_ancestor_with_a_receipt_means_baseline_none_and_all() {
    let repo = SelectRepo::new();

    let out = repo.select_tests();

    assert_ok(&out, "select-tests.sh with no receipts anywhere");
    assert_eq!(baseline_line(&out), "BASELINE NONE");
    assert_eq!(selection_lines(&out), vec!["ALL".to_string()]);
}

/// A baseline is found, but no coverage map exists for it — escape hatch 1.
#[test]
fn a_baseline_with_no_coverage_map_means_all() {
    let repo = SelectRepo::new();
    let base = repo.tree();
    assert_ok(
        &repo.certify("gate", None),
        "fixture: certifying the baseline",
    );

    repo.commit("src/a.rs", "fn a2() {}\n", "touch a.rs, no map exists");

    let out = repo.select_tests();

    assert_ok(&out, "select-tests.sh with a baseline but no map");
    assert_eq!(baseline_line(&out), format!("BASELINE {base}"));
    assert_eq!(selection_lines(&out), vec!["ALL".to_string()]);
    assert!(
        stderr(&out).contains("no coverage map"),
        "the reason must be named, got: {}",
        stderr(&out)
    );
}

/// A changed path outside `src/**.rs`, `crates/**.rs`, `tests/*.rs` — escape
/// hatch 2. README.md is the fixture's own representative of "everything
/// else": `Makefile`, `scripts/`, `.githooks/`, `web_dashboard.html` are all
/// this same shape.
#[test]
fn a_changed_path_outside_the_covered_globs_means_all() {
    let repo = SelectRepo::new();
    let base = repo.tree();
    assert_ok(
        &repo.certify("gate", None),
        "fixture: certifying the baseline",
    );
    repo.write_map(&base, &[("story_priority", "src/a.rs")]);

    repo.commit(
        "README.md",
        "readme, changed\n",
        "touch a file outside the covered globs",
    );

    let out = repo.select_tests();

    assert_ok(
        &out,
        "select-tests.sh with a change outside the covered globs",
    );
    assert_eq!(selection_lines(&out), vec!["ALL".to_string()]);
    assert!(
        stderr(&out).contains("README.md"),
        "the offending path must be named, got: {}",
        stderr(&out)
    );
}

// ---------------------------------------------------------------------------
// select-tests.sh: the ordinary selective path
// ---------------------------------------------------------------------------

/// The flagship case: a `src/**.rs` change the map attributes to one binary
/// selects exactly that binary, plus the always-on tree-scanning set.
#[test]
fn a_covered_src_file_change_selects_its_mapped_binary() {
    let repo = SelectRepo::new();
    let base = repo.tree();
    assert_ok(
        &repo.certify("gate", None),
        "fixture: certifying the baseline",
    );
    repo.write_map(
        &base,
        &[
            ("story_priority", "src/a.rs"),
            ("story_summary", "src/b.rs"),
        ],
    );

    repo.commit("src/a.rs", "fn a2() {}\n", "touch a.rs only");

    let out = repo.select_tests();

    assert_ok(&out, "select-tests.sh over a mapped src change");
    assert_eq!(baseline_line(&out), format!("BASELINE {base}"));
    let mut selected = selection_lines(&out);
    selected.sort();
    assert_eq!(
        selected,
        vec!["scanner_test".to_string(), "story_priority".to_string()],
        "must select exactly the mapped binary plus the tree-scanning set, got: {selected:?}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_dirty_tree_is_diffed_selectively_with_immutable_source_objects_and_no_leftovers() {
    let repo = SelectRepo::new();
    let base = repo.tree();
    assert_ok(
        &repo.certify("gate", None),
        "fixture: certifying the baseline",
    );
    repo.write_map(&base, &[("story_priority", "src/a.rs")]);
    repo.write("src/a.rs", "fn dirty_without_a_commit() {}\n");

    let private_tmp = scratch_dir();
    let mut immutable = ImmutableObjects::freeze(repo.common_dir().join("objects"));
    let out = repo.select_tests_with_tmpdir(private_tmp.path());
    immutable.restore();

    assert_ok(
        &out,
        "select-tests.sh over a dirty tree with immutable source objects",
    );
    assert_eq!(baseline_line(&out), format!("BASELINE {base}"));
    let mut selected = selection_lines(&out);
    selected.sort();
    assert_eq!(
        selected,
        vec!["scanner_test".to_string(), "story_priority".to_string()],
        "the private dirty-tree diff must drive the ordinary selective result"
    );
    let leftovers = std::fs::read_dir(private_tmp.path())
        .expect("reading the selector's isolated temporary root")
        .map(|entry| entry.expect("reading a temporary entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("storyhook-select-tests-objects."))
        })
        .collect::<Vec<_>>();
    assert!(
        leftovers.is_empty(),
        "select-tests.sh must remove its private object store; left: {:?}",
        leftovers.iter().collect::<Vec<_>>()
    );
}

/// A changed `tests/*.rs` file selects its OWN binary even though the map —
/// captured before this file existed — has never heard of it.
#[test]
fn a_changed_test_file_selects_its_own_binary_even_absent_from_the_map() {
    let repo = SelectRepo::new();
    let base = repo.tree();
    assert_ok(
        &repo.certify("gate", None),
        "fixture: certifying the baseline",
    );
    repo.write_map(&base, &[("story_priority", "src/a.rs")]);

    repo.write("tests/story_summary.rs", "#[test] fn t() {}\n");
    repo.git(&["add", "-A"]);
    repo.git(&[
        "commit",
        "-qm",
        "add a new test file the map has never seen",
    ]);

    let out = repo.select_tests();

    assert_ok(&out, "select-tests.sh over a brand-new test file");
    let mut selected = selection_lines(&out);
    selected.sort();
    assert_eq!(
        selected,
        vec!["scanner_test".to_string(), "story_summary".to_string()],
        "got: {selected:?}"
    );
}

/// The derived tree-scanning set — `tests/scanner_test.rs` in this fixture,
/// which reads `CARGO_MANIFEST_DIR` — is selected on EVERY selective run,
/// never just when its own file changed.
#[test]
fn the_tree_scanning_set_is_always_selected_regardless_of_the_map() {
    let repo = SelectRepo::new();
    let base = repo.tree();
    assert_ok(
        &repo.certify("gate", None),
        "fixture: certifying the baseline",
    );
    // Deliberately does NOT mention scanner_test.rs at all.
    repo.write_map(&base, &[("story_priority", "src/a.rs")]);

    repo.commit("src/a.rs", "fn a2() {}\n", "touch a.rs");

    let out = repo.select_tests();

    let selected = selection_lines(&out);
    assert!(
        selected.contains(&"scanner_test".to_string()),
        "the tree-scanning set must always be present, got: {selected:?}"
    );
}

/// A tracked file the map genuinely has no entry for, and that is not a
/// tests/*.rs file, selects nothing beyond the tree-scanning set — the
/// correct reading (nothing observed exercises it) rather than a gap.
#[test]
fn a_tracked_file_absent_from_the_map_selects_only_the_tree_scanning_set() {
    let repo = SelectRepo::new();
    repo.commit(
        "src/uncovered.rs",
        "fn u() {}\n",
        "add a file the map will never mention",
    );
    let base = repo.tree();
    assert_ok(
        &repo.certify("gate", None),
        "fixture: certifying the baseline",
    );
    repo.write_map(&base, &[("story_priority", "src/a.rs")]);

    repo.commit(
        "src/uncovered.rs",
        "fn u2() {}\n",
        "touch the uncovered file",
    );

    let out = repo.select_tests();

    assert_eq!(selection_lines(&out), vec!["scanner_test".to_string()]);
}

/// No changes since the baseline at all — a degenerate but valid case:
/// `BASELINE` is still printed, and the selection is empty (never `ALL`).
#[test]
fn no_changes_since_the_baseline_selects_nothing() {
    let repo = SelectRepo::new();
    let base = repo.tree();
    assert_ok(
        &repo.certify("gate", None),
        "fixture: certifying the baseline",
    );
    repo.write_map(&base, &[("story_priority", "src/a.rs")]);

    let out = repo.select_tests();

    assert_ok(&out, "select-tests.sh with nothing changed");
    assert_eq!(baseline_line(&out), format!("BASELINE {base}"));
    assert!(
        selection_lines(&out).is_empty(),
        "got: {:?}",
        selection_lines(&out)
    );
}

/// Only the NEAREST fully-certified tree counts — a `changed`-tier receipt
/// sitting between the true baseline and HEAD must not be mistaken for one.
/// This is the one-hop invariant `docs/spec/selective-testing.md` states:
/// select-tests.sh never diffs against a previous SELECTIVE run.
#[test]
fn a_changed_tier_receipt_is_never_treated_as_a_baseline() {
    let repo = SelectRepo::new();
    let true_base = repo.tree();
    assert_ok(
        &repo.certify("gate", None),
        "fixture: certifying the true baseline",
    );
    repo.write_map(&true_base, &[("story_priority", "src/a.rs")]);

    repo.commit("src/a.rs", "fn a2() {}\n", "an intermediate commit");
    let intermediate = repo.tree();
    assert_ok(
        &repo.certify("changed", Some(&true_base)),
        "fixture: certifying the intermediate commit at `changed` tier",
    );

    repo.commit("src/b.rs", "fn b2() {}\n", "the tip");

    let out = repo.select_tests();

    // The baseline resolved must be the TRUE (gate) baseline, not the
    // intermediate `changed` receipt — proving the walk skips past it.
    assert_eq!(baseline_line(&out), format!("BASELINE {true_base}"));
    let _ = intermediate; // silence unused-var lint if the walk is ever changed to log it
}

// ---------------------------------------------------------------------------
// The bash 3.2 regression this story's own development hit directly
// ---------------------------------------------------------------------------

/// macOS's system `bash` is 3.2 (frozen there by the GPLv3), which can
/// misparse a bare `*)` `case` arm sitting inside a `$(...)` command
/// substitution — measured directly during this story's own development:
/// `select-tests.sh`'s selection loop (a `case` over each changed file,
/// distinguishing a `tests/*.rs` file from everything else) sits inside
/// exactly that shape, and every pattern there now carries a leading `(`
/// specifically to avoid it. This test provokes the exact combination that
/// exercises both arms of that `case` in one run — a changed `tests/*.rs`
/// file AND a changed, mapped `src/*.rs` file together — so a regression
/// that removes the leading parens fails this test with a shell syntax
/// error rather than staying invisible until someone happens to run the
/// suite on this exact platform.
#[test]
fn a_selection_spanning_a_test_file_and_a_mapped_src_file_does_not_hit_the_bash_32_parser_bug() {
    let repo = SelectRepo::new();
    let base = repo.tree();
    assert_ok(
        &repo.certify("gate", None),
        "fixture: certifying the baseline",
    );
    repo.write_map(&base, &[("story_priority", "src/a.rs")]);

    repo.write("tests/story_summary.rs", "#[test] fn t() {}\n");
    repo.write("src/a.rs", "fn a2() {}\n");
    repo.git(&["add", "-A"]);
    repo.git(&[
        "commit",
        "-qm",
        "touch both a mapped src file and a new test file",
    ]);

    let out = repo.select_tests();

    assert_ok(
        &out,
        &format!(
            "select-tests.sh must not hit a shell parse error\nstderr: {}",
            stderr(&out)
        ),
    );
    assert!(
        !stderr(&out).contains("syntax error"),
        "a bash 3.2 case-parsing regression, got stderr: {}",
        stderr(&out)
    );
    let mut selected = selection_lines(&out);
    selected.sort();
    assert_eq!(
        selected,
        vec![
            "scanner_test".to_string(),
            "story_priority".to_string(),
            "story_summary".to_string()
        ],
        "got: {selected:?}"
    );
}

// ---------------------------------------------------------------------------
// gate-receipt.sh: the `changed` tier
// ---------------------------------------------------------------------------

/// `postlude changed` with no base tree at all is refused.
#[test]
fn postlude_changed_needs_a_base() {
    let repo = SelectRepo::new();
    assert_ok(
        &run(
            repo.path(),
            "bash",
            &[&script("gate-receipt.sh"), "preflight"],
        ),
        "fixture: enrolling",
    );

    let out = run(
        repo.path(),
        "bash",
        &[&script("gate-receipt.sh"), "postlude", "changed"],
    );

    assert!(!out.status.success(), "must refuse with no base");
    assert!(
        stderr(&out).contains("needs a base tree"),
        "got: {}",
        stderr(&out)
    );
}

/// `postlude changed <base>` is refused when `<base>` carries no
/// `gate`/`full` receipt of its own — a `changed` receipt must name a tree
/// that was itself fully certified, never another selective run or nothing
/// at all.
#[test]
fn postlude_changed_base_must_carry_a_gate_or_full_receipt() {
    let repo = SelectRepo::new();
    assert_ok(
        &run(
            repo.path(),
            "bash",
            &[&script("gate-receipt.sh"), "preflight"],
        ),
        "fixture: enrolling",
    );

    let out = run(
        repo.path(),
        "bash",
        &[
            &script("gate-receipt.sh"),
            "postlude",
            "changed",
            "0000000000000000000000000000000000000000",
        ],
    );

    assert!(!out.status.success(), "must refuse an uncertified base");
    assert!(
        stderr(&out).contains("carries no gate/full receipt"),
        "got: {}",
        stderr(&out)
    );
}

/// A `changed` receipt for a tree that already carries `gate` must not
/// downgrade it — the general "weaker tier does not overwrite a stronger
/// receipt" rule, extended from `full`-over-`gate` to the three-tier order
/// `changed < gate < full`.
#[test]
fn a_changed_receipt_does_not_downgrade_an_existing_gate_receipt() {
    let repo = SelectRepo::new();
    assert_ok(
        &repo.certify("gate", None),
        "fixture: writing the gate receipt",
    );
    let tree = repo.tree();

    let out = repo.certify("changed", Some(&tree));

    assert_ok(
        &out,
        "a changed-tier postlude over an already-gate tree must not error",
    );
    assert!(
        stderr(&out).contains("does not downgrade"),
        "got: {}",
        stderr(&out)
    );
    let receipt = std::fs::read_to_string(
        repo.common_dir()
            .join("storyhook/gate-receipts")
            .join(&tree),
    )
    .expect("reading the receipt");
    assert!(receipt.contains("tier gate"), "got: {receipt}");
}

/// The reverse direction: certifying at `full` over an existing `changed`
/// receipt for the same tree is an upgrade, and must succeed, replacing the
/// tier and dropping the `base` line (a `full` receipt makes no claim
/// relative to a baseline — it is unconditional).
#[test]
fn a_full_receipt_upgrades_an_existing_changed_receipt() {
    let repo = SelectRepo::new();
    let base = repo.tree();
    assert_ok(&repo.certify("gate", None), "fixture: certifying the base");

    repo.commit("src/a.rs", "fn a2() {}\n", "the tip");
    let tip = repo.tree();
    assert_ok(
        &repo.certify("changed", Some(&base)),
        "fixture: writing a changed receipt",
    );

    let out = repo.certify("full", None);

    assert_ok(
        &out,
        "a full-tier postlude must upgrade an existing changed receipt",
    );
    let receipt =
        std::fs::read_to_string(repo.common_dir().join("storyhook/gate-receipts").join(&tip))
            .expect("reading the receipt");
    assert!(receipt.contains("tier full"), "got: {receipt}");
    assert!(
        !receipt.contains("base "),
        "a full receipt must carry no base line, got: {receipt}"
    );
}

/// A genuine `changed` receipt records its base tree, verbatim.
#[test]
fn a_changed_receipt_records_its_base_tree() {
    let repo = SelectRepo::new();
    let base = repo.tree();
    assert_ok(&repo.certify("gate", None), "fixture: certifying the base");

    repo.commit("src/a.rs", "fn a2() {}\n", "the tip");
    let tip = repo.tree();

    assert_ok(
        &repo.certify("changed", Some(&base)),
        "fixture: writing the changed receipt",
    );

    let receipt =
        std::fs::read_to_string(repo.common_dir().join("storyhook/gate-receipts").join(&tip))
            .expect("reading the receipt");
    assert!(receipt.contains("tier changed"), "got: {receipt}");
    assert!(receipt.contains(&format!("base {base}")), "got: {receipt}");
}

// ---------------------------------------------------------------------------
// Derived fences
// ---------------------------------------------------------------------------

/// `pub(crate) const SHUTDOWN_CHECK: Duration = Duration::from_millis(<n>);`
/// in `src/daemon/serve.rs` — read as source text rather than imported,
/// mirroring `tests/orphan_check.rs`'s own identical need: the constant is
/// deliberately `pub(crate)`, and widening its visibility just so a test
/// could import it would be a production change in service of the test.
fn shutdown_check_ms() -> u64 {
    let src = std::fs::read_to_string(checkout().join("src/daemon/serve.rs"))
        .expect("reading src/daemon/serve.rs");
    let marker = "pub(crate) const SHUTDOWN_CHECK: Duration = Duration::from_millis(";
    let after = src
        .split(marker)
        .nth(1)
        .unwrap_or_else(|| panic!("src/daemon/serve.rs must declare `{marker}<n>);`"));
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits
        .parse()
        .unwrap_or_else(|e| panic!("SHUTDOWN_CHECK's value {digits:?} must parse: {e}"))
}

/// `readonly PARENT_WATCH_GRACE_SECS=<n>` in `scripts/coverage-map.sh`.
fn parent_watch_grace_secs() -> u64 {
    let src = std::fs::read_to_string(checkout().join("scripts/coverage-map.sh"))
        .expect("reading scripts/coverage-map.sh");
    let marker = "readonly PARENT_WATCH_GRACE_SECS=";
    let after = src
        .split(marker)
        .nth(1)
        .unwrap_or_else(|| panic!("scripts/coverage-map.sh must declare `{marker}<n>`"));
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits
        .parse()
        .unwrap_or_else(|e| panic!("PARENT_WATCH_GRACE_SECS's value {digits:?} must parse: {e}"))
}

/// The same 20x floor `tests/orphan_check.rs` already applies to
/// `ORPHAN_GRACE_SECS` against the identical mechanism — half of today's
/// actual 40x ratio (10s / 250ms), generous headroom against a small,
/// deliberate change to either constant while still catching the shape of
/// drift that matters: the grace period being cut without re-deriving it
/// from the parent-watch tick it is waiting on.
const MIN_GRACE_MULTIPLE_OF_SHUTDOWN_CHECK: u64 = 20;

#[test]
fn coverage_maps_parent_watch_grace_is_derived_from_the_tick_it_disproves() {
    let grace_ms = parent_watch_grace_secs() * 1000;
    let tick_ms = shutdown_check_ms();
    assert!(
        grace_ms >= tick_ms * MIN_GRACE_MULTIPLE_OF_SHUTDOWN_CHECK,
        "scripts/coverage-map.sh's PARENT_WATCH_GRACE_SECS ({grace_ms}ms) has drifted too \
         close to src/daemon/serve.rs's SHUTDOWN_CHECK ({tick_ms}ms) it derives from"
    );
}

/// `test-changed` must enrol first, wrap its fallible body, and certify last,
/// exactly like `test`
/// (`tests/push_gate.rs::the_makefile_enrolls_first_and_certifies_last`
/// pins `test` itself) — the invariant that makes "no receipt unless every
/// leg and orphan cleanup passed" true by make's own fail-fast semantics.
#[test]
fn test_changed_enrolls_first_and_certifies_last() {
    let makefile = std::fs::read_to_string(checkout().join("Makefile")).expect("reading Makefile");

    let body: Vec<&str> = makefile
        .lines()
        .skip_while(|l| !l.starts_with("test-changed:"))
        .skip(1)
        .take_while(|l| l.starts_with('\t'))
        .collect();
    assert!(
        body.len() > 3,
        "the test-changed target's recipe was not found — this fence is broken, not the Makefile"
    );

    assert!(
        body[0].contains("gate-receipt.sh preflight"),
        "the first line of test-changed must enroll, got: {}",
        body[0]
    );
    assert!(
        body[1].contains("with-orphan-postlude.sh") && body[1].contains("_test-changed-body"),
        "the second line of test-changed must wrap its fallible body, got: {}",
        body[1]
    );
    let last_nonblank = body
        .iter()
        .rev()
        .find(|l| !l.trim().is_empty())
        .expect("the recipe must have a last line");
    assert!(
        last_nonblank.contains("gate-receipt.sh postlude"),
        "the LAST line of test-changed must write the receipt, got: {last_nonblank}"
    );
}
