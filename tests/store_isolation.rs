//! Store isolation: the store you name is the store you get (SH-113, SH-123).
//!
//! Daemon runtime state used to be keyed on the state home while the store was
//! keyed on the data home. The two move independently and nothing reconciled
//! them, so a client pointed at one store was served — for reads *and* writes —
//! by a daemon holding another, with no diagnostic and exit 0.
//!
//! Every fixture here is built by hand rather than with
//! [`storyhook_test_support::TestEnv`], and that is the point. `TestEnv` pairs a
//! private `STORYHOOK_DATA_DIR` with a private `XDG_STATE_HOME`; the pairing is
//! exactly what this file exists to stop being load-bearing, and a fixture that
//! isolates both cannot observe a client being served by another store's
//! daemon.
//!
//! Design of record: `docs/spec/store-isolation.md`.

use std::path::{Path, PathBuf};
use std::process::Command;

use storyhook_test_support::{daemon_containment, scratch_dir, story_binary};
use tempfile::TempDir;

/// A private world for one test: a `HOME`, one state home shared by every
/// command the test runs, and as many stores as it asks for.
struct Probe {
    root: TempDir,
}

impl Probe {
    fn new() -> Self {
        Self {
            root: scratch_dir(),
        }
    }

    /// A directory inside this probe, created on demand.
    fn dir(&self, name: &str) -> PathBuf {
        let path = self.root.path().join(name);
        std::fs::create_dir_all(&path).expect("creating a fixture directory");
        path
    }

    /// A path inside this probe, whose parent is created but which is not.
    fn file(&self, name: &str) -> PathBuf {
        let path = self.root.path().join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("creating a fixture directory");
        }
        path
    }

    fn home(&self) -> PathBuf {
        self.dir("home")
    }

    /// The `XDG_STATE_HOME` every command in this probe shares.
    ///
    /// Shared deliberately: two clients naming two stores while sharing one
    /// state home is the configuration the defect lived in.
    fn state_home(&self) -> PathBuf {
        self.dir("state")
    }

    /// Storyhook's own directory inside the shared state home.
    fn storyhook_state(&self) -> PathBuf {
        self.state_home().join("storyhook")
    }

    /// The per-store daemon directories that exist right now.
    fn daemon_dirs(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(self.storyhook_state().join("daemons")) else {
            return Vec::new();
        };
        let mut found: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        found.sort();
        found
    }

    /// One `story` invocation with nothing inherited but `PATH`.
    ///
    /// `env_clear` matters: the variables under test must not be able to arrive
    /// from the ambient shell or from `make test`'s wrapper, and what this file
    /// pins is a variable that was *believed* to isolate.
    fn story(&self, cwd: &Path) -> Command {
        let mut cmd = Command::new(story_binary());
        cmd.current_dir(cwd)
            .env_clear()
            .env("HOME", self.home())
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("XDG_STATE_HOME", self.state_home())
            // `env_clear` above threw away the containment `run-tests.sh`
            // exports for the whole run, so it has to be put back: never the
            // production port 3456, which is where a developer's own dashboard
            // lives, and never without a parent to die with. Asked for rather
            // than spelled out, because a copy of the pair is what SH-136 is
            // about — and `every_rust_harness_that_clears_the_environment_
            // reinstates_daemon_containment` below now requires the call.
            .envs(daemon_containment());
        cmd
    }

    /// Stands down whatever daemon is holding the store under `data_dir`.
    ///
    /// This replaces the in-process transport, which is what the byte questions
    /// below used to reach for. A daemon holds the database open with its own
    /// page cache and its own write-ahead-log handle, so a byte comparison taken
    /// while one is running compares a file nobody has finished writing.
    fn quiesce(&self, cwd: &Path, data_dir: &Path) {
        let _ = self
            .story(cwd)
            .env("STORYHOOK_DATA_DIR", data_dir)
            .args(["daemon", "stop"])
            .output();
    }
}

