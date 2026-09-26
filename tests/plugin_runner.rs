//! The plugin leg's runner, `plugins/story/tests/run-tests.sh` (SH-783).
//!
//! Driven over a fixture directory of small `test-*.sh` scripts, never over the
//! real suite: what is under test is the runner's own contract. It runs the
//! scripts up to `STORYHOOK_PLUGIN_JOBS` at a time, runs a script marked
//! `# plugin-runner: serial` alone after the pool, reports every script in one
//! fixed order whatever order they finish in, and takes its running scripts
//! with it when it is terminated.
//!
//! The runner resolves its helpers relative to its own directory, so the
//! fixture mirrors the checkout's layout with symlinks to the tracked files.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use storyhook_test_support::{ChildGuard, scratch_dir};
use tempfile::TempDir;

/// How long a fixture script waits for a sibling that runs at the same time.
/// Generous on purpose (SH-394): it bounds a failure, and a passing run never
/// waits it out.
const OVERLAP_PATIENCE_SECS: u64 = 30;

fn checkout() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// A checkout-shaped directory holding the tracked runner and fixture scripts.
struct Suite {
    root: TempDir,
}

impl Suite {
    fn new() -> Self {
        let root = scratch_dir();
        let tests = root.path().join("plugins/story/tests");
        let scripts = root.path().join("scripts");
        fs::create_dir_all(&tests).expect("fixture: creating the tests dir");
        fs::create_dir_all(&scripts).expect("fixture: creating scripts/");
        symlink(
            checkout().join("plugins/story/tests/run-tests.sh"),
            tests.join("run-tests.sh"),
        )
        .expect("fixture: linking the runner");
        for helper in ["gate-progress.sh", "test-env.sh"] {
            symlink(
                checkout().join("scripts").join(helper),
                scripts.join(helper),
            )
            .expect("fixture: linking a runner helper");
        }
        fs::create_dir(root.path().join("shared")).expect("fixture: creating shared/");
        Self { root }
    }

    /// Where fixture scripts leave markers for each other and for assertions.
    fn shared(&self) -> PathBuf {
        self.root.path().join("shared")
    }

    /// Writes `test-<name>.sh` with `body` after a strict-mode preamble.
    fn script(&self, name: &str, body: &str) {
        let path = self
            .root
            .path()
            .join(format!("plugins/story/tests/test-{name}.sh"));
        fs::write(&path, format!("#!/usr/bin/env bash\nset -u\n{body}\n"))
            .expect("fixture: writing a test script");
        let mut perms = fs::metadata(&path).expect("fixture: stat").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("fixture: chmod");
    }

    /// The runner, with `jobs` as its job limit and nothing inherited that
    /// names a gate journal.
    fn command(&self, jobs: &str) -> Command {
        let mut command = Command::new("bash");
        command
            .arg(self.root.path().join("plugins/story/tests/run-tests.sh"))
            .env("STORYHOOK_PLUGIN_JOBS", jobs)
            .env("FIXTURE_SHARED", self.shared())
            .env_remove("STORYHOOK_GATE_PROGRESS")
            .stdin(Stdio::null());
        command
    }

    fn run(&self, jobs: &str) -> Output {
        self.command(jobs)
            .output()
            .expect("running the plugin runner")
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The `name → PASS|FAIL` report lines, in the order the runner printed them.
fn verdicts(out: &Output) -> Vec<(String, String)> {
    stdout(out)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?;
            let verdict = fields.next()?;
            (name.starts_with("test-") && fields.next().is_none())
                .then(|| (name.to_string(), verdict.to_string()))
        })
        .collect()
}

/// A script that announces itself, then waits for `partner` to announce.
fn waits_for(partner: &str) -> String {
    format!(
        r#"me="$(basename "$0" .sh)"
touch "$FIXTURE_SHARED/$me.started"
deadline=$((SECONDS + {OVERLAP_PATIENCE_SECS}))
while [ ! -e "$FIXTURE_SHARED/test-{partner}.started" ]; do
  [ "$SECONDS" -lt "$deadline" ] || {{ echo "never overlapped with {partner}"; exit 1; }}
  sleep 0.1
done"#
    )
}

/// A script that records how many scripts were running when it started.
const COUNTS_ITS_COMPANY: &str = r#"me="$(basename "$0" .sh)"
mkdir "$FIXTURE_SHARED/running.$me"
ls -d "$FIXTURE_SHARED"/running.* | wc -l | tr -d ' ' >>"$FIXTURE_SHARED/company"
sleep 1
rmdir "$FIXTURE_SHARED/running.$me"
touch "$FIXTURE_SHARED/$me.done""#;

