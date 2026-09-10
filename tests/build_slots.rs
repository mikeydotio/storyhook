//! `scripts/rustc-slot.py` — the machine-wide bound on concurrent rustc
//! processes (SH-655) — driven for real, by symlink, with a fake rustc.
//!
//! The wrapper is reached through `.cargo/config.toml`'s `build.rustc-wrapper`
//! and holds one of K `flock` slots on an inherited fd while it EXECS rustc
//! in place. What this file proves, each by running the tracked script and
//! never a copy of its logic:
//!
//! - never more than K compiles overlap, and the bound actually bit;
//! - a probe with no `--crate-name` takes no slot and is never queued;
//! - rustc's own exit status and pid are the wrapper's, because it execs;
//! - a SIGKILLed holder frees its slot with no reclaim code path — the kernel
//!   is the liveness oracle, the reason `flock` was chosen over the mkdir lock
//!   `machine-lock.sh` uses for a holder that is a shell;
//! - an unusable slot root fails OPEN, loudly, and the compile still runs;
//! - K derives from the machine's performance-core count, measured here from
//!   the same source rather than written down;
//! - a wait is reported on stderr and, under a gate-held run, to the SH-524
//!   progress journal — and nowhere when that journal is unset;
//! - the wiring: the config names exactly this script, the script is
//!   executable in git, no tracked file overrides the wrapper through the
//!   environment, and the two cadences it shares with `machine-lock.sh` are
//!   equal (SH-136 — one number, or a test that says when it stops being).
//!
//! Every deadline derives from the fake rustc's own sleep (SH-394): the bound
//! a wait disproves is "this compile is still running", and how long that is
//! is the fixture's to say.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use storyhook_test_support::{ChildGuard, run_bounded, scratch_dir};
use tempfile::TempDir;

fn checkout() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The tracked wrapper, reached through a symlink in a disposable root, and
/// a fake rustc that logs `start`/`end` lines with a nanosecond clock and its
/// own pid, sleeps `FAKE_RUSTC_SLEEP` seconds, and exits `FAKE_RUSTC_EXIT`.
struct Fixture {
    root: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = scratch_dir();
        fs::create_dir_all(root.path().join("scripts")).unwrap();
        std::os::unix::fs::symlink(
            checkout().join("scripts/rustc-slot.py"),
            root.path().join("scripts/rustc-slot.py"),
        )
        .unwrap();
        let fake = root.path().join("rustc");
        fs::write(
            &fake,
            format!(
                "#!/usr/bin/env bash\nset -u\n\
                 if [ \"${{1:-}}\" = -vV ]; then echo 'rustc 0.0.0 (fake)'; exit 0; fi\n\
                 crate=''\n\
                 while [ \"$#\" -gt 0 ]; do [ \"$1\" = --crate-name ] && crate=\"$2\"; shift; done\n\
                 now() {{ python3 -c 'import time; print(time.monotonic_ns())'; }}\n\
                 printf 'start %s %s %s\\n' \"$(now)\" \"$crate\" \"$$\" >>\"{log}\"\n\
                 sleep \"${{FAKE_RUSTC_SLEEP:-0}}\"\n\
                 printf 'end %s %s %s\\n' \"$(now)\" \"$crate\" \"$$\" >>\"{log}\"\n\
                 exit \"${{FAKE_RUSTC_EXIT:-0}}\"\n",
                log = root.path().join("log").display()
            ),
        )
        .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        Self { root }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn lock_root(&self) -> PathBuf {
        self.path().join("locks")
    }

    /// `python3 scripts/rustc-slot.py <fake rustc> <args…>` with K slots.
    fn compile(&self, slots: usize, crate_name: &str, sleep_secs: f64) -> Command {
        let mut cmd = self.wrapper(slots);
        cmd.arg(self.path().join("rustc"))
            .args(["--crate-name", crate_name])
            .env("FAKE_RUSTC_SLEEP", format!("{sleep_secs}"));
        cmd
    }

    fn wrapper(&self, slots: usize) -> Command {
        let mut cmd = Command::new("python3");
        cmd.arg(self.path().join("scripts/rustc-slot.py"))
            .env("STORYHOOK_LOCK_DIR", self.lock_root())
            .env("STORYHOOK_BUILD_SLOTS", slots.to_string())
            .env_remove("STORYHOOK_GATE_PROGRESS")
            .env_remove("STORYHOOK_GATE_PROGRESS_PATH");
        cmd
    }

