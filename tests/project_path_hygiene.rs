//! Registrations that point at nothing, and checkouts that moved.
//!
//! A registration is a claim that a project can be opened at a path. Two ways
//! it goes wrong, and they want opposite answers:
//!
//! * the checkout is **gone** — the claim is stale, and `story doctor --fix`
//!   forgets it;
//! * the checkout **moved** — the claim is wrong, and `story relink` corrects
//!   it.
//!
//! Both live here because the distinction is the interesting part: forgetting a
//! project that merely moved would be data loss dressed as tidying.
//!
//! Everything runs against a data home under `CARGO_TARGET_TMPDIR`, which is
//! inside the checkout rather than under any temporary directory. That is
//! deliberate twice over: the catalog audit is deliberately silent in a
//! throwaway store (a fixture that has vanished is not a finding), and project
//! creation is refused for a temporary path in a real store, so the fixtures
//! have to be real paths too.

use std::path::{Path, PathBuf};

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
        let out = story(dir, &data_home, &["project", "init", "--prefix", prefix]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "init failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
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
        text.contains("story doctor --fix") && text.contains("story relink"),
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

/// The moved-checkout case: `relink` points the project at its new home.
#[test]
fn relink_points_a_project_at_its_new_checkout() {
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
        &["relink", &slug, moved.to_str().unwrap()],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "relink failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let listed = stdout(&story(&f.workdir, &f.data_home, &["project", "list"]));
    assert!(
        listed.contains(moved.to_str().unwrap()),
        "the catalog must name the new location: {listed}"
    );
    // **Both** facts move, not just the one resolution reads. A directory is
    // recorded twice — as a `project_paths` row and as `checkout_path` — and a
    // relink that carried only the first would leave the project claiming its
    // repo-side work runs in a directory that no longer exists.
    assert!(
        !listed.contains(&f.checkout.display().to_string()),
        "no trace of the address it moved out of may survive: {listed}"
    );
    assert!(
        listed.contains(&format!("checkout  {}", moved.display())),
        "and the recorded checkout is the new location: {listed}"
    );

    // And the project is usable from there, with its story.
    let summary = stdout(&story(&moved, &f.data_home, &["summary"]));
    assert!(
        summary.contains("stories: 1"),
        "the story must have come with it: {summary}"
    );
}

/// Relinking across identities is refused.
///
/// Without this check `relink` would be a way to staple one project's identity
/// onto another project's checkout, and the store would then resolve one
/// repository to two projects depending on which door it came in by.
#[test]
fn relink_refuses_a_pointer_naming_a_different_project() {
    let f = fixture("relink-mismatch");
    let second = real_dir("relink-mismatch-other");
    story(
        &second,
        &f.data_home,
        &["project", "init", "--prefix", "QQ"],
    );
    let first_slug = slug_for(&f, "relink-mismatch-checkout");

    let out = story(
        &f.workdir,
        &f.data_home,
        &["relink", &first_slug, second.to_str().unwrap()],
    );

    assert_eq!(
        out.status.code(),
        Some(2),
        "relinking across identities must be refused"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("different project"),
        "the refusal must say why: {stderr}"
    );
    assert!(
        stderr.contains("story project init"),
        "and name the command that does adopt a checkout: {stderr}"
    );
}

/// A directory with no pointer file cannot be relinked to.
#[test]
fn relink_refuses_a_directory_with_no_pointer_file() {
    let f = fixture("relink-nopointer");
    let bare = real_dir("relink-nopointer-bare");
    let slug = slug_for(&f, "relink-nopointer-checkout");

    let out = story(
        &f.workdir,
        &f.data_home,
        &["relink", &slug, bare.to_str().unwrap()],
    );
    assert_eq!(
        out.status.code(),
        Some(3),
        "nothing to relink to is a not-found"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(".storyhook.toml"),
        "the message must name what is missing"
    );
}