/// Runs `cmd`, asserting it succeeded, and returns its stdout.
fn ok(cmd: &mut Command) -> String {
    let out = cmd.output().expect("running the binary under test");
    assert!(
        out.status.success(),
        "expected success, got {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Runs `cmd`, asserting it failed, and returns its stderr.
fn refused(cmd: &mut Command) -> String {
    let out = cmd.output().expect("running the binary under test");
    assert!(
        !out.status.success(),
        "expected a refusal, got success\n--- stdout ---\n{}",
        String::from_utf8_lossy(&out.stdout),
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The bytes of a store file, for the "nothing touched it" assertions.
fn bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// SH-123: two stores, one state home
// ---------------------------------------------------------------------------

/// The reproduction from the spec, as a test.
///
/// Two data directories and one state home. Before store isolation the second
/// client became an RPC client of the first store's daemon: its own store file
/// was never created, and every project it believed it had made went into
/// somebody else's database.
#[test]
fn a_second_data_dir_is_not_served_by_the_first_ones_daemon() {
    let probe = Probe::new();
    let store_a = probe.dir("A");
    let store_b = probe.dir("B");
    let repo_a = probe.dir("repoA");
    let repo_b = probe.dir("repoB");

    ok(probe
        .story(&repo_a)
        .env("STORYHOOK_DATA_DIR", &store_a)
        .args(["project", "new", "--prefix", "AAA"]));
    ok(probe
        .story(&repo_a)
        .env("STORYHOOK_DATA_DIR", &store_a)
        .args(["new", "CANARY belongs to store A"]));

    ok(probe
        .story(&repo_b)
        .env("STORYHOOK_DATA_DIR", &store_b)
        .args(["project", "new", "--prefix", "BBB"]));

    assert!(
        store_b.join("store.db").exists(),
        "naming a second store must create it; if it does not, the client is \
         talking to the first store's daemon"
    );

    let in_b = ok(probe
        .story(&repo_b)
        .env("STORYHOOK_DATA_DIR", &store_b)
        .args(["project", "list"]));
    assert!(
        in_b.contains("repoB"),
        "store B must know its own project; it said:\n{in_b}"
    );
    assert!(
        !in_b.contains("repoA"),
        "store B must not see store A's project; it said:\n{in_b}"
    );

    let in_a = ok(probe
        .story(&repo_a)
        .env("STORYHOOK_DATA_DIR", &store_a)
        .args(["project", "list"]));
    assert!(
        in_a.contains("repoA"),
        "store A must still know its own project; it said:\n{in_a}"
    );
    assert!(
        !in_a.contains("repoB"),
        "store A must not have been written into by store B's client; it said:\n{in_a}"
    );

    let stories = ok(probe
        .story(&repo_a)
        .env("STORYHOOK_DATA_DIR", &store_a)
        .args(["list"]));
    assert!(
        stories.contains("CANARY"),
        "store A's story must have survived; it said:\n{stories}"
    );
}

/// Two stores in one session, selected by flag rather than by variable.
#[test]
fn a_write_under_one_store_path_is_invisible_under_another() {
    let probe = Probe::new();
    let one = probe.file("stores/one.db");
    let two = probe.file("stores/two.db");
    let repo = probe.dir("repo");

    let one_flag = one.to_str().unwrap().to_string();
    let two_flag = two.to_str().unwrap().to_string();

    ok(probe.story(&repo).args([
        "--store-path",
        &one_flag,
        "project",
        "new",
        "--prefix",
        "ONE",
    ]));
    ok(probe
        .story(&repo)
        .args(["--store-path", &one_flag, "new", "CANARY belongs to one"]));

    ok(probe.story(&repo).args([
        "--store-path",
        &two_flag,
        "project",
        "new",
        "--prefix",
        "TWO",
    ]));
    let listed = ok(probe.story(&repo).args(["--store-path", &two_flag, "list"]));
    assert!(
        !listed.contains("CANARY"),
        "a story written under one store must not be visible under another; \
         store two said:\n{listed}"
    );

    let listed = ok(probe.story(&repo).args(["--store-path", &one_flag, "list"]));
    assert!(
        listed.contains("CANARY"),
        "store one must still have its own story; it said:\n{listed}"
    );
}

/// The variable and the flag name the same thing, and the flag wins.
#[test]
fn the_store_path_variable_is_honoured_and_the_flag_outranks_it() {
    let probe = Probe::new();
    let by_var = probe.file("stores/by-var.db");
    let by_flag = probe.file("stores/by-flag.db");
    let repo = probe.dir("repo");

    ok(probe
        .story(&repo)
        .env("STORYHOOK_STORE_PATH", &by_var)
        .args(["project", "new", "--prefix", "VAR"]));
    assert!(
        by_var.exists(),
        "STORYHOOK_STORE_PATH must name the store file itself"
    );

    ok(probe
        .story(&repo)
        .env("STORYHOOK_STORE_PATH", &by_var)
        .args([
            "--store-path",
            by_flag.to_str().unwrap(),
            "project",
            "new",
            "--prefix",
            "FLAG",
        ]));
    assert!(
        by_flag.exists(),
        "--store-path must outrank STORYHOOK_STORE_PATH"
    );

    let listed = ok(probe
        .story(&repo)
        .env("STORYHOOK_STORE_PATH", &by_var)
        .args(["project", "list"]));
    assert!(
        !listed.contains("FLAG"),
        "the flag's project must not have landed in the variable's store; it said:\n{listed}"
    );
}

// ---------------------------------------------------------------------------
// One store is one daemon
// ---------------------------------------------------------------------------

/// Two spellings of one path are one store, and therefore one daemon.
///
/// The failure this prevents is two daemons on one SQLite file: each with its
/// own page cache, its own change token and its own write-ahead-log handle,
/// neither knowing about the other.
#[test]
fn two_spellings_of_one_store_share_one_daemon() {
    let probe = Probe::new();
    let real = probe.dir("stores");
    let link = probe.root.path().join("link");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &link).expect("creating a symlink to the store directory");
    let repo = probe.dir("repo");

    let plain = real.join("one.db");
    let dotted = real.join(".").join("..").join("stores").join("one.db");
    let linked = link.join("one.db");
    let trailing = format!("{}/", real.display());
    let trailing = Path::new(&trailing).join("one.db");

    ok(probe.story(&repo).args([
        "--store-path",
        plain.to_str().unwrap(),
        "project",
        "new",
        "--prefix",
        "ONE",
    ]));
    ok(probe
        .story(&repo)
        .args(["--store-path", plain.to_str().unwrap(), "new", "CANARY"]));

    for spelling in [&dotted, &linked, &trailing] {
        let listed =
            ok(probe
                .story(&repo)
                .args(["--store-path", spelling.to_str().unwrap(), "list"]));
        assert!(
            listed.contains("CANARY"),
            "`{}` is another spelling of the same store and must see its \
             stories; it said:\n{listed}",
            spelling.display()
        );
    }

    assert_eq!(
        probe.daemon_dirs().len(),
        1,
        "four spellings of one store must produce one daemon, not four: {:?}",
        probe.daemon_dirs()
    );
}

/// Clients racing to start a daemon for one store produce one daemon; clients
/// naming two stores produce two.
#[test]
fn concurrent_clients_produce_exactly_one_daemon_per_store() {
    let probe = Probe::new();
    let one = probe.file("stores/one.db");
    let two = probe.file("stores/two.db");
    let repo = probe.dir("repo");

    // The stores exist before the race, so that what is being raced is the
    // daemon spawn rather than the schema migration.
    ok(probe
        .story(&repo)
        .args(["store", "new", one.to_str().unwrap()]));
    ok(probe
        .story(&repo)
        .args(["store", "new", two.to_str().unwrap()]));

    let mut running = Vec::new();
    for _ in 0..4 {
        for store in [&one, &two] {
            running.push(
                probe
                    .story(&repo)
                    .args(["--store-path", store.to_str().unwrap(), "project", "list"])
                    // Piped, so that a loser's diagnostics reach the assertion
                    // below instead of the test runner's own output.
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .expect("spawning a racing client"),
            );
        }
    }
    for child in running {
        let out = child.wait_with_output().expect("waiting for a client");
        assert!(
            out.status.success(),
            "every racing client must succeed; one said:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    assert_eq!(
        probe.daemon_dirs().len(),
        2,
        "eight clients over two stores must leave two daemons: {:?}",
        probe.daemon_dirs()
    );
}

/// A run against a named store must leave the ambient one exactly as it was.
///
/// The ambient store's daemon is stood down before either byte reading, and only
/// then: a daemon holds the database open with its own page cache and its own
/// log, so a comparison taken while one is running compares a file nobody has
/// finished writing.
#[test]
fn a_store_path_run_leaves_the_ambient_store_byte_identical() {
    let probe = Probe::new();
    let ambient = probe.dir("ambient");
    let named = probe.file("stores/named.db");
    let repo = probe.dir("repo");
    let elsewhere = probe.dir("elsewhere");

    ok(probe
        .story(&repo)
        .env("STORYHOOK_DATA_DIR", &ambient)
        .args(["project", "new", "--prefix", "AMB"]));
    ok(probe
        .story(&repo)
        .env("STORYHOOK_DATA_DIR", &ambient)
        .args(["new", "Do not touch me"]));

    let store = ambient.join("store.db");
    let listed_before = ok(probe
        .story(&repo)
        .env("STORYHOOK_DATA_DIR", &ambient)
        .args(["list"]));
    probe.quiesce(&repo, &ambient);
    let before = bytes(&store);

    for args in [
        vec!["project", "new", "--prefix", "NAMED"],
        vec!["new", "A story for the named store"],
        vec!["list"],
        vec!["project", "list"],
    ] {
        let cwd = if args[0] == "project" && args.get(1) == Some(&"list") {
            elsewhere.clone()
        } else {
            repo.clone()
        };
        let mut cmd = probe.story(&cwd);
        cmd.env("STORYHOOK_DATA_DIR", &ambient)
            .args(["--store-path", named.to_str().unwrap()])
            .args(&args);
        ok(&mut cmd);
    }

    probe.quiesce(&repo, &ambient);
    assert_eq!(
        bytes(&store),
        before,
        "a --store-path run must leave the ambient store byte-identical"
    );
    let listed_after = ok(probe
        .story(&repo)
        .env("STORYHOOK_DATA_DIR", &ambient)
        .args(["list"]));
    assert_eq!(listed_before, listed_after);
}

// ---------------------------------------------------------------------------
// `story store new`
// ---------------------------------------------------------------------------

#[test]
fn store_new_creates_an_empty_store_that_commands_can_use() {
    let probe = Probe::new();
    let store = probe.file("stores/fresh.db");
    let repo = probe.dir("repo");

    ok(probe
        .story(&repo)
        .args(["store", "new", store.to_str().unwrap()]));
    assert!(store.exists(), "store new must create the file it names");

    let listed =
        ok(probe
            .story(&repo)
            .args(["--store-path", store.to_str().unwrap(), "project", "list"]));
    assert!(
        listed.contains("No projects yet"),
        "a fresh store has no projects in it; it said:\n{listed}"
    );

    ok(probe.story(&repo).args([
        "--store-path",
        store.to_str().unwrap(),
        "project",
        "new",
        "--prefix",
        "NEW",
    ]));
}

/// The real store is created by the daemon on first run, never by this verb.
#[test]
fn store_new_refuses_the_default_path() {
    let probe = Probe::new();
    let xdg_data = probe.dir("xdg-data");
    let default_store = xdg_data.join("storyhook").join("store.db");
    let repo = probe.dir("repo");

    let stderr = refused(probe.story(&repo).env("XDG_DATA_HOME", &xdg_data).args([
        "store",
        "new",
        default_store.to_str().unwrap(),
    ]));
    assert!(
        stderr.contains("default"),
        "the refusal must name why the default store is special; it said:\n{stderr}"
    );
    assert!(
        !default_store.exists(),
        "a refused `store new` must write nothing"
    );
}

#[test]
fn store_new_refuses_a_path_that_already_exists() {
    let probe = Probe::new();
    let store = probe.file("stores/taken.db");
    let repo = probe.dir("repo");

    ok(probe
        .story(&repo)
        .args(["store", "new", store.to_str().unwrap()]));
    let stderr = refused(
        probe
            .story(&repo)
            .args(["store", "new", store.to_str().unwrap()]),
    );
    assert!(
        stderr.contains("exists"),
        "the refusal must say the path is taken; it said:\n{stderr}"
    );
}

/// `store new` names the store it creates, so it must not resolve — let alone
/// create — any other one on the way.
///
/// Pinned with nothing set at all, which is the case a test build refuses to
/// resolve a store in. If this verb needed the ambient store it could not run
/// here, and creating a scratch store would require first creating the real
/// one.
#[test]
fn store_new_does_not_resolve_the_ambient_store() {
    let probe = Probe::new();
    let store = probe.file("stores/standalone.db");
    let repo = probe.dir("repo");

    ok(probe
        .story(&repo)
        .args(["store", "new", store.to_str().unwrap()]));
    assert!(store.exists());
    assert!(
        !probe
            .home()
            .join(".local/share/storyhook/store.db")
            .exists(),
        "store new must not have created the default store on its way"
    );
}

// ---------------------------------------------------------------------------
// `story store backup` (SH-135)
// ---------------------------------------------------------------------------

/// Pulls the path `dispatch_store_backup`'s success message names, rather
/// than re-deriving `maintenance_backups_dir()`'s own layout logic here — the
/// message is the one place a caller (human or script) learns where the file
/// went, so asserting through it also pins that the message is honest.
fn backup_path_from_message(message: &str) -> PathBuf {
    let after = message
        .strip_prefix("backup written to ")
        .unwrap_or_else(|| panic!("unexpected success message: {message}"));
    let path = after
        .split(" (label:")
        .next()
        .unwrap_or_else(|| panic!("unexpected success message: {message}"));
    PathBuf::from(path)
}

#[test]
fn store_backup_writes_a_verified_snapshot_into_the_maintenance_directory() {
    let probe = Probe::new();
    let repo = probe.dir("repo");
    let data_dir = probe.dir("data");

    let out = ok(probe
        .story(&repo)
        .env("STORYHOOK_DATA_DIR", &data_dir)
        .args(["store", "backup", "--label", "pre-migration"]));
    assert!(
        out.contains("pre-migration"),
        "the success message should name the label; it said:\n{out}"
    );

    let path = backup_path_from_message(out.trim());
    assert!(
        path.exists(),
        "the message named {path:?}, which must exist on disk"
    );
    assert!(
        path.to_string_lossy().contains("pre-migration"),
        "the label should be part of the filename: {path:?}"
    );
    assert!(
        path.components().any(|c| c.as_os_str() == "maintenance"),
        "a hand-taken backup must land under a `maintenance` directory, not the one the \
         daily prune scans: {path:?}"
    );
}

#[test]
fn store_backup_defaults_its_label_to_manual() {
    let probe = Probe::new();
    let repo = probe.dir("repo");
    let data_dir = probe.dir("data");

    let out = ok(probe
        .story(&repo)
        .env("STORYHOOK_DATA_DIR", &data_dir)
        .args(["store", "backup"]));

    let path = backup_path_from_message(out.trim());
    assert!(
        path.to_string_lossy().contains("manual"),
        "an unlabeled backup should default to `manual`: {path:?}"
    );
}

#[test]
fn store_backup_refuses_a_label_that_is_not_a_safe_filename_component() {
    let probe = Probe::new();
    let repo = probe.dir("repo");
    let data_dir = probe.dir("data");

    let stderr = refused(
        probe
            .story(&repo)
            .env("STORYHOOK_DATA_DIR", &data_dir)
            .args(["store", "backup", "--label", "../../etc/passwd"]),
    );
    assert!(
        stderr.contains("invalid backup label"),
        "the refusal should name the problem; it said:\n{stderr}"
    );
}

/// `story store backup` acts on the ambient store, not a project — it must
/// answer in a directory storyhook has never heard of, the same as `story web
/// status` (`invoker_seam.rs`'s `the_project_less_verbs_all_answer_outside_a_project`
/// pins this at the seam level; this pins it end to end through the real
/// binary).
#[test]
fn store_backup_runs_in_an_uninitialized_directory() {
    let probe = Probe::new();
    let repo = probe.dir("repo");
    let data_dir = probe.dir("data");

    ok(probe
        .story(&repo)
        .env("STORYHOOK_DATA_DIR", &data_dir)
        .args(["store", "backup"]));
}

/// The backup is reported by `daemon status`, per this same story's council
/// decision — never by `story doctor`, whose output is pinned by a golden
/// corpus and whose exit code means a project's integrity, not a machine's
/// backup freshness.
#[test]
fn store_backup_is_reported_by_daemon_status_not_by_doctor() {
    let probe = Probe::new();
    let repo = probe.dir("repo");
    let data_dir = probe.dir("data");

    ok(probe
        .story(&repo)
        .env("STORYHOOK_DATA_DIR", &data_dir)
        .args(["store", "backup", "--label", "pre-migration"]));

    let status = ok(probe
        .story(&repo)
        .env("STORYHOOK_DATA_DIR", &data_dir)
        .args(["daemon", "status"]));
    assert!(
        status.contains("maintenance backups"),
        "`daemon status` should report the maintenance directory; it said:\n{status}"
    );

    ok(probe
        .story(&repo)
        .env("STORYHOOK_DATA_DIR", &data_dir)
        .args(["project", "new", "--prefix", "SH"]));
    let doctor = ok(probe
        .story(&repo)
        .env("STORYHOOK_DATA_DIR", &data_dir)
        .args(["doctor"]));
    assert!(
        !doctor.to_lowercase().contains("maintenance backup"),
        "`story doctor`'s golden-corpus-pinned output must not mention backups; it said:\n{doctor}"
    );
}

// ---------------------------------------------------------------------------
// The upgrade
// ---------------------------------------------------------------------------

/// A daemon started before this change published its portfile at the state
/// home's root. After the upgrade a client looks under `daemons/<key>/`, finds
/// nothing, and must stand the old one down rather than run beside it.
#[test]
fn a_daemon_at_the_legacy_portfile_is_stood_down_rather_than_duplicated() {
    let probe = Probe::new();
    let xdg_data = probe.dir("xdg-data");
    let ambient = xdg_data.join("storyhook");
    std::fs::create_dir_all(&ambient).expect("creating the ambient store directory");
    let repo = probe.dir("repo");

    let with_ambient = |cwd: &Path| {
        let mut cmd = probe.story(cwd);
        cmd.env("XDG_DATA_HOME", &xdg_data)
            .env("STORYHOOK_DATA_DIR", &ambient);
        cmd
    };

    ok(with_ambient(&repo).args(["project", "new", "--prefix", "OLD"]));
    let dirs = probe.daemon_dirs();
    assert_eq!(dirs.len(), 1, "one store, one daemon: {dirs:?}");

    let keyed = dirs[0].join("daemon.json");
    let legacy = probe.storyhook_state().join("daemon.json");
    let before: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&keyed).expect("reading the portfile"))
            .expect("parsing the portfile");
    std::fs::rename(&keyed, &legacy).expect("moving the portfile to where the old build wrote it");

    ok(with_ambient(&repo).args(["list"]));

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&keyed).expect("reading the new portfile"))
            .expect("parsing the new portfile");
    assert_ne!(
        before["pid"], after["pid"],
        "the daemon holding the legacy portfile must have been replaced, not joined"
    );
    assert!(
        !legacy.exists(),
        "the legacy portfile must be cleared once its daemon is stood down"
    );
    assert_eq!(
        probe.daemon_dirs().len(),
        1,
        "the upgrade must leave one daemon on the store, not two"
    );
}