    fn log(&self) -> Vec<(String, u128, String, u32)> {
        fs::read_to_string(self.path().join("log"))
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let mut f = line.split(' ');
                (
                    f.next().unwrap().to_string(),
                    f.next().unwrap().parse().unwrap(),
                    f.next().unwrap().to_string(),
                    f.next().unwrap().parse().unwrap(),
                )
            })
            .collect()
    }

    /// The largest number of fake compiles alive at any instant.
    fn peak_concurrency(&self) -> usize {
        let mut events: Vec<(u128, i32)> = self
            .log()
            .iter()
            .map(|(kind, at, _, _)| (*at, if kind == "start" { 1 } else { -1 }))
            .collect();
        // An end and a start at one nanosecond cannot both be true of one
        // slot; sort the end first so a handover never reads as an overlap.
        events.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        let (mut live, mut peak) = (0i32, 0i32);
        for (_, delta) in events {
            live += delta;
            peak = peak.max(live);
        }
        peak as usize
    }

    fn wait_for_starts(&self, n: usize, within: Duration) {
        let give_up = Instant::now() + within;
        while Instant::now() < give_up {
            if self.log().iter().filter(|(k, ..)| k == "start").count() >= n {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "{n} compiles did not start within {within:?}: {:?}",
            self.log()
        );
    }
}

const SLEEP: f64 = 0.4;

fn secs(s: f64) -> Duration {
    Duration::from_secs_f64(s)
}

#[test]
fn never_more_than_k_compiles_overlap_and_the_bound_bit() {
    let fx = Fixture::new();
    let k = 2;
    let n = 6;
    let started = Instant::now();
    let mut children: Vec<ChildGuard> = (0..n)
        .map(|i| {
            ChildGuard::spawn_with_output(&mut fx.compile(k, &format!("c{i}"), SLEEP)).unwrap()
        })
        .collect();
    // Six compiles of SLEEP each through two slots take at least three
    // rounds; the ceiling is that serial worst case with room for six
    // python starts, never a number about this machine.
    let deadline = secs(SLEEP * n as f64 * 2.0);
    for child in &mut children {
        let out = child.wait_with_output_within(deadline, || format!("log: {:?}", fx.log()));
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let elapsed = started.elapsed();
    assert_eq!(fx.log().iter().filter(|(k, ..)| k == "end").count(), n);
    assert!(
        fx.peak_concurrency() <= k,
        "peak {} > {k}: {:?}",
        fx.peak_concurrency(),
        fx.log()
    );
    // The positive control: with the bound in force, six compiles of SLEEP
    // through two slots cannot finish inside three sleeps' worth of wall
    // clock (ceil(6/2) rounds). Delete the lock and this fails.
    let serial_floor = secs(SLEEP * (n as f64 / k as f64).ceil());
    assert!(
        elapsed >= serial_floor,
        "all {n} compiles finished in {elapsed:?}, faster than {k} slots allow ({serial_floor:?}): the bound did not bite"
    );
}

#[test]
fn a_probe_without_a_crate_name_takes_no_slot_and_is_never_queued() {
    let fx = Fixture::new();
    let hold = SLEEP * 10.0;
    let mut holder = ChildGuard::spawn_with_output(&mut fx.compile(1, "holder", hold)).unwrap();
    fx.wait_for_starts(1, secs(hold));

    let mut probe = fx.wrapper(1);
    probe.arg(fx.path().join("rustc")).arg("-vV");
    let started = Instant::now();
    let out = run_bounded(probe, "rustc -vV through the wrapper", secs(hold));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("rustc 0.0.0 (fake)"));
    assert!(
        started.elapsed() < secs(hold / 2.0),
        "the probe waited behind the held slot for {:?}",
        started.elapsed()
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).is_empty(),
        "a probe has nothing to say: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    holder.kill_and_reap();
}

#[test]
fn rustcs_exit_status_and_pid_are_the_wrappers_own_because_it_execs() {
    let fx = Fixture::new();
    let mut cmd = fx.compile(2, "failing", 0.0);
    cmd.env("FAKE_RUSTC_EXIT", "3");
    let mut child = ChildGuard::spawn_with_output(&mut cmd).unwrap();
    let pid = child.pid();
    let out = child.wait_with_output_within(secs(SLEEP * 10.0), || format!("{:?}", fx.log()));
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let logged_pid = fx.log()[0].3;
    assert_eq!(
        logged_pid, pid,
        "the fake rustc ran as pid {logged_pid} under wrapper pid {pid}: the wrapper forked instead of exec'ing, so a lock it held would outlive nothing"
    );
}

