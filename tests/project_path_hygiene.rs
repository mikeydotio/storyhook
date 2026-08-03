//! `story doctor`'s catalog half: what a project records about a directory, and
//! what the directory knows that the store does not.
//!
//! A linked checkout is a claim that a project can be opened at a path. Two
//! ways it goes wrong, and they want opposite answers:
//!
//! * the checkout is **gone** — the claim is stale, and `story doctor --fix`
//!   forgets it;
//! * the checkout **moved** — the claim is wrong, and
//!   `story project link checkout` records where it went.
//!
//! Both live here because the distinction is the interesting part: forgetting a
//! project that merely moved would be data loss dressed as tidying.
//!
//! The third case runs the other way. A project whose checkout **owns a git
//! origin nobody registered** is one a fresh clone cannot resolve, because
//! since SH-119 there is no index of directories to fall back on. `doctor`
//! reports it and `--fix` records it — but only where the checkout genuinely
//! owns the origin, because R4 is explicit that anything else is reported and
//! never guessed at.
//!
//! Everything runs against a data home under `CARGO_TARGET_TMPDIR`, which is
//! inside the checkout rather than under any temporary directory. That is
//! deliberate twice over: the catalog audit is deliberately silent in a
//! throwaway store (a fixture that has vanished is not a finding), and project
//! creation is refused for a temporary path in a real store, so the fixtures
//! have to be real paths too.

use std::path::{Path, PathBuf};

use storyhook_test_support::assert_selection_is_not_inherited;

/// A directory under `target/`, which is not a temporary directory.
fn real_dir(label: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(label);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creating a fixture directory");
    dir.canonicalize().expect("canonicalizing a fixture")
}