// ---------------------------------------------------------------------------
// The portfile describes its store
// ---------------------------------------------------------------------------

/// A portfile a human reads should say which store the daemon holds, and a
/// digest collision should be detectable rather than silent.
#[test]
fn a_portfile_names_the_store_its_daemon_holds() {
    let probe = Probe::new();
    let store = probe.file("stores/named.db");
    let repo = probe.dir("repo");

    ok(probe.story(&repo).args([
        "--store-path",
        store.to_str().unwrap(),
        "project",
        "new",
        "--prefix",
        "NMD",
    ]));

    let dirs = probe.daemon_dirs();
    assert_eq!(dirs.len(), 1, "one store, one daemon: {dirs:?}");
    let info: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dirs[0].join("daemon.json")).expect("reading the portfile"),
    )
    .expect("parsing the portfile");
    assert_eq!(
        info["store_path"].as_str().map(Path::new),
        Some(store.as_path()),
        "the portfile must name the store its daemon holds"
    );
}

/// Every command carries the flag, not just the ones that obviously read the
/// store.
#[test]
fn the_store_path_flag_reaches_the_daemon_family_too() {
    let probe = Probe::new();
    let store = probe.file("stores/named.db");
    let repo = probe.dir("repo");

    ok(probe.story(&repo).args([
        "--store-path",
        store.to_str().unwrap(),
        "project",
        "new",
        "--prefix",
        "NMD",
    ]));

    let status =
        ok(probe
            .story(&repo)
            .args(["--store-path", store.to_str().unwrap(), "daemon", "status"]));
    assert!(
        status.contains("running"),
        "`daemon status` under --store-path must describe that store's daemon; it said:\n{status}"
    );

    let elsewhere = probe.file("stores/elsewhere.db");
    let status = ok(probe.story(&repo).args([
        "--store-path",
        elsewhere.to_str().unwrap(),
        "daemon",
        "status",
    ]));
    assert!(
        status.contains("not running"),
        "a store with no daemon must not report one; it said:\n{status}"
    );
}

