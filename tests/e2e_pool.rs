//! `scripts/e2e-pool.sh`, **provoked**: the browser leg's slice planner and its
//! bounded pool (SH-792).
//!
//! The release gate's browser leg used to run its Playwright projects one
//! after another, one worker each, for about 41 minutes. It now lists the
//! selection once, splits it into slices of whole spec files, and runs the
//! slices concurrently, each through its own isolated fixture. Two things have
//! to be true for that to be the same verdict, only sooner:
//!
//! - **The plan loses nothing and duplicates nothing.** Every selected file
//!   lands in exactly one slice of its own project, and the budget goes where
//!   the largest slice is.
//! - **The pool is a gate, not a best effort.** It never runs more than its
//!   bound, runs every slice even after one fails, reads a slice that died
//!   without a verdict as a failure rather than waiting for it forever, keeps
//!   each slice's output whole, and on a signal stops the writers before the
//!   shells whose cleanup deletes what they write.
//!
//! Every case drives the tracked library through a symlink (the
//! `tests/orphan_check.rs` convention), under macOS's `/bin/bash` 3.2 and the
//! caller's own `set -euo pipefail`, with stub runners in place of Playwright.
//! Ordering is proved with marker files and barriers, never with elapsed time.

use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use storyhook_test_support::{ChildGuard, run_bounded, scratch_dir_named};
use tempfile::TempDir;

/// Every scenario here finishes in a few seconds. This bound exists only so a
/// pool that never returns reports itself instead of holding the suite; it is
/// a hang detector, not a performance claim.
const SCENARIO_DEADLINE: Duration = Duration::from_secs(120);

/// How long the signal scenario may take to reach "both slices running".
const STARTUP_DEADLINE: Duration = Duration::from_secs(60);

fn checkout() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// A disposable root holding a symlink to the tracked library. `$M` in every
/// scenario is this root.
struct Fixture {
    root: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = scratch_dir_named("e2e-pool-");
        std::os::unix::fs::symlink(
            checkout().join("scripts/e2e-pool.sh"),
            root.path().join("e2e-pool.sh"),
        )
        .expect("symlinking the tracked library into the fixture");
        Self { root }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn file(&self, name: &str) -> PathBuf {
        self.path().join(name)
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.file(name)).unwrap_or_else(|e| panic!("reading marker {name}: {e}"))
    }

    fn exists(&self, name: &str) -> bool {
        self.file(name).exists()
    }

    /// Writes `body` as a `/bin/bash` script that runs under
    /// `set -euo pipefail` with the library sourced, and returns its path.
    fn script(&self, body: &str) -> PathBuf {
        let path = self.file("scenario.sh");
        let root = self.path().display();
        fs::write(
            &path,
            format!("set -euo pipefail\nM='{root}'\n. \"$M/e2e-pool.sh\"\n{body}\n"),
        )
        .expect("writing the scenario");
        path
    }

    fn command(&self, body: &str) -> Command {
        let mut cmd = Command::new("/bin/bash");
        cmd.arg(self.script(body)).current_dir(self.path());
        cmd
    }

    fn run(&self, body: &str) -> Output {
        run_bounded(
            self.command(body),
            "an e2e-pool scenario",
            storyhook_test_support::load_grace::graced_now(SCENARIO_DEADLINE),
        )
    }
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn assert_ok(out: &Output) {
    assert!(
        out.status.success(),
        "scenario failed: {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        stdout_of(out),
        stderr_of(out)
    );
}

// ---------------------------------------------------------------- counts ---

const LISTING: &str = "Listing tests:
  [chromium] › specs/a.spec.ts:3:5 › first
  [chromium] › specs/a.spec.ts:9:5 › group › second › with › nesting
  [chromium] › specs/b.spec.ts:3:5 › third
  [mobile-chromium] › specs/a.spec.ts:3:5 › first
  [untrusted-origin-chromium] › specs/untrusted-origin-cookie.spec.ts:12:1 › cookie
  [webkit] › specs/b.spec.ts:3:5 › third
Total: 6 tests in 3 files
";