#[test]
fn every_script_is_reported_once_in_one_fixed_order_and_any_failure_fails_the_run() {
    for jobs in ["1", "3"] {
        let suite = Suite::new();
        // `a` finishes last when the pool runs all three at once, so a
        // completion-order report would put it last.
        suite.script("a", "sleep 1");
        suite.script("b", "echo 'b-diagnostic'; exit 1");
        suite.script("c", "true");

        let out = suite.run(jobs);
        let text = stdout(&out);

        assert_eq!(out.status.code(), Some(1), "jobs={jobs}: {out:?}");
        assert_eq!(
            verdicts(&out),
            [
                ("test-a.sh".into(), "PASS".into()),
                ("test-b.sh".into(), "FAIL".into()),
                ("test-c.sh".into(), "PASS".into()),
            ],
            "jobs={jobs}:\n{text}"
        );
        assert!(
            text.contains("      b-diagnostic"),
            "a failing script's own output must follow its FAIL line, indented\n{text}"
        );
        assert!(text.contains("passed: 2  failed: 1"), "{text}");
        assert!(text.contains("failed tests:\n  - test-b.sh"), "{text}");
    }
}

#[test]
fn a_green_run_exits_zero() {
    let suite = Suite::new();
    suite.script("a", "true");
    suite.script("b", "true");

    let out = suite.run("2");

    assert!(out.status.success(), "{out:?}");
    assert!(stdout(&out).contains("passed: 2  failed: 0"), "{out:?}");
}

#[test]
fn scripts_run_concurrently_up_to_the_job_limit_and_never_past_it() {
    let suite = Suite::new();
    // `a` and `b` can only pass if they run at the same time.
    suite.script("a", &waits_for("b"));
    suite.script("b", &waits_for("a"));
    for name in ["c", "d", "e"] {
        suite.script(name, COUNTS_ITS_COMPANY);
    }

    let out = suite.run("2");

    assert!(out.status.success(), "{}", stdout(&out));
    let company = fs::read_to_string(suite.shared().join("company")).expect("company log");
    let most = company
        .lines()
        .map(|n| n.parse::<u32>().expect("a count"))
        .max()
        .expect("three counts");
    assert!(most <= 2, "a job limit of 2 ran {most} scripts at once");
}

#[test]
fn a_serial_script_runs_alone_after_every_pooled_script_has_finished() {
    let suite = Suite::new();
    // Sorts first, so discovery order alone would run it first.
    suite.script(
        "a-serial",
        r#"# plugin-runner: serial
for other in b c d; do
  [ -e "$FIXTURE_SHARED/test-$other.done" ] || { echo "ran before test-$other finished"; exit 1; }
done
if ls -d "$FIXTURE_SHARED"/running.* >/dev/null 2>&1; then echo "ran beside another script"; exit 1; fi"#,
    );
    for name in ["b", "c", "d"] {
        suite.script(name, COUNTS_ITS_COMPANY);
    }

    let out = suite.run("4");

    assert!(out.status.success(), "{}", stdout(&out));
    let names: Vec<String> = verdicts(&out).into_iter().map(|(name, _)| name).collect();
    assert_eq!(
        names,
        ["test-b.sh", "test-c.sh", "test-d.sh", "test-a-serial.sh"],
        "the serial lane reports after the pool, each lane in discovery order"
    );
}