/// `--store-path` means *this invocation and everything it starts*.
///
/// The flag is published into `$STORYHOOK_STORE_PATH` rather than threaded
/// (`main`'s `publish_store_path`), because the consumers that matter cannot be
/// reached by threading: `story daemon status` and `story web status`
/// re-resolve, the TUI is dispatched ahead of the parser, and a child process is
/// a different program. A variable every one of them inherits is the only thing
/// that reaches all four.
///
/// This test observes the child, which is the consumer nothing else sees. An
/// event hook runs the binary again with no flag and no variable of its own, and
/// the story it writes has to land in the store its parent named. Deleting the
/// publication leaves the hook's `story` with nothing naming a store: in a test
/// build it refuses outright, and in a real one it writes into the developer's
/// own store — which is the silent half, and the reason this is pinned by
/// behaviour rather than by prose.
///
/// The command is the binary under test by absolute path. A bare `story`
/// resolves through `PATH`, which in a test run is whatever the developer has
/// installed.
#[test]
fn a_child_process_of_a_store_path_run_lands_in_the_same_store() {
    let probe = Probe::new();
    let store = probe.file("stores/named.db");
    let repo = probe.dir("repo");
    let flag = store.to_str().unwrap().to_string();

    ok(probe
        .story(&repo)
        .args(["--store-path", &flag, "project", "new", "--prefix", "CHD"]));

    let pointer = repo.join(".storyhook.toml");
    let existing = std::fs::read_to_string(&pointer).expect("the project has a pointer file");
    std::fs::write(
        &pointer,
        format!(
            "{existing}\n[hooks.on_create]\ncommand = \"{} new 'from the hook'\"\n",
            story_binary().display()
        ),
    )
    .expect("configuring the hook");

    ok(probe
        .story(&repo)
        .args(["--store-path", &flag, "new", "the parent"]));

    let listed = ok(probe.story(&repo).args(["--store-path", &flag, "list"]));
    assert!(
        listed.contains("from the hook"),
        "the hook's `story new` carried no flag and no store variable of its own, \
         so the only way it could have reached this store is the one `main` \
         published. The store said:\n{listed}"
    );
}