#[test]
fn file_counts_group_tests_by_project_and_file_and_never_confuse_prefixed_projects() {
    let fx = Fixture::new();
    fs::write(fx.file("listing"), LISTING).unwrap();
    let out = fx.run(r#"e2e_pool_file_counts <"$M/listing""#);
    assert_ok(&out);
    assert_eq!(
        stdout_of(&out),
        "chromium\tspecs/a.spec.ts\t2\n\
         chromium\tspecs/b.spec.ts\t1\n\
         mobile-chromium\tspecs/a.spec.ts\t1\n\
         untrusted-origin-chromium\tspecs/untrusted-origin-cookie.spec.ts\t1\n\
         webkit\tspecs/b.spec.ts\t1\n"
    );
}

#[test]
fn file_counts_of_a_listing_that_selects_nothing_are_empty() {
    let fx = Fixture::new();
    fs::write(
        fx.file("listing"),
        "Listing tests:\nTotal: 0 tests in 0 files\n",
    )
    .unwrap();
    let out = fx.run(r#"e2e_pool_file_counts <"$M/listing""#);
    assert_ok(&out);
    assert_eq!(stdout_of(&out), "");
}

// ------------------------------------------------------------------ plan ---

/// Runs `e2e_pool_plan BUDGET "$M/lists" PROJECTS...` over `counts`.
fn plan(fx: &Fixture, counts: &str, budget: &str, projects: &str) -> Output {
    fs::write(fx.file("counts"), counts).unwrap();
    fx.run(&format!(
        r#"mkdir -p "$M/lists"
e2e_pool_plan {budget} "$M/lists" {projects} <"$M/counts""#
    ))
}

/// `(slice, project, test-list basename, count)` rows of a plan.
fn rows(fx: &Fixture, out: &Output) -> Vec<(String, String, String, u32)> {
    let lists = format!("{}/lists/", fx.path().display());
    stdout_of(out)
        .lines()
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            assert_eq!(fields.len(), 4, "a plan row has four fields: {line:?}");
            let list = fields[2]
                .strip_prefix(&lists)
                .unwrap_or_else(|| panic!("test-list {} is outside {lists}", fields[2]));
            (
                fields[0].to_owned(),
                fields[1].to_owned(),
                list.to_owned(),
                fields[3].parse().expect("a numeric count"),
            )
        })
        .collect()
}

const TWO_ENGINES: &str = "chromium\tspecs/a.spec.ts\t10
chromium\tspecs/b.spec.ts\t6
chromium\tspecs/c.spec.ts\t4
chromium\tspecs/d.spec.ts\t4
webkit\tspecs/a.spec.ts\t10
webkit\tspecs/b.spec.ts\t6
webkit\tspecs/c.spec.ts\t4
webkit\tspecs/d.spec.ts\t4
tiny\tspecs/t.spec.ts\t1
";

#[test]
fn the_plan_spends_the_budget_on_the_largest_slice_and_packs_files_longest_first() {
    let fx = Fixture::new();
    let out = plan(&fx, TWO_ENGINES, "4", "chromium webkit tiny");
    assert_ok(&out);
    // Each project starts with one slice; the fourth goes to the largest,
    // with the config order breaking chromium's tie with webkit. Packing is
    // longest first into the lightest bin: a(10) | b(6) | c(4) joins b |
    // d(4) joins the lower-numbered of two 10s. Admission is largest first.
    assert_eq!(
        rows(&fx, &out),
        vec![
            ("webkit".into(), "webkit".into(), "webkit.list".into(), 24),
            (
                "chromium.1of2".into(),
                "chromium".into(),
                "chromium.1of2.list".into(),
                14
            ),
            (
                "chromium.2of2".into(),
                "chromium".into(),
                "chromium.2of2.list".into(),
                10
            ),
            ("tiny".into(), "tiny".into(), "tiny.list".into(), 1),
        ]
    );
    // A test-list names whole files for its own project, in listing order,
    // in the `[project] › file` form Playwright's --test-list matches.
    assert_eq!(
        fx.read("lists/chromium.1of2.list"),
        "[chromium] › specs/a.spec.ts\n[chromium] › specs/d.spec.ts\n"
    );
    assert_eq!(
        fx.read("lists/chromium.2of2.list"),
        "[chromium] › specs/b.spec.ts\n[chromium] › specs/c.spec.ts\n"
    );
    assert_eq!(
        fx.read("lists/webkit.list"),
        "[webkit] › specs/a.spec.ts\n[webkit] › specs/b.spec.ts\n\
         [webkit] › specs/c.spec.ts\n[webkit] › specs/d.spec.ts\n"
    );
    assert_eq!(fx.read("lists/tiny.list"), "[tiny] › specs/t.spec.ts\n");
}

