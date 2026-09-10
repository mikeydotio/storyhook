//! A child spawned from an `Environment` resolves that environment, not its
//! own process's (SH-633).
//!
//! Four places hand a `story.sh` child an [`Environment`]: dispatch, unclaim,
//! and the verifier's `notify`/`reap`. Every one of them used to publish only
//! the store. A `story` run inside such a child kept the store but resolved
//! its **state home** from whatever `HOME`/`XDG_STATE_HOME` the child had —
//! the developer's real ones — so it looked for the store's daemon under
//! `~/.local/state/storyhook/daemons/<key>`, found nothing, and started a
//! second daemon for the same store there. Measured: 1,199 such runtime
//! directories on one machine, from the e2e harness and from
//! `tests/verification_queue.rs`'s real-helper reap test alike.
//!
//! Two of the four doors are pinned in `tests/engine_dispatcher.rs` (stub
//! helpers echoing what they were handed); this file pins the verifier's door
//! the same way, and then asks the **real binary** the question the defect
//! was about — where `story` inside that child believes the daemon lives —
//! through `story daemon status`, which reports its paths without starting
//! anything. Last, a derived scan keeps publishing a store to a child behind
//! one door, [`Environment::child_vars`], so a fifth spawn site cannot
//! reintroduce the half-published environment by hand.

use std::path::{Path, PathBuf};

use storyhook::daemon::verification::{ShellVerificationActuator, VerificationActuator};
use storyhook::domain::Priority;
use storyhook::service::verification::{VerificationCandidate, VerificationProblem};
use storyhook_test_support::{FIXTURE_NOW, ServiceFixture, scratch_dir, story_binary};

/// A candidate the verifier can `notify` about; no lease, no pull request —
/// the helper is a stub and only its environment is under test.
fn candidate(fixture: &ServiceFixture, checkout: &Path) -> VerificationCandidate {
    VerificationCandidate {
        project: fixture.project(),
        project_slug: "fixture".into(),
        story_id: "SH-1".into(),
        title: "environment".into(),
        priority: Priority::High,
        created_at: FIXTURE_NOW.into(),
        verifying_since: Some(FIXTURE_NOW.into()),
        verifying_generation: None,
        checkout: checkout.to_path_buf(),
        cleanup_lease: None,
        pull_request: Err(VerificationProblem::MissingPullRequest),
    }
}

/// A helper that records its environment to `record` and answers `ok`.
fn env_recording_helper(dir: &Path, record: &Path) -> PathBuf {
    let script = dir.join("story.sh");
    std::fs::write(
        &script,
        format!(
            "#!/usr/bin/env bash\n/usr/bin/env > '{}'\nprintf '{{\"ok\":true}}\\n'\n",
            record.display()
        ),
    )
    .expect("writing the helper stub");
    script
}

/// A helper that asks the real `story` where its daemon lives, records the
/// answer to `record`, and answers `ok`.
fn status_recording_helper(dir: &Path, record: &Path) -> PathBuf {
    let script = dir.join("story.sh");
    std::fs::write(
        &script,
        format!(
            "#!/usr/bin/env bash\n\"$STORY_BIN\" daemon status > '{}' 2>&1\nprintf '{{\"ok\":true}}\\n'\n",
            record.display()
        ),
    )
    .expect("writing the helper stub");
    script
}

fn recorded_env(record: &Path) -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(record)
        .expect("the helper recorded its environment")
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

/// The verifier's `notify` door hands its child the store AND the state home
/// its own environment resolved.
#[test]
fn the_verifier_tells_its_child_the_state_home_beside_the_store() {
    let fixture = ServiceFixture::new();
    let scratch = scratch_dir();
    let record = scratch.path().join("env.txt");
    let helper = env_recording_helper(scratch.path(), &record);
    let actuator = ShellVerificationActuator::with_paths(
        fixture.env().clone(),
        helper,
        story_binary().to_path_buf(),
    );

    actuator
        .notify(&candidate(&fixture, scratch.path()), "hello")
        .expect("the stub helper answers ok");

    let seen = recorded_env(&record);
    // Stated directly rather than derived from `child_vars()`, so a
    // `child_vars` that stopped publishing the state home cannot also shrink
    // what this test expects of it.
    let state_home_root = fixture
        .env()
        .state_home()
        .parent()
        .expect("a state home is <XDG_STATE_HOME>/storyhook");
    assert_eq!(
        seen.get("XDG_STATE_HOME").map(String::as_str),
        Some(state_home_root.to_str().unwrap()),
        "the child was handed XDG_STATE_HOME={:?}; the environment it was spawned from resolves \
         its state home under {}",
        seen.get("XDG_STATE_HOME"),
        state_home_root.display()
    );
    assert_eq!(
        seen.get("STORYHOOK_STORE_PATH").map(String::as_str),
        Some(fixture.env().store_path().to_str().unwrap()),
        "positive control: the store was always published"
    );
}