#[test]
fn a_sigkilled_holder_frees_its_slot_with_no_reclaim_path() {
    let fx = Fixture::new();
    let hold = SLEEP * 50.0;
    let mut holder = ChildGuard::spawn_with_output(&mut fx.compile(1, "holder", hold)).unwrap();
    fx.wait_for_starts(1, secs(hold));
    let mut waiter = ChildGuard::spawn_with_output(&mut fx.compile(1, "waiter", 0.0)).unwrap();
    // The waiter must genuinely be blocked before the holder dies, or the
    // test proves only that an empty slot can be taken.
    std::thread::sleep(secs(SLEEP));
    assert_eq!(
        fx.log().iter().filter(|(k, ..)| k == "start").count(),
        1,
        "the waiter started while the slot was held: {:?}",
        fx.log()
    );

    // SAFETY: the pid is a child this test spawned and still owns.
    assert_eq!(unsafe { libc::kill(holder.pid() as i32, libc::SIGKILL) }, 0);
    // The ceiling is the holder's remaining sleep: had the slot NOT been
    // released by the kernel on death, the waiter would sit until then.
    let out = waiter.wait_with_output_within(secs(hold), || format!("{:?}", fx.log()));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("waited"),
        "the wait that ended must say so: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    holder.kill_and_reap();
}

#[test]
fn an_unusable_slot_root_fails_open_loudly_and_the_compile_still_runs() {
    let fx = Fixture::new();
    // A regular file where the lock root would have to be a directory.
    let blocker = fx.path().join("not-a-directory");
    fs::write(&blocker, "").unwrap();
    let mut cmd = fx.compile(2, "unbounded", 0.0);
    cmd.env("STORYHOOK_LOCK_DIR", &blocker);
    let out = run_bounded(cmd, "compile under an unusable root", secs(SLEEP * 10.0));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("UNBOUNDED"), "{stderr}");
    assert!(
        stderr.contains(&blocker.display().to_string()),
        "the line names the root: {stderr}"
    );
    assert_eq!(fx.log().iter().filter(|(k, ..)| k == "end").count(), 1);
}

#[test]
fn k_derives_from_the_machines_own_performance_core_count() {
    let fx = Fixture::new();
    let mut cmd = fx.wrapper(1);
    cmd.env_remove("STORYHOOK_BUILD_SLOTS").arg("--plan");
    let out = run_bounded(cmd, "rustc-slot.py --plan", secs(SLEEP * 10.0));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

    let (expected, source): (usize, &str) = if cfg!(target_os = "macos") {
        let sysctl = run_bounded(
            {
                let mut c = Command::new("sysctl");
                c.args(["-n", "hw.perflevel0.logicalcpu"]);
                c
            },
            "sysctl",
            secs(SLEEP * 10.0),
        );
        (
            String::from_utf8_lossy(&sysctl.stdout)
                .trim()
                .parse()
                .unwrap(),
            "hw.perflevel0.logicalcpu",
        )
    } else {
        let nproc = run_bounded(Command::new("nproc"), "nproc", secs(SLEEP * 10.0));
        (
            String::from_utf8_lossy(&nproc.stdout)
                .trim()
                .parse()
                .unwrap(),
            "sched_getaffinity",
        )
    };
    assert_eq!(plan["slots"], expected, "{plan}");
    assert_eq!(plan["source"], source, "{plan}");
    assert_eq!(
        plan["root"],
        fx.lock_root().join("build-slots").display().to_string()
    );
}

#[test]
fn a_wait_is_reported_on_stderr_and_to_a_set_journal_and_nowhere_when_unset() {
    let fx = Fixture::new();
    let hold = SLEEP * 4.0;
    let mut holder = ChildGuard::spawn_with_output(&mut fx.compile(1, "holder", hold)).unwrap();
    fx.wait_for_starts(1, secs(hold));

    let journal = fx.path().join("progress.ndjson");
    let mut cmd = fx.compile(1, "waiter", 0.0);
    cmd.env("STORYHOOK_GATE_PROGRESS", &journal)
        .env("STORYHOOK_GATE_PROGRESS_PATH", "release gate/rust-suite");
    let mut waiter = ChildGuard::spawn_with_output(&mut cmd).unwrap();
    let out = waiter.wait_with_output_within(secs(hold * 2.0), || format!("{:?}", fx.log()));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("build slots") && stderr.contains("waiting"),
        "{stderr}"
    );
    assert!(stderr.contains("waited"), "{stderr}");
    assert!(
        stderr.contains("holder"),
        "the report names the holder's crate: {stderr}"
    );

    let lines: Vec<serde_json::Value> = fs::read_to_string(&journal)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(
        lines.iter().any(|l| l["kind"] == "activity"
            && l["status"] == "running"
            && l["path"] == "release gate/rust-suite"
            && l["label"]
                .as_str()
                .unwrap()
                .contains("waiting for a build slot")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l["kind"] == "activity" && l["status"] == "passed"),
        "{lines:?}"
    );
    holder.kill_and_reap();

    // The no-op contract: with the variable unset nothing is written, so an
    // interactive build is byte-identical to one before this file existed.
    let fx2 = Fixture::new();
    let mut holder2 = ChildGuard::spawn_with_output(&mut fx2.compile(1, "holder", hold)).unwrap();
    fx2.wait_for_starts(1, secs(hold));
    let mut waiter2 = ChildGuard::spawn_with_output(&mut fx2.compile(1, "waiter", 0.0)).unwrap();
    let out = waiter2.wait_with_output_within(secs(hold * 2.0), || format!("{:?}", fx2.log()));
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("waited"));
    assert!(!fx2.path().join("progress.ndjson").exists());
    holder2.kill_and_reap();
}