#[test]
fn every_project_gets_a_slice_and_none_gets_more_slices_than_files() {
    let fx = Fixture::new();
    let counts = "x\tspecs/x1.spec.ts\t5\nx\tspecs/x2.spec.ts\t5\ny\tspecs/y.spec.ts\t1\n";
    let out = plan(&fx, counts, "10", "x y");
    assert_ok(&out);
    assert_eq!(
        rows(&fx, &out),
        vec![
            ("x.1of2".into(), "x".into(), "x.1of2.list".into(), 5),
            ("x.2of2".into(), "x".into(), "x.2of2.list".into(), 5),
            ("y".into(), "y".into(), "y.list".into(), 1),
        ]
    );

    // A budget below the project count still runs every project.
    let fx = Fixture::new();
    let out = plan(&fx, TWO_ENGINES, "1", "chromium webkit tiny");
    assert_ok(&out);
    let slices: Vec<String> = rows(&fx, &out).into_iter().map(|r| r.0).collect();
    assert_eq!(slices, vec!["chromium", "webkit", "tiny"]);
}

#[test]
fn every_file_lands_in_exactly_one_slice_of_its_own_project_at_every_budget() {
    let files: Vec<(String, u32)> = (1..=13)
        .map(|i| (format!("specs/f{i:02}.spec.ts"), (i * 7) % 11 + 1))
        .collect();
    let mut counts = String::new();
    for project in ["p", "q"] {
        for (file, count) in &files {
            counts.push_str(&format!("{project}\t{file}\t{count}\n"));
        }
    }
    let total_per_project: u32 = files.iter().map(|(_, c)| c).sum();

    for budget in 1..=30 {
        let fx = Fixture::new();
        let out = plan(&fx, &counts, &budget.to_string(), "p q");
        assert_ok(&out);
        let rows = rows(&fx, &out);
        for project in ["p", "q"] {
            let mine: Vec<_> = rows.iter().filter(|r| r.1 == project).collect();
            assert!(
                !mine.is_empty() && mine.len() <= files.len(),
                "budget {budget}: {project} got {} slices",
                mine.len()
            );
            let mut seen: Vec<String> = Vec::new();
            let mut summed = 0;
            for (slice, _, list, count) in &mine {
                let text = fx.read(&format!("lists/{list}"));
                assert!(
                    !text.is_empty(),
                    "budget {budget}: {slice} is an empty slice"
                );
                for line in text.lines() {
                    let file = line
                        .strip_prefix(&format!("[{project}] › "))
                        .unwrap_or_else(|| panic!("{slice} lists a foreign line {line:?}"));
                    seen.push(file.to_owned());
                    summed += files
                        .iter()
                        .find(|(f, _)| f == file)
                        .expect("a known file")
                        .1;
                }
                let listed: u32 = text
                    .lines()
                    .map(|l| {
                        let f = l.split(" › ").nth(1).unwrap();
                        files.iter().find(|(name, _)| name == f).unwrap().1
                    })
                    .sum();
                assert_eq!(
                    listed, *count,
                    "budget {budget}: {slice}'s count is its files' sum"
                );
            }
            seen.sort();
            let expected: Vec<String> = files.iter().map(|(f, _)| f.clone()).collect();
            assert_eq!(
                seen, expected,
                "budget {budget}: {project}'s files, once each"
            );
            assert_eq!(summed, total_per_project);
        }
        let counts_in_order: Vec<u32> = rows.iter().map(|r| r.3).collect();
        let mut sorted = counts_in_order.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(
            counts_in_order, sorted,
            "budget {budget}: admission is largest first"
        );
    }
}