/// The observed defect, end to end with the real binary: `story` inside the
/// verifier's child locates the daemon under the environment it was spawned
/// from — never under the test process's own `$HOME`.
#[test]
fn story_inside_a_verifier_child_finds_the_daemon_where_its_parent_does() {
    let fixture = ServiceFixture::new();
    let scratch = scratch_dir();
    let record = scratch.path().join("status.txt");
    let helper = status_recording_helper(scratch.path(), &record);
    let actuator = ShellVerificationActuator::with_paths(
        fixture.env().clone(),
        helper,
        story_binary().to_path_buf(),
    );

    actuator
        .notify(&candidate(&fixture, scratch.path()), "hello")
        .expect("the stub helper answers ok");

    let status = std::fs::read_to_string(&record).expect("the helper recorded `daemon status`");
    let reported: Vec<(&str, PathBuf)> = status
        .lines()
        .filter_map(|line| {
            let (label, path) = line.split_once(char::is_whitespace)?;
            matches!(label, "portfile" | "pidfile" | "log")
                .then(|| (label, PathBuf::from(path.trim())))
        })
        .collect();
    assert_eq!(
        reported.len(),
        3,
        "`daemon status` did not report its runtime paths; it said:\n{status}"
    );
    let expected = fixture.env().daemon_state_dir();
    for (label, path) in &reported {
        assert_eq!(
            path.parent(),
            Some(expected.as_path()),
            "`story daemon status` inside the child puts its {label} at {}; the environment it \
             was spawned from keeps this store's daemon under {}. A daemon started from there \
             would be a second daemon for one store, published somewhere nothing stands down.",
            path.display(),
            expected.display()
        );
    }
    assert!(
        !status.contains("not the build you are running"),
        "the child's `story` is a different build from the daemon's; the store is shared \
         between two binaries:\n{status}"
    );
}

/// Publishing a store to a child happens behind one door.
///
/// Derived over `git ls-files`, comments stripped: no production source sets
/// `STORYHOOK_STORE_PATH` on a `Command` except `Environment::child_vars`'s
/// own module, so a new spawn site cannot hand a child the store and forget
/// the state home the way four sites did. `main.rs` publishing the flag into
/// its **own** process (`env::set_var`) is a different operation and is not
/// matched.
#[test]
fn a_store_is_published_to_a_child_only_through_child_vars() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z", "--", "src/*.rs"])
        .output()
        .expect("listing this repository's tracked sources");
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so this scan proved nothing: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    // Assembled at run time so this file's own prose never matches.
    let marker = format!(".env(\"{}\"", "STORYHOOK_STORE_PATH");
    let door = "src/env/mod.rs";

    let mut offenders = Vec::new();
    let mut files = 0;
    for path in listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let relative = std::str::from_utf8(path).expect("a UTF-8 path");
        files += 1;
        let text = std::fs::read_to_string(root.join(relative))
            .unwrap_or_else(|e| panic!("reading {relative}: {e}"));
        if storyhook_test_support::without_rust_comments(&text).contains(&marker)
            && relative != door
        {
            offenders.push(relative.to_string());
        }
    }
    assert!(files > 20, "expected the whole tree, got {files} files");
    assert!(
        offenders.is_empty(),
        "{offenders:?} publish STORYHOOK_STORE_PATH to a child by hand. A child told the \
         store but not the state home starts a second daemon for that store under its own \
         state home (SH-633); publish through `Environment::child_vars` instead."
    );

    // Positive control: the door itself is where the name is set, so a scan
    // that stopped recognising the shape would fail here rather than pass.
    let door_text = std::fs::read_to_string(root.join(door)).expect("reading the door");
    assert!(
        storyhook_test_support::without_rust_comments(&door_text).contains("STORYHOOK_STORE_PATH"),
        "the door no longer names the variable; the scan is checking the wrong shape"
    );
}