#[test]
fn the_gate_journal_gets_the_total_and_one_case_per_script_and_no_child_sees_it() {
    let suite = Suite::new();
    let journal = suite.root.path().join("journal.jsonl");
    fs::write(&journal, "").expect("fixture: the journal");
    let leak = r#"if [ -n "${STORYHOOK_GATE_PROGRESS:-}" ]; then echo "inherited the journal"; exit 1; fi"#;
    suite.script("a", leak);
    suite.script("b", &format!("{leak}\nexit 1"));

    let out = suite
        .command("2")
        .env("STORYHOOK_GATE_PROGRESS", &journal)
        .output()
        .expect("running the plugin runner");

    assert_eq!(out.status.code(), Some(1), "{}", stdout(&out));
    assert_eq!(
        verdicts(&out),
        [
            ("test-a.sh".into(), "PASS".into()),
            ("test-b.sh".into(), "FAIL".into()),
        ]
    );
    let journal = fs::read_to_string(&journal).expect("the journal");
    let lines: Vec<&str> = journal.lines().collect();
    assert!(
        lines[0].contains(r#""path":"release gate/plugin","status":"running""#)
            && lines[0].contains(r#""total":2"#),
        "{journal}"
    );
    let cases = |outcome: &str| {
        lines
            .iter()
            .filter(|line| {
                line.contains(&format!(
                    r#"{{"kind":"case","path":"release gate/plugin","outcome":"{outcome}"}}"#
                ))
            })
            .count()
    };
    assert_eq!((cases("pass"), cases("fail")), (1, 1), "{journal}");
    assert!(
        lines
            .last()
            .expect("a final item")
            .contains(r#""status":"failed""#),
        "{journal}"
    );
}

#[test]
fn a_job_limit_that_is_not_a_positive_integer_is_refused() {
    for jobs in ["0", "two", "-1"] {
        let suite = Suite::new();
        suite.script("a", r#"touch "$FIXTURE_SHARED/ran""#);

        let out = suite.run(jobs);

        assert_eq!(out.status.code(), Some(2), "jobs={jobs}: {out:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("STORYHOOK_PLUGIN_JOBS"),
            "jobs={jobs}: the refusal must name the variable: {out:?}"
        );
        assert!(
            !suite.shared().join("ran").exists(),
            "jobs={jobs}: nothing may run"
        );
    }
}

/// Whether `pid` is still running rather than gone or a zombie.
fn pid_running(pid: &str) -> bool {
    let out = Command::new("ps")
        .args(["-o", "state=", "-p", pid])
        .output()
        .expect("running ps");
    let state = String::from_utf8_lossy(&out.stdout);
    let state = state.trim();
    !state.is_empty() && !state.starts_with('Z')
}

#[test]
fn a_terminated_run_takes_its_running_scripts_and_their_children_with_it() {
    let suite = Suite::new();
    for name in ["a", "b"] {
        suite.script(
            name,
            r#"me="$(basename "$0" .sh)"
sleep 300 &
echo "$$ $!" >"$FIXTURE_SHARED/$me.pids"
wait"#,
        );
    }
    let mut runner = ChildGuard::spawn(
        suite
            .command("2")
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .expect("spawning the plugin runner");

    let give_up = Instant::now() + Duration::from_secs(OVERLAP_PATIENCE_SECS);
    let pid_files = [
        suite.shared().join("test-a.pids"),
        suite.shared().join("test-b.pids"),
    ];
    while !pid_files.iter().all(|file| file.exists()) {
        assert!(Instant::now() < give_up, "both scripts never started");
        std::thread::sleep(Duration::from_millis(100));
    }
    let pids: Vec<String> = pid_files
        .iter()
        .flat_map(|file| {
            fs::read_to_string(file)
                .expect("a pid file")
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect();

    let status = Command::new("kill")
        .args(["-TERM", &runner.pid().to_string()])
        .status()
        .expect("signalling the runner");
    assert!(status.success());
    let exit = runner.wait_within(Duration::from_secs(OVERLAP_PATIENCE_SECS), || {
        "the runner did not exit after SIGTERM".into()
    });

    assert!(
        exit.signal() == Some(15) || exit.code() == Some(143),
        "a terminated runner must report the signal, not a verdict: {exit:?}"
    );
    let survivors: Vec<&String> = pids.iter().filter(|pid| pid_running(pid)).collect();
    assert!(
        survivors.is_empty(),
        "scripts and their children outlived the runner: {survivors:?}"
    );
}

/// A script never inherits the caller's tmux server. The gate often runs
/// inside a tmux pane, and a script that runs `tmux new-session` without
/// `-S` (`test-dispatch-failure-cleanup.sh`) would otherwise create its
/// sessions on that real server instead of the per-test `TMUX_TMPDIR` lib.sh
/// gives it.
#[test]
fn children_never_inherit_the_callers_tmux_server() {
    let suite = Suite::new();
    suite.script(
        "a",
        r#"if [ -n "${TMUX:-}${TMUX_PANE:-}" ]; then echo "inherited TMUX=${TMUX:-} TMUX_PANE=${TMUX_PANE:-}"; exit 1; fi"#,
    );

    let out = suite
        .command("1")
        .env("TMUX", "/private/tmp/tmux-501/default,1234,0")
        .env("TMUX_PANE", "%9")
        .output()
        .expect("running the plugin runner");

    assert!(out.status.success(), "{}", stdout(&out));
}