#[test]
fn a_project_that_selects_nothing_gets_no_slice() {
    let fx = Fixture::new();
    let out = plan(&fx, "a\tspecs/a.spec.ts\t3\n", "4", "a b");
    assert_ok(&out);
    assert_eq!(
        rows(&fx, &out),
        vec![("a".into(), "a".into(), "a.list".into(), 3)]
    );
    assert!(!fx.exists("lists/b.list"));
}

#[test]
fn the_plan_refuses_a_bad_budget_an_unsafe_name_a_foreign_project_and_a_missing_directory() {
    for budget in ["0", "-2", "1.5", "x", "''"] {
        let fx = Fixture::new();
        let out = plan(&fx, TWO_ENGINES, budget, "chromium webkit tiny");
        assert!(!out.status.success(), "budget {budget} was accepted");
        assert!(stderr_of(&out).contains("budget"), "{}", stderr_of(&out));
        assert_eq!(stdout_of(&out), "", "a refused plan prints no slices");
    }

    let fx = Fixture::new();
    let out = plan(&fx, "we/b\tspecs/a.spec.ts\t1\n", "4", "we/b");
    assert!(
        !out.status.success(),
        "a project name with a slash was accepted"
    );
    assert!(stderr_of(&out).contains("we/b"), "{}", stderr_of(&out));

    let fx = Fixture::new();
    let out = plan(&fx, TWO_ENGINES, "4", "chromium webkit");
    assert!(
        !out.status.success(),
        "a listing naming an unselected project was accepted"
    );
    assert!(stderr_of(&out).contains("tiny"), "{}", stderr_of(&out));

    let fx = Fixture::new();
    fs::write(fx.file("counts"), TWO_ENGINES).unwrap();
    let out = fx.run(r#"e2e_pool_plan 4 "$M/absent" chromium webkit tiny <"$M/counts""#);
    assert!(
        !out.status.success(),
        "a missing test-list directory was accepted"
    );
    assert!(stderr_of(&out).contains("absent"), "{}", stderr_of(&out));
}

// ------------------------------------------------------------------ pool ---

#[test]
fn the_pool_runs_jobs_slices_at_once_and_never_more() {
    let fx = Fixture::new();
    let out = fx.run(
        r#"mkdir "$M/running"
slice() {
  mkdir "$M/running/$1"
  if [ "$1" = C ]; then
    # C may start only once A or B has finished.
    if [ -e "$M/A.done" ] || [ -e "$M/B.done" ]; then echo after > "$M/C.start"; else echo early > "$M/C.start"; fi
  else
    tries=0
    until [ "$(ls "$M/running" | wc -l | tr -d ' ')" -ge 2 ]; do
      tries=$((tries + 1))
      if [ "$tries" -gt 300 ]; then echo timeout > "$M/$1.barrier"; break; fi
      sleep 0.1
    done
    [ -e "$M/$1.barrier" ] || echo met > "$M/$1.barrier"
  fi
  rmdir "$M/running/$1"
  : > "$M/$1.done"
}
e2e_pool_run 2 1 slice A B C"#,
    );
    assert_ok(&out);
    assert_eq!(
        fx.read("A.barrier"),
        "met\n",
        "A and B ran at the same time"
    );
    assert_eq!(
        fx.read("B.barrier"),
        "met\n",
        "A and B ran at the same time"
    );
    assert_eq!(fx.read("C.start"), "after\n", "C waited for a free slot");
}

#[test]
fn logs_replay_whole_and_in_admission_order_whatever_order_slices_finish_in() {
    let fx = Fixture::new();
    let out = fx.run(
        r#"slice() {
  case "$1" in
    A)
      echo "A first"
      tries=0
      until [ -e "$M/B.done" ]; do
        tries=$((tries + 1)); [ "$tries" -le 600 ] || { echo "A gave up"; return 1; }
        sleep 0.05
      done
      echo "A second" >&2
      ;;
    B) echo "B first"; echo "B second" >&2; : > "$M/B.done" ;;
  esac
}
e2e_pool_run 2 1 slice A B"#,
    );
    assert_ok(&out);
    let stdout = stdout_of(&out);
    let lines: Vec<&str> = stdout.lines().collect();
    let at = |needle: &str| {
        lines
            .iter()
            .position(|l| *l == needle)
            .unwrap_or_else(|| panic!("{needle:?} missing from:\n{stdout}"))
    };
    assert_eq!(
        at("A second"),
        at("A first") + 1,
        "A's log is whole:\n{stdout}"
    );
    assert_eq!(
        at("B second"),
        at("B first") + 1,
        "B's log is whole:\n{stdout}"
    );
    assert!(
        at("A second") < at("B first"),
        "admission order, not finish order:\n{stdout}"
    );

    // The live notes are progress, not a contract on order: two slices that
    // finish inside one poll are noted in admission order.
    let stderr = stderr_of(&out);
    for note in [
        "started A",
        "started B",
        "finished A: passed",
        "finished B: passed",
    ] {
        assert!(stderr.contains(note), "no {note:?} note in:\n{stderr}");
    }
}