// ---------------------------------------------------------------------------
// The harnesses that isolate a test run
// ---------------------------------------------------------------------------

/// Whether `line` exports `name`.
fn exports(line: &str, name: &str) -> bool {
    line.trim_start()
        .strip_prefix("export ")
        .is_some_and(|rest| rest.trim_start().starts_with(&format!("{name}=")))
}

/// Whether `line` stops `$STORYHOOK_STORE_PATH` arriving from the caller.
///
/// Two spellings, because the two kinds of harness answer it differently:
/// a shell wrapper `unset`s the developer's, and `TestEnv` gives the child one
/// of its own so that what the child sees is asserted rather than assumed.
fn neutralizes_the_store_path(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("unset STORYHOOK_STORE_PATH") || exports(trimmed, "STORYHOOK_STORE_PATH")
}

/// Whether `line` pins `$STORYHOOK_DAEMON_ADDR` to the daemon's ephemeral-port
/// contract, rather than leaving it to default to the shared port 3456.
fn pins_the_daemon_to_an_ephemeral_port(line: &str) -> bool {
    exports(line, "STORYHOOK_DAEMON_ADDR") && line.contains("127.0.0.1:0")
}

/// The tracked shell scripts that export `$STORYHOOK_DATA_DIR`, paired with
/// their contents.
///
/// Shared by every test in this module that needs "every harness that isolates
/// the data directory" — the set this file's isolation invariants are pinned
/// against — so that set is derived once rather than re-scanned, and possibly
/// re-diverged, per test.
fn data_dir_harnesses(root: &Path) -> Vec<(String, String)> {
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

    let harnesses: Vec<(String, String)> = listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .filter_map(|path| {
            let relative = std::str::from_utf8(path).expect("a UTF-8 path").to_string();
            let text = std::fs::read_to_string(root.join(&relative))
                .unwrap_or_else(|e| panic!("reading {relative}: {e}"));
            text.lines()
                .any(|line| exports(line, "STORYHOOK_DATA_DIR"))
                .then_some((relative, text))
        })
        .collect();

    // A scan that matches nothing passes every assertion a caller builds on top
    // of it, which would make a broken pattern indistinguishable from a clean
    // tree.
    assert!(
        harnesses.len() >= 3,
        "this scan is supposed to find every shell harness that isolates the \
         data directory, and it found {}: {harnesses:?}. The pattern is broken, \
         not the harnesses.",
        harnesses.len()
    );
    harnesses
}

