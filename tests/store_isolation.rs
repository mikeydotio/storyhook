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

use storyhook_test_support::{scratch_dir, story_binary};
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
            // Through the daemon, always. This whole file is about which daemon
            // answers, and `--local` has no daemon to be wrong about.
            .env("STORYHOOK_INVOKER", "daemon")
            // Never the production port: a suite that could bind 3456 would
            // fight the developer's own dashboard for it.
            .env("STORYHOOK_DAEMON_ADDR", "127.0.0.1:0")
            // A daemon this test starts must not outlive it.
            .env("STORYHOOK_PARENT_PID", std::process::id().to_string());
        cmd
    }

    /// A `story` invocation that runs in its own process.
    fn local_story(&self, cwd: &Path) -> Command {
        let mut cmd = self.story(cwd);
        cmd.env("STORYHOOK_INVOKER", "local");
        cmd
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
        .args(["project", "init", "--prefix", "AAA"]));
    ok(probe
        .story(&repo_a)
        .env("STORYHOOK_DATA_DIR", &store_a)
        .args(["new", "CANARY belongs to store A"]));

    ok(probe
        .story(&repo_b)
        .env("STORYHOOK_DATA_DIR", &store_b)
        .args(["project", "init", "--prefix", "BBB"]));

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
        "init",
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
        "init",
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
        .args(["project", "init", "--prefix", "VAR"]));
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
            "init",
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
        "init",
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
/// Local throughout for the ambient store, and never a daemon on it: a daemon
/// holds the database open with its own page cache and would answer from memory
/// rather than notice the file.
#[test]
fn a_store_path_run_leaves_the_ambient_store_byte_identical() {
    let probe = Probe::new();
    let ambient = probe.dir("ambient");
    let named = probe.file("stores/named.db");
    let repo = probe.dir("repo");
    let elsewhere = probe.dir("elsewhere");

    ok(probe
        .local_story(&repo)
        .env("STORYHOOK_DATA_DIR", &ambient)
        .args(["project", "init", "--prefix", "AMB"]));
    ok(probe
        .local_story(&repo)
        .env("STORYHOOK_DATA_DIR", &ambient)
        .args(["new", "Do not touch me"]));

    let store = ambient.join("store.db");
    let before = bytes(&store);
    let listed_before = ok(probe
        .local_story(&repo)
        .env("STORYHOOK_DATA_DIR", &ambient)
        .args(["list"]));

    for args in [
        vec!["project", "init", "--prefix", "NAMED"],
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

    assert_eq!(
        bytes(&store),
        before,
        "a --store-path run must leave the ambient store byte-identical"
    );
    let listed_after = ok(probe
        .local_story(&repo)
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
        "init",
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

    ok(with_ambient(&repo).args(["project", "init", "--prefix", "OLD"]));
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
        "init",
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
        "init",
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