#[test]
fn a_failing_slice_never_stops_the_others_and_any_failure_returns_one() {
    let fx = Fixture::new();
    let out = fx.run(
        r#"slice() {
  : > "$M/$1.ran"
  case "$1" in A) return 3 ;; B) return 0 ;; C) return 137 ;; esac
}
status=0
e2e_pool_run 2 1 slice A B C || status=$?
echo "$status" > "$M/status""#,
    );
    assert_ok(&out);
    for slice in ["A", "B", "C"] {
        assert!(fx.exists(&format!("{slice}.ran")), "{slice} never ran");
    }
    // Normalized: a slice's own 137 must not reach gate-legs.sh, which reads
    // any status of 125 or more as a cancelled gate.
    assert_eq!(fx.read("status"), "1\n");
    let summary = stdout_of(&out);
    for expected in [
        "A",
        "FAILED (exit 3)",
        "B",
        "passed",
        "C",
        "FAILED (exit 137)",
    ] {
        assert!(
            summary.contains(expected),
            "{expected:?} missing from:\n{summary}"
        );
    }

    let fx = Fixture::new();
    let out = fx.run(
        r#"slice() { : ; }
e2e_pool_run 3 1 slice A B C D E"#,
    );
    assert_ok(&out);
}

#[test]
fn a_slice_that_dies_without_a_verdict_is_a_failure_not_a_hang() {
    let fx = Fixture::new();
    let out = fx.run(
        r#"slice() {
  if [ "$1" = A ]; then sh -c 'kill -9 $PPID'; sleep 5; fi
  : > "$M/$1.ran"
}
status=0
e2e_pool_run 2 1 slice A B || status=$?
echo "$status" > "$M/status""#,
    );
    assert_ok(&out);
    assert_eq!(fx.read("status"), "1\n", "stdout:\n{}", stdout_of(&out));
    assert!(fx.exists("B.ran"));
    assert!(
        !fx.exists("A.ran"),
        "A's wrapper was killed before it finished"
    );
    assert!(
        stdout_of(&out).contains("no verdict"),
        "the summary says why A failed:\n{}",
        stdout_of(&out)
    );
}

#[test]
fn the_pool_refuses_bad_arguments_and_runs_nothing() {
    for (args, word) in [
        ("0 1 slice A", "jobs"),
        ("-1 1 slice A", "jobs"),
        ("x 1 slice A", "jobs"),
        ("'' 1 slice A", "jobs"),
        ("2 x slice A", "grace"),
        ("2 -1 slice A", "grace"),
        ("2 1 no_such_function A", "no_such_function"),
    ] {
        let fx = Fixture::new();
        let out = fx.run(&format!(
            r#"slice() {{ : > "$M/ran"; }}
status=0
e2e_pool_run {args} || status=$?
echo "$status" > "$M/status""#
        ));
        assert_ok(&out);
        assert_ne!(
            fx.read("status"),
            "0\n",
            "`e2e_pool_run {args}` was accepted"
        );
        assert!(
            stderr_of(&out).contains(word),
            "{args}: {}",
            stderr_of(&out)
        );
        assert!(!fx.exists("ran"), "`e2e_pool_run {args}` ran a slice");
    }
}

#[test]
fn the_callers_signal_traps_are_restored_when_the_pool_returns() {
    let fx = Fixture::new();
    let out = fx.run(
        r#"trap 'echo caller-term' TERM
slice() { : ; }
e2e_pool_run 1 1 slice A
trap -p TERM > "$M/term"
trap -p INT > "$M/int""#,
    );
    assert_ok(&out);
    assert_eq!(fx.read("term"), "trap -- 'echo caller-term' SIGTERM\n");
    assert_eq!(fx.read("int"), "");
}