/// A harness that isolates `$STORYHOOK_DATA_DIR` and stops there has not
/// isolated anything.
///
/// `$STORYHOOK_STORE_PATH` outranks `$STORYHOOK_DATA_DIR`, so an exported one in
/// a developer's shell — which is exactly what somebody debugging a second store
/// has — sends the whole run into their own store. Nothing notices: the data-dir
/// guard inspects the variable that lost, and the run passes. Measured while
/// SH-131 was being written: one 9-test file left 9 projects and 7 stories in the
/// leaked store and never created the isolated one.
///
/// Derived rather than enumerated, because the enumeration is what failed. The
/// three `unset` lines went in together with store isolation itself and
/// `scripts/capture-baseline.sh` was missed — a fourth harness, exporting the
/// same variables, whose own comment claims it provides "the same contract
/// `scripts/run-tests.sh` provides". A list in a document could not see that; a
/// derived rule cannot miss it, and a fifth harness inherits the check by
/// existing.
#[test]
fn every_harness_that_isolates_the_data_dir_neutralizes_the_store_path() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let harnesses = data_dir_harnesses(root);

    let gaps: Vec<String> = harnesses
        .iter()
        .filter(|(_, text)| !text.lines().any(neutralizes_the_store_path))
        .map(|(relative, _)| relative.clone())
        .collect();
    assert!(
        gaps.is_empty(),
        "{gaps:?} export STORYHOOK_DATA_DIR without neutralizing \
         STORYHOOK_STORE_PATH, which outranks it. A developer with one exported \
         runs whatever these scripts start against their own store, and nothing \
         says so. Add `unset STORYHOOK_STORE_PATH` beside the data-directory \
         export."
    );
}

/// Every tracked shell script that isolates `$STORYHOOK_DATA_DIR` also contains
/// the daemon it spawns: an ephemeral `$STORYHOOK_DAEMON_ADDR` and a
/// `$STORYHOOK_PARENT_PID` to die with.
///
/// The same shape of defect `every_harness_that_isolates_the_data_dir_
/// neutralizes_the_store_path` closes, one variable pair over (SH-136, filed
/// while SH-131 was counting a different list and found this one stale too). A
/// harness that skips `STORYHOOK_DAEMON_ADDR=127.0.0.1:0` lets its daemon bind
/// the shared port 3456 — a developer's own dashboard, on any machine where it
/// happened to be free. One that skips `STORYHOOK_PARENT_PID` spawns a daemon
/// that does not know to die with the run that started it, which is not
/// hypothetical: a leaked daemon once cost 78 of 139 tests in a later run.
/// CLAUDE.md hand-enumerated these harnesses in prose instead of deriving them,
/// and the count silently drifted — four, then five, then six — every time a
/// harness was added. A derived set cannot drift the same way.
///
/// Green on arrival, deliberately: every harness this scan finds today already
/// pins both variables, so there is no regression to catch. What it buys is a
/// fence, not a fix — a script added or edited later that isolates the data
/// directory without also containing its daemon fails here instead of leaking
/// one onto a developer's machine.
#[test]
fn every_harness_that_isolates_the_data_dir_also_contains_its_daemon() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let harnesses = data_dir_harnesses(root);

    let unpinned: Vec<String> = harnesses
        .iter()
        .filter(|(_, text)| !text.lines().any(pins_the_daemon_to_an_ephemeral_port))
        .map(|(relative, _)| relative.clone())
        .collect();
    assert!(
        unpinned.is_empty(),
        "{unpinned:?} export STORYHOOK_DATA_DIR without pinning \
         STORYHOOK_DAEMON_ADDR to 127.0.0.1:0. A daemon this harness spawns binds \
         the shared port 3456 instead of an ephemeral one — the same port a \
         developer's own dashboard may be using. Add \
         `export STORYHOOK_DAEMON_ADDR=\"${{STORYHOOK_DAEMON_ADDR:-127.0.0.1:0}}\"` \
         beside the data-directory export."
    );

    let orphanable: Vec<String> = harnesses
        .iter()
        .filter(|(_, text)| {
            !text
                .lines()
                .any(|line| exports(line, "STORYHOOK_PARENT_PID"))
        })
        .map(|(relative, _)| relative.clone())
        .collect();
    assert!(
        orphanable.is_empty(),
        "{orphanable:?} export STORYHOOK_DATA_DIR without exporting \
         STORYHOOK_PARENT_PID. A daemon this harness spawns does not know to die \
         with the run that started it and can outlive it, poisoning every later \
         run on the machine. Add `export STORYHOOK_PARENT_PID=\"$$\"` beside the \
         data-directory export."
    );
}

// ---------------------------------------------------------------------------
// SH-258: "a real store" is resolved in one place, not re-inferred per test
// ---------------------------------------------------------------------------

/// The one file allowed to infer "not temporary" from the checkout's own
/// layout — everyone else asks it instead.
const REAL_STORE_OWNER: &str = "crates/storyhook-test-support/src/real_store.rs";

/// Whether `text` re-derives "not temporary" from the checkout the way the
/// four sites [`REAL_STORE_OWNER`] replaced did: `CARGO_TARGET_TMPDIR`
/// directly, or `CARGO_MANIFEST_DIR` joined with a literal `"target"`
/// segment. Both were silently wrong whenever the checkout itself was
/// temp-rooted — the failure `creating_a_project_at_a_throwaway_path_in_a_
/// real_store_is_refused` reported as `left: 201, right: 201`, naming neither
/// the store nor the cause.
fn re_infers_a_real_store_from_the_checkout(text: &str) -> bool {
    text.contains("env!(\"CARGO_TARGET_TMPDIR\")")
        || (text.contains("CARGO_MANIFEST_DIR") && text.contains(".join(\"target\")"))
}

/// The pattern above matches the two shapes SH-258 actually found, and does
/// not fire on the ordinary, legitimate use of `CARGO_MANIFEST_DIR` to read a
/// file out of the checkout's own `src/` — which roughly a dozen other test
/// files do for unrelated reasons. A scan that matched everything would pass
/// this test's sibling for the wrong reason.
#[test]
fn the_real_store_regression_pattern_matches_what_it_claims_to() {
    assert!(re_infers_a_real_store_from_the_checkout(
        "let dir = Path::new(env!(\"CARGO_TARGET_TMPDIR\")).join(label);"
    ));
    assert!(re_infers_a_real_store_from_the_checkout(
        "PathBuf::from(env!(\"CARGO_MANIFEST_DIR\")).join(\"target\").join(label)"
    ));
    assert!(!re_infers_a_real_store_from_the_checkout(
        "storyhook_test_support::non_temporary_dir(label)"
    ));
    assert!(!re_infers_a_real_store_from_the_checkout(
        "Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"src\")"
    ));
}