fn story(cwd: &Path, data_home: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = std::process::Command::new(storyhook_test_support::story_binary());
    cmd.current_dir(cwd)
        .env_clear()
        .env("HOME", data_home)
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("STORYHOOK_DATA_DIR", data_home)
        .args(args);
    // The cleared environment is the point; these two are put back because a
    // daemon that inherited a cleared one would prefer the developer's dashboard
    // port and outlive the run.
    for (name, value) in storyhook_test_support::daemon_containment() {
        cmd.env(name, value);
    }
    cmd.output().expect("running the binary under test")
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A data home, a persistent project to run commands from, and a second
/// project that the test is free to delete or move.
///
/// The working directory matters more than it looks. `CARGO_TARGET_TMPDIR` is
/// inside this checkout, and project resolution walks *up* from the working
/// directory — so a command run from a bare directory under `target/` resolves
/// storyhook's own pointer file and reports on storyhook. Every command below
/// therefore runs from a directory with a pointer file of its own.
///
/// That sentence used to be the whole defence, and a convention is one forgotten
/// `project new` from silence — the failure would be a green test asserting
/// against the developer's own tracker. [`assert_selection_is_not_inherited`]
/// now checks it (SH-121).
struct Fixture {
    data_home: PathBuf,
    workdir: PathBuf,
    checkout: PathBuf,
}

fn fixture(label: &str) -> Fixture {
    let data_home = real_dir(&format!("{label}-store"));
    let workdir = real_dir(&format!("{label}-workdir"));
    let checkout = real_dir(&format!("{label}-checkout"));
    for (dir, prefix) in [(&workdir, "WD"), (&checkout, "PH")] {
        let out = story(dir, &data_home, &["project", "new", "--prefix", prefix]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "init failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_selection_is_not_inherited(dir);
    }
    Fixture {
        data_home,
        workdir,
        checkout,
    }
}

/// The slug of the project registered at `needle`, read from `project list`.
fn slug_for(f: &Fixture, needle: &str) -> String {
    let listed = stdout(&story(&f.workdir, &f.data_home, &["project", "list"]));
    listed
        .lines()
        .find(|line| line.contains(needle))
        .and_then(|line| line.split_whitespace().next())
        .unwrap_or_else(|| panic!("no registration matching `{needle}` in:\n{listed}"))
        .to_string()
}

/// A registration whose directory is gone is reported — and reported as
/// *advice*, not as an integrity failure.
#[test]
fn doctor_reports_a_registration_whose_directory_is_gone() {
    let f = fixture("orphan-report");
    story(&f.checkout, &f.data_home, &["new", "A story worth keeping"]);
    std::fs::remove_dir_all(&f.checkout).expect("removing the checkout");

    let out = story(&f.workdir, &f.data_home, &["doctor"]);

    assert_eq!(
        out.status.code(),
        Some(0),
        "a missing directory is advice, not an integrity failure — an unplugged \
         external disk must not make `doctor` exit non-zero; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = stdout(&out);
    assert!(
        text.contains("no longer exists"),
        "doctor must say the path is gone: {text}"
    );
    assert!(
        text.contains("story doctor --fix") && text.contains("project link checkout"),
        "it must offer both answers — forget it, or point it somewhere real: {text}"
    );
    assert!(
        text.contains("1 story"),
        "the story count is what separates a fixture worth forgetting from real \
         work whose checkout moved: {text}"
    );
}

/// `--fix` forgets the path and *only* the path.
#[test]
fn doctor_fix_deregisters_the_orphan_but_keeps_the_stories() {
    let f = fixture("orphan-fix");
    story(&f.checkout, &f.data_home, &["new", "Survives the cleanup"]);
    std::fs::remove_dir_all(&f.checkout).expect("removing the checkout");

    let fixed = story(&f.workdir, &f.data_home, &["doctor", "--fix"]);
    assert_eq!(fixed.status.code(), Some(0), "doctor --fix must succeed");
    assert!(
        stdout(&fixed).contains("deregistered"),
        "it must say what it forgot: {}",
        stdout(&fixed)
    );

    // The stale *path* is gone — but the project is not, and that is the
    // point. `--fix` forgets where a checkout was, never the work recorded
    // against it, so the project stays listed with nowhere to open it. Under
    // `web list` this project vanished entirely, which is precisely how a
    // checkout that was merely unplugged used to become unreachable.
    let listed = stdout(&story(&f.workdir, &f.data_home, &["project", "list"]));
    assert!(
        !listed.contains(&f.checkout.display().to_string()),
        "the stale path must be gone: {listed}"
    );
    assert!(
        listed.contains("orphan-fix-checkout"),
        "the project itself survives and stays reachable: {listed}"
    );
    assert!(
        listed.contains("no checkout on this machine"),
        "and says why it has no path: {listed}"
    );

    // ...and reporting clean now.
    let again = story(&f.workdir, &f.data_home, &["doctor"]);
    assert!(
        !stdout(&again).contains("no longer exists"),
        "the orphan must not be reported twice: {}",
        stdout(&again)
    );
}

/// The moved-checkout case: `link checkout` records where it went.
///
/// One of three tests that were about `relink`, kept as *capability* tests
/// rather than deleted with the verb. Each pins something `link checkout` can do
/// that `relink` could not, and this one pins the part they share.
#[test]
fn link_checkout_records_a_checkout_that_moved() {
    let f = fixture("relink-move");
    story(
        &f.checkout,
        &f.data_home,
        &["new", "Moved with the checkout"],
    );

    // Move the checkout, pointer file and all.
    let moved = real_dir("relink-move-destination");
    std::fs::copy(
        f.checkout.join(".storyhook.toml"),
        moved.join(".storyhook.toml"),
    )
    .expect("carrying the pointer file across");
    let slug = slug_for(&f, "relink-move-checkout");
    std::fs::remove_dir_all(&f.checkout).expect("removing the old checkout");

    let out = story(
        &f.workdir,
        &f.data_home,
        &[
            "--project",
            &slug,
            "project",
            "link",
            "checkout",
            moved.to_str().unwrap(),
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "link checkout failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let listed = stdout(&story(&f.workdir, &f.data_home, &["project", "list"]));
    assert!(
        listed.contains(&format!("checkout  {}", moved.display())),
        "the recorded checkout is the new location: {listed}"
    );

    // And the project is usable from there, with its story — by the pointer file
    // the move carried, not by anything `link checkout` wrote. Linking a
    // checkout never touches resolution (D21), which is the whole difference
    // between the two facts a directory can be recorded as.
    let summary = stdout(&story(&moved, &f.data_home, &["summary"]));
    assert!(
        summary.contains("stories: 1"),
        "the story must have come with it: {summary}"
    );

    // The stale *path row* for the address it moved out of is a separate fact
    // with a separate answer. `relink` forgot it as a side effect; `doctor --fix`
    // is where that decision is taken now, with the story count in front of the
    // person taking it.
    story(&f.workdir, &f.data_home, &["doctor", "--fix"]);
    let after = stdout(&story(&f.workdir, &f.data_home, &["project", "list"]));
    assert!(
        !after.contains(&f.checkout.display().to_string()),
        "no trace of the address it moved out of may survive a --fix: {after}"
    );
}

/// A checkout carrying **another project's** pointer file is linked, not
/// refused — and the directory still resolves to the project its pointer names.
///
/// `relink` refused this outright, because it read the pointer file and required
/// the uuid to match. It had to: it wrote a `project_paths` row, so a mismatch
/// really would have left one directory resolving to two projects depending on
/// which door it came in by. `link checkout` writes a column nothing resolves
/// by, so the same arrangement is merely two projects whose repo-side work runs
/// in one tree — which is a monorepo, and SH-151's subject.
#[test]
fn link_checkout_does_not_read_the_pointer_file_it_is_pointed_at() {
    let f = fixture("relink-mismatch");
    let second = real_dir("relink-mismatch-other");
    story(&second, &f.data_home, &["project", "new", "--prefix", "QQ"]);
    let first_slug = slug_for(&f, "relink-mismatch-checkout");

    let out = story(
        &f.workdir,
        &f.data_home,
        &[
            "--project",
            &first_slug,
            "project",
            "link",
            "checkout",
            second.to_str().unwrap(),
        ],
    );

    assert_eq!(
        out.status.code(),
        Some(0),
        "a checkout link is not a claim on identity; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The load-bearing half: the directory still answers for the project its
    // pointer file names. If this ever changes, `link checkout` has become a
    // `project_paths` write and SH-151 has been foreclosed by accident.
    let summary = stdout(&story(&second, &f.data_home, &["project", "list"]));
    assert!(
        summary.contains(&format!("checkout  {}", second.display())),
        "the link is recorded: {summary}"
    );
    let ids = stdout(&story(
        &second,
        &f.data_home,
        &["new", "Whose project is this"],
    ));
    assert!(
        ids.contains("QQ-1"),
        "the directory must still resolve to the project its pointer names: {ids}"
    );
}

/// A directory with **no pointer file** is linked. This is the capability
/// `relink` did not have.
///
/// `relink` needed a `.storyhook.toml` in the directory it was pointed at and
/// answered exit 3 without one — which ruled out exactly the cases that most
/// need it: a fresh clone, a worktree, a checkout whose pointer was never
/// committed. `link checkout` asks the directory for nothing.
#[test]
fn link_checkout_accepts_a_directory_with_no_pointer_file() {
    let f = fixture("relink-nopointer");
    let bare = real_dir("relink-nopointer-bare");
    let slug = slug_for(&f, "relink-nopointer-checkout");

    let out = story(
        &f.workdir,
        &f.data_home,
        &[
            "--project",
            &slug,
            "project",
            "link",
            "checkout",
            bare.to_str().unwrap(),
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "a directory with no pointer file is exactly the case this replaces; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listed = stdout(&story(&f.workdir, &f.data_home, &["project", "list"]));
    assert!(
        listed.contains(&format!("checkout  {}", bare.display())),
        "and it is recorded: {listed}"
    );
}

// ---------------------------------------------------------------------------
// The origin backfill (SH-119, R4)
// ---------------------------------------------------------------------------

/// Runs `git <args>` in `cwd`, asserting success.
fn git(cwd: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(args)
        .output()
        .expect("running git");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository with an origin, and a project attached to it whose origin
/// registration has been withdrawn.
///
/// The withdrawal is what makes the fixture honest. `story project new`
/// registers an owned origin as it creates the project, so the only way to
/// build the state the backfill exists to repair — a project whose checkout
/// knows an origin the store does not — is to unlink it afterwards. That is
/// exactly the shape of every project created before origins were recorded at
/// all, which on the author's machine was all thirteen.
fn repository_with_an_unregistered_origin(label: &str) -> (Fixture, String, String) {
    let f = fixture(label);
    let origin = format!("https://github.com/acme/{label}.git");
    git(&f.checkout, &["init", "-q", "-b", "main"]);
    git(&f.checkout, &["remote", "add", "origin", &origin]);

    // `project new` ran before the repository existed, so nothing was
    // registered; a run now would register it, which is what `--fix` must do
    // instead. Re-running it here would defeat the test.
    let slug = slug_for(&f, &f.checkout.display().to_string());
    (f, slug, origin)
}

#[test]
fn doctor_reports_a_checkout_whose_origin_nobody_registered() {
    let (f, slug, origin) = repository_with_an_unregistered_origin("origin-report");

    let out = story(&f.workdir, &f.data_home, &["doctor"]);
    let report = stdout(&out);
    assert_eq!(
        out.status.code(),
        Some(0),
        "advice, not an integrity failure"
    );
    assert!(
        report.contains(&slug) && report.contains(&origin),
        "the report must name the project and the origin it could record: {report}"
    );
    assert!(
        report.contains("cannot resolve"),
        "and say what it costs: {report}"
    );
}

#[test]
fn doctor_fix_registers_the_origin_and_then_has_nothing_to_say() {
    let (f, slug, origin) = repository_with_an_unregistered_origin("origin-fix");

    let fixed = story(&f.workdir, &f.data_home, &["doctor", "--fix"]);
    assert_eq!(fixed.status.code(), Some(0));
    assert!(
        stdout(&fixed).contains("registered 1 origin") && stdout(&fixed).contains(&origin),
        "it must say what it recorded: {}",
        stdout(&fixed)
    );

    // The whole point of recording it: the directory now resolves by its
    // origin, with no pointer file involved at all.
    std::fs::remove_file(f.checkout.join(".storyhook.toml")).expect("removing the pointer");
    let listed = stdout(&story(
        &f.checkout,
        &f.data_home,
        &["project", "settings", "list"],
    ));
    assert!(
        !listed.is_empty(),
        "a checkout with no pointer must resolve by the origin just registered"
    );

    let again = stdout(&story(&f.workdir, &f.data_home, &["doctor"]));
    assert!(
        !again.contains(&slug) || !again.contains("no registered origin"),
        "a second run has nothing left to report: {again}"
    );
}

#[test]
fn doctor_reports_but_never_registers_an_origin_the_checkout_does_not_own() {
    // A project in a subdirectory of a repository. Its checkout reports the
    // enclosing repository's origin and does not own it, so registering it
    // would be one repository wearing two identities — the defect SH-151 was
    // filed to close. The report says so; `--fix` leaves it alone.
    let f = fixture("origin-inherited");
    git(&f.checkout, &["init", "-q", "-b", "main"]);
    git(
        &f.checkout,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/mono.git",
        ],
    );
    let service = f.checkout.join("service-a");
    std::fs::create_dir_all(&service).expect("creating the sub-project directory");
    let out = story(
        &service,
        &f.data_home,
        &["project", "new", "--prefix", "SA"],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let report = stdout(&story(&f.workdir, &f.data_home, &["doctor"]));
    assert!(
        report.contains("belongs to") && report.contains("service-a"),
        "the report must name the owner rather than offering to register: {report}"
    );

    let fixed = stdout(&story(&f.workdir, &f.data_home, &["doctor", "--fix"]));
    assert!(
        fixed.contains("left alone"),
        "`--fix` must say what it declined to guess at: {fixed}"
    );
    // The origin is recorded **once**, against the directory that owns it — the
    // repository's own top level, whose project `--fix` was right to register.
    // What must never happen is the sub-project taking it, which is the state
    // that makes a clone of the monorepo answer for the wrong service.
    let listed = stdout(&story(&f.workdir, &f.data_home, &["project", "list"]));
    assert_eq!(
        listed.matches("acme/mono").count(),
        1,
        "exactly one project may hold the repository's origin: {listed}"
    );
    let service_row = listed
        .split("  service-a ")
        .nth(1)
        .expect("a row for the sub-project");
    assert!(
        !service_row.contains("\n      origin"),
        "the sub-project must hold no origin at all: {service_row}"
    );
}