/// A: its writer is a plain `sleep`; its shell's EXIT trap records whether the
/// writer was already gone when the cleanup ran. B: a writer that ignores
/// TERM and must be killed. C: never admitted.
const SIGNAL_SCENARIO: &str = r#"slice() {
  case "$1" in
    A)
      (
        trap 'if kill -0 "$(cat "$M/A.leaf")" 2>/dev/null; then echo alive; else echo gone; fi > "$M/A.cleanup"' EXIT
        sleep 1000 &
        echo "$!" > "$M/A.leaf"
        : > "$M/A.running"
        wait "$(cat "$M/A.leaf")"
      )
      ;;
    B)
      python3 -c 'import os, signal, sys, time
signal.signal(signal.SIGTERM, signal.SIG_IGN)
open(sys.argv[1] + "/B.pid", "w").write(str(os.getpid()))
open(sys.argv[1] + "/B.running", "w").close()
time.sleep(1000)' "$M"
      ;;
    C) : > "$M/C.started" ;;
  esac
}
trap 'echo ran > "$M/caller.exit"' EXIT
e2e_pool_run 2 1 slice A B C
echo returned > "$M/returned""#;

fn wait_for(fx: &Fixture, markers: &[&str], deadline: Duration) {
    let mut patience = storyhook_test_support::load_grace::Patience::new(deadline);
    while !markers.iter().all(|m| fx.exists(m)) {
        assert!(
            !patience.expired(),
            "{patience}; markers {markers:?} never all appeared (STARTUP_DEADLINE)"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn pid_alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(Stdio::null())
        .status()
        .expect("running kill -0")
        .success()
}

fn signal_stops_writers_first_and_re_raises(signal: &str, number: i32) {
    let fx = Fixture::new();
    let mut command = fx.command(SIGNAL_SCENARIO);
    command
        .stdout(fs::File::create(fx.file("stdout")).unwrap())
        .stderr(fs::File::create(fx.file("stderr")).unwrap());
    // Guarded from the instant it exists, so a panic before the wait below
    // cannot leak the scenario or its stubs (tests/fixture_isolation.rs).
    let mut child = ChildGuard::spawn(&mut command).expect("starting the signal scenario");
    wait_for(&fx, &["A.running", "B.running", "B.pid"], STARTUP_DEADLINE);

    let sent = Command::new("kill")
        .args([format!("-{signal}"), child.pid().to_string()])
        .status()
        .expect("sending the signal");
    assert!(sent.success());
    let status = child.wait_within(
        storyhook_test_support::load_grace::graced_now(SCENARIO_DEADLINE),
        || {
            format!(
                "the pool did not exit after its {signal} (SCENARIO_DEADLINE):\n{}",
                fx.read("stderr")
            )
        },
    );
    let stderr = fx.read("stderr");

    assert!(
        status.signal() == Some(number) || status.code() == Some(128 + number),
        "the pool must die of {signal}, not return a verdict: {status:?}\n{stderr}"
    );
    assert_eq!(
        fx.read("caller.exit"),
        "ran\n",
        "the caller's EXIT trap ran"
    );
    assert!(!fx.exists("returned"), "the pool returned into its caller");
    assert!(
        !fx.exists("C.started"),
        "an unstarted slice started after {signal}"
    );
    assert_eq!(
        fx.read("A.cleanup"),
        "gone\n",
        "A's shell cleaned up while its writer still ran:\n{stderr}"
    );
    assert!(!pid_alive(&fx.read("A.leaf")), "A's writer survived");
    assert!(
        !pid_alive(&fx.read("B.pid")),
        "B's TERM-proof writer survived:\n{stderr}"
    );
}

#[test]
fn a_term_stops_writers_before_shells_kills_the_stubborn_starts_nothing_new_and_re_raises() {
    signal_stops_writers_first_and_re_raises("TERM", 15);
}

#[test]
fn an_interrupt_is_handled_the_same_way() {
    signal_stops_writers_first_and_re_raises("INT", 2);
}