/// Every tracked Rust file except [`REAL_STORE_OWNER`] itself must ask
/// `storyhook_test_support::non_temporary_dir` for a fixture root the store
/// guards will classify as real, rather than re-deriving one from the
/// checkout's own layout.
///
/// Derived over `git ls-files`, the same style `data_dir_harnesses` scans
/// with. A hand-maintained list of "the sites that do this" is exactly what
/// let three of the reported test's four siblings go unnoticed: the story
/// that found the first one only reproduced it because `cargo test` fail-
/// fasts at the first failing target, and the lib target that carried it
/// happened to run first. A copy re-added here — in a fifth site, or a
/// reintroduced sixth — fails this test instead of waiting for the next
/// release cut from a scratch clone to rediscover it.
#[test]
fn nothing_outside_real_store_rs_re_infers_a_real_store_from_the_checkout() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(
        root.join(REAL_STORE_OWNER).is_file(),
        "REAL_STORE_OWNER is stale — {REAL_STORE_OWNER} no longer exists, so this \
         scan's one exclusion excludes nothing"
    );

    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z", "--", "*.rs"])
        .output()
        .expect("listing this repository's tracked Rust files");
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so this scan proved nothing: {}",
        String::from_utf8_lossy(&listed.stderr)
    );

    let offenders: Vec<String> = listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .filter_map(|path| {
            let relative = std::str::from_utf8(path).expect("a UTF-8 path").to_string();
            if relative == REAL_STORE_OWNER {
                return None;
            }
            let text = std::fs::read_to_string(root.join(&relative))
                .unwrap_or_else(|e| panic!("reading {relative}: {e}"));
            re_infers_a_real_store_from_the_checkout(&text).then_some(relative)
        })
        .collect();

    // A scan over a tracked-file list this small (>1000 files) that matches
    // nothing at all could equally mean the pattern is broken; the sibling
    // test above is what makes that distinguishable from a genuinely clean
    // tree.
    assert!(
        offenders.is_empty(),
        "{offenders:?} re-derive \"not temporary\" from CARGO_MANIFEST_DIR or \
         CARGO_TARGET_TMPDIR directly — the premise SH-258 fixed, and silently \
         wrong again from a temp-rooted checkout. Call \
         storyhook_test_support::non_temporary_dir instead."
    );
}

// ---------------------------------------------------------------------------
// SH-249: the command that starts a daemon does not get to pick its port
// ---------------------------------------------------------------------------

/// Every spelling that starts a daemon binds the address the *environment*
/// resolved, and none substitutes a preference of its own.
///
/// **A fence over the spellings, not an example of one.** `story web start`
/// carried its own `DEFAULT_WEB_PORT` and forced `127.0.0.1:3456` whenever
/// `--port` was omitted, so it walked straight past `$STORYHOOK_DAEMON_ADDR` —
/// the single variable `daemon_containment` sets, the one
/// `every_harness_that_isolates_the_data_dir_also_contains_its_daemon` spends its
/// whole length making every harness export, and therefore the only thing keeping
/// a test daemon off the port a developer's own dashboard is using.
/// `tests/daemon_lifecycle.rs`'s alias test already ran that exact spelling inside
/// an isolated environment. Pinning `web start` alone would leave the next
/// spelling free to reintroduce the same default, so all four are derived from one
/// list and a fifth inherits the check by being added to it.
///
/// **Asserting the port is honoured, not that 3456 is avoided.** "Did not bind
/// 3456" passes for free on any machine where something already holds 3456 —
/// including the developer's real dashboard, which is precisely the machine this
/// is about — because [`bind_preferred`](storyhook::daemon::lifecycle::bind_preferred)
/// falls back to a kernel-assigned port when its preference is taken. That
/// fallback is what made the defect invisible: the wrong preference quietly
/// became a right-looking port. So each spelling is given a preference it could
/// only satisfy by asking the environment, and must come back holding exactly it.
///
/// The `--serve` spellings are checked separately from the `start` ones because
/// they arrive by a different route — `main::foreground_serve_port` rather than
/// `daemon::commands::start` — and that is the route a later refactor is most
/// likely to break silently, since no user-facing test runs it.
#[test]
fn every_spelling_that_starts_a_daemon_honours_the_preferred_port() {
    /// `story <spelling>` — how a dashboard gets started, in every spelling the
    /// CLI accepts. `foreground` marks the two that run the daemon in the calling
    /// process and never return, so the check has to spawn and poll rather than
    /// wait for an exit.
    const SPELLINGS: &[(&[&str], bool)] = &[
        (&["daemon", "start"], false),
        (&["web", "start"], false),
        (&["daemon", "--serve"], true),
        (&["web", "--serve"], true),
    ];

    let mut wrong = Vec::new();
    for (spelling, foreground) in SPELLINGS {
        let probe = Probe::new();
        let repo = probe.dir("repo");
        let data = probe.dir("data");
        // The port must be known before the process that binds it exists, which
        // is the one case `reserve_port` is for: the question here is whether the
        // *preference* was honoured, so there has to be a specific number to
        // honour.
        let preferred = storyhook_test_support::reserve_port();

        let mut command = probe.story(&repo);
        command
            .env("STORYHOOK_DATA_DIR", &data)
            .env("STORYHOOK_DAEMON_ADDR", format!("127.0.0.1:{preferred}"))
            .args(*spelling);

        let bound = if *foreground {
            let child = command
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawning a foreground daemon");
            let _guard = storyhook_test_support::ChildGuard::new(child);
            await_published_port(&probe)
        } else {
            ok(&mut command);
            let bound = published_port(&probe).expect("a client that returned must have a daemon");
            probe.quiesce(&repo, &data);
            Some(bound)
        };

        match bound {
            Some(bound) if bound == preferred => {}
            other => wrong.push(format!(
                "`story {}` asked for {preferred} and bound {}",
                spelling.join(" "),
                match other {
                    Some(bound) => bound.to_string(),
                    None => "nothing — it never published a portfile".to_string(),
                }
            )),
        }
    }

    assert!(
        wrong.is_empty(),
        "{wrong:?}\n\nEvery spelling that starts a daemon must defer to the address the \
         environment resolved — `$STORYHOOK_DAEMON_ADDR`, else `default_daemon_addr` for \
         this store — and none may substitute a port of its own. A spelling that forces \
         one overrides the only containment the test suite has against binding 3456, the \
         port a developer's own dashboard uses, and overrides store isolation's \
         port-0-for-a-non-default-store decision with it (SH-249)."
    );
}