// ---- wiring ---------------------------------------------------------------

fn tracked_files() -> Vec<String> {
    let out = Command::new("git")
        .args(["ls-files", "-z", "--stage"])
        .current_dir(checkout())
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

#[test]
fn the_cargo_config_names_this_script_and_git_tracks_it_executable() {
    let config: toml::Value =
        toml::from_str(&fs::read_to_string(checkout().join(".cargo/config.toml")).unwrap())
            .unwrap();
    assert_eq!(
        config["build"]["rustc-wrapper"].as_str(),
        Some("scripts/rustc-slot.py"),
        "{config}"
    );
    let entry = tracked_files()
        .into_iter()
        .find(|l| l.ends_with("\tscripts/rustc-slot.py"))
        .expect("scripts/rustc-slot.py is tracked");
    assert!(
        entry.starts_with("100755 "),
        "cargo can only run an executable wrapper; git records {entry}"
    );
}

/// No tracked file may set the environment overrides that beat the config
/// file, or the bound would be silently gone for whatever that file runs.
#[test]
fn no_tracked_file_overrides_the_wrapper_through_the_environment() {
    let this = "tests/build_slots.rs";
    let offenders: Vec<String> = tracked_files()
        .into_iter()
        .filter_map(|l| l.split('\t').nth(1).map(str::to_string))
        .filter(|p| p != this && !p.ends_with(".md"))
        .filter(|p| {
            fs::read_to_string(checkout().join(p)).is_ok_and(|s| {
                [
                    "RUSTC_WRAPPER=",
                    "CARGO_BUILD_RUSTC_WRAPPER=",
                    "CARGO_BUILD_JOBS=",
                ]
                .iter()
                .any(|needle| s.contains(needle))
            })
        })
        .collect();
    assert!(offenders.is_empty(), "{offenders:?}");
    // Positive control: the scanner sees an assignment when one exists.
    assert!("export RUSTC_WRAPPER=sccache".contains("RUSTC_WRAPPER="));
}

/// The two cadences the wrapper shares with `machine-lock.sh` are one number
/// each, on both sides, or this says so (SH-136).
#[test]
fn the_wrappers_cadences_equal_machine_locks() {
    let lock = fs::read_to_string(checkout().join("scripts/machine-lock.sh")).unwrap();
    let wrapper = fs::read_to_string(checkout().join("scripts/rustc-slot.py")).unwrap();
    fn shell_const(src: &str, name: &str) -> u64 {
        src.lines()
            .find_map(|l| l.trim().strip_prefix(&format!("readonly {name}=")))
            .unwrap_or_else(|| panic!("machine-lock.sh declares {name}"))
            .trim()
            .parse()
            .unwrap()
    }
    fn py_const(src: &str, name: &str) -> u64 {
        src.lines()
            .find_map(|l| l.strip_prefix(&format!("{name} = ")))
            .unwrap_or_else(|| panic!("rustc-slot.py declares {name}"))
            .trim()
            .parse()
            .unwrap()
    }
    assert_eq!(
        py_const(&wrapper, "RESCAN_SECS"),
        shell_const(&lock, "LOCK_POLL_SECS")
    );
    // WAIT_REPORT_SECS is spelled as a derivation in the shell
    // (`$GATE_MEDIAN_SECS`), so compare against the measured median it names.
    assert_eq!(
        py_const(&wrapper, "WAIT_REPORT_SECS"),
        shell_const(&lock, "GATE_MEDIAN_SECS")
    );
    assert!(lock.contains("readonly WAIT_REPORT_SECS=$GATE_MEDIAN_SECS"));
}