/// The port in this probe's portfile, or `None` while there is not exactly one.
fn published_port(probe: &Probe) -> Option<u16> {
    let dirs = probe.daemon_dirs();
    let [dir] = dirs.as_slice() else {
        return None;
    };
    let info: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("daemon.json")).ok()?).ok()?;
    info["port"].as_u64().map(|port| port as u16)
}

/// [`published_port`], waited for — the foreground spellings publish on their own
/// schedule rather than before an exit this test could wait on.
///
/// The bound covers a tailnet probe (3s, and it runs before the portfile is
/// written) plus process start, with margin: what is being told apart is
/// "published somewhere else" from "never published at all", and both answers are
/// reported by the caller.
fn await_published_port(probe: &Probe) -> Option<u16> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Some(port) = published_port(probe) {
            return Some(port);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

// ---------------------------------------------------------------------------
// SH-253: the variable's IP means something, or it is refused
// ---------------------------------------------------------------------------

/// `$STORYHOOK_DAEMON_ADDR` names an address the daemon will actually bind, or
/// the command stops and says so.
///
/// **The unit tests in `env::tests` cover which IPs are refused; this covers
/// that the refusal reaches a person.** It is a CLI test rather than a second
/// parser test because the refusal's cost lives out here: `Environment::from_process`
/// runs at the top of *every* `story` invocation, so a value that was inert
/// yesterday now stops `story project list` as surely as it stops `story daemon
/// start`. A message that only said "no" would read as a total outage, which is
/// why the assertions below are about what the text offers rather than merely
/// that it refused.
///
/// The loopback spelling is run first, and its success is the control: without
/// it, a refusal for some entirely unrelated reason — a missing store, an
/// unrelated usage error — would pass this test while proving nothing.
#[test]
fn a_daemon_address_naming_an_ip_the_daemon_will_not_bind_is_refused_out_loud() {
    let probe = Probe::new();
    let repo = probe.dir("repo");
    let data = probe.dir("data");

    ok(probe
        .story(&repo)
        .env("STORYHOOK_DATA_DIR", &data)
        .args(["project", "list"]));
    probe.quiesce(&repo, &data);

    let stderr = refused(
        probe
            .story(&repo)
            .env("STORYHOOK_DATA_DIR", &data)
            .env("STORYHOOK_DAEMON_ADDR", "0.0.0.0:3456")
            .args(["project", "list"]),
    );

    assert!(
        stderr.contains("STORYHOOK_DAEMON_ADDR"),
        "the refusal must name the variable it is about:\n{stderr}"
    );
    for offer in ["127.0.0.1:PORT", "--port", "tailnet"] {
        assert!(
            stderr.contains(offer),
            "the refusal fires on every `story` command, so it has to name the way \
             out — `{offer}` is missing from:\n{stderr}"
        );
    }

    // Nothing was started. The control run above spawned a daemon and
    // `quiesce` stood it down, so an unpublished port here is the refused run
    // declining to start another — a value refused during environment
    // resolution is refused before anything binds.
    assert_eq!(
        published_port(&probe),
        None,
        "a refused address must not leave a daemon behind"
    );
}

// ---------------------------------------------------------------------------
// SH-263: the e2e harness's fixture knobs reach the dispatch child it configures
// ---------------------------------------------------------------------------

/// Every `FAKE_TMUX_*` the e2e harness exports is exported **before** the
/// snapshot that carries the family across the dispatch child's cleared
/// environment.
///
/// A dispatch child's environment is cleared and rebuilt from an allowlist
/// (`src/env/spawn_env.rs`, SH-193) that no `FAKE_TMUX_*` name matches, so
/// `scripts/run-e2e.sh` hands them over by snapshotting them to one file per
/// knob and re-exporting them from a generated wrapper. The snapshot is a
/// moment in time: a knob exported after it is set in the harness and absent
/// from every dispatch child, which is precisely the failure this story is
/// about — `FAKE_TMUX_STATE` was exported there for a long time while every
/// dispatch child ignored it and used the fake's fixed shared default, and no
/// test said so because the specs pass either way until something else writes
/// to that directory.
///
/// Derived over the file rather than listing the knobs: the snapshot itself is
/// derived (`compgen -e`), so a knob added later crosses for free, and the only
/// way to break it is position.
#[test]
fn the_e2e_harness_exports_no_fake_tmux_knob_after_the_snapshot_that_forwards_them() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let relative = "scripts/run-e2e.sh";
    let text = std::fs::read_to_string(root.join(relative))
        .unwrap_or_else(|e| panic!("reading {relative}: {e}"));

    let snapshots: Vec<usize> = text
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("compgen -e"))
        .map(|(number, _)| number)
        .collect();
    assert_eq!(
        snapshots.len(),
        1,
        "{relative} must snapshot its FAKE_TMUX_* knobs exactly once (the \
         `compgen -e` loop); found {} such lines, so this scan cannot say which \
         exports are late",
        snapshots.len()
    );
    let snapshot = snapshots[0];

    let late: Vec<String> = text
        .lines()
        .enumerate()
        .filter(|(number, line)| {
            *number > snapshot
                && line
                    .trim_start()
                    .strip_prefix("export ")
                    .is_some_and(|rest| rest.trim_start().starts_with("FAKE_TMUX_"))
        })
        .map(|(number, line)| format!("line {}: {}", number + 1, line.trim()))
        .collect();
    assert!(
        late.is_empty(),
        "{relative} exports a fake-tmux knob after the snapshot that forwards \
         them to dispatch children, so that knob is set for the harness and \
         absent from every dispatch the daemon spawns (SH-263):\n{}",
        late.join("\n")
    );
}
