//! The error contract: every `AppError` variant, the exit code it produces, the
//! stream it is written to, and the shape of its `--json` envelope.
//!
//! Exit codes are the machine-readable half of this CLI's interface — `story.sh`
//! branches on exit 9 to detect a lost CAS claim, and exit 3 is how a caller
//! learns an id does not exist. The rearchitecture moves error construction
//! behind a service layer (W2) and then across a daemon hop (W5), and both are
//! the kind of change that silently turns a `NotFound` into a `Storage` or moves
//! a message from stderr to stdout. This file is what refuses to let that pass.
//!
//! The contract, per the SH-59 ruling recorded in `docs/rearch/STATE.md`:
//!
//! | form | stream | body |
//! |---|---|---|
//! | plain | **stderr**, stdout empty | `error: {message}\n` |
//! | `--json` | **stdout**, stderr empty | `{"error":…,"exit_code":N,"result":"error"}` |
//!
//! `StateConflict` is the one exception: its envelope uses `"result":"conflict"`
//! and carries `expected` and `actual`, because a lost compare-and-swap is a
//! result a caller acts on rather than a failure it reports.

use std::path::Path;

use storyhook::error::AppError;
use storyhook_test_support::{TestEnv, scratch_dir};

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/// One row: a variant, the contract it must satisfy, and a way to provoke it.
struct Case {
    /// Must match [`variant_name`], which is exhaustive over `AppError`.
    variant: &'static str,
    exit_code: i32,
    /// A fragment that identifies this variant's message. Deliberately a
    /// fragment and not the whole string: this file pins the *contract*, and
    /// pinning exact prose here would duplicate the golden corpus and turn every
    /// wording change into two failures.
    message: &'static str,
    /// Runs a real `story` invocation that ends in this variant. `json` selects
    /// the output form; everything else — fixture, environment, corruption — is
    /// the row's own business.
    provoke: fn(&TestEnv, bool) -> std::process::Output,
}

/// Every variant reachable through the CLI.
///
/// `SyncConflict` is absent because it is **not reachable**: it is constructed
/// nowhere in `src/`, and `web.rs` only maps it to HTTP 409. It is covered
/// instead by [`unreachable_variants_still_hold_their_exit_codes`], and
/// [`the_table_covers_every_variant`] is what stops it being forgotten.
fn cases() -> Vec<Case> {
    let mut cases = vec![
        Case {
            variant: "Usage",
            exit_code: 2,
            message: "unknown command",
            provoke: |env, json| run(env.project().build().path(), env, &["no-such-verb"], json),
        },
        Case {
            variant: "Validation",
            exit_code: 2,
            message: "is not defined",
            provoke: |env, json| {
                let project = env.project().seed_story("A story").build();
                run(
                    project.path(),
                    env,
                    &["move", "SH-1", "no-such-state"],
                    json,
                )
            },
        },
        Case {
            variant: "NotFound",
            exit_code: 3,
            message: "not found",
            provoke: |env, json| run(env.project().build().path(), env, &["show", "SH-999"], json),
        },
        Case {
            variant: "LockTimeout",
            exit_code: 4,
            // The only row that costs wall-clock time: `with_project_lock`
            // polls for a hard-coded 5s (src/lock.rs:23) with no env override,
            // so provoking a real timeout takes just over 5s. Worth it — exit 4
            // is otherwise unpinned, and W5 moves this lock into a daemon.
            message: "timed out waiting for the project write lock",
            provoke: |env, json| {
                let project = env.project().build();
                let root = project.path().to_path_buf();

                // The lock must be *held* before the child starts, or the child
                // takes it uncontended and the row proves nothing. The channel
                // is what makes that ordering real rather than hoped for.
                let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
                let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
                let holder = std::thread::spawn(move || {
                    storyhook::lock::with_project_lock(&root, || {
                        acquired_tx.send(()).expect("signalling lock acquisition");
                        // Held until the child has given up, so the timeout is
                        // the child's own 5s deadline and not a race with ours.
                        let _ = release_rx.recv();
                        Ok(())
                    })
                    .expect("the holder thread must acquire the lock");
                });
                acquired_rx
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .expect("the holder thread never acquired the project lock");

                // `new` is a write, so it goes through with_project_lock; a read
                // like `show` never takes the lock and would never time out.
                let out = run(project.path(), env, &["new", "Blocked on the lock"], json);
                let _ = release_tx.send(());
                holder.join().expect("the holder thread panicked");
                out
            },
        },
        Case {
            variant: "Integrity",
            exit_code: 5,
            message: "is missing state",
            provoke: |env, json| {
                let project = env.project().seed_story("A story").build();
                // An empty event log parses cleanly and then folds into a story
                // with no state — the definition of an integrity fault, as
                // opposed to a storage one.
                std::fs::write(story_log(project.path(), "SH-1"), "")
                    .expect("emptying the event log");
                run(project.path(), env, &["show", "SH-1"], json)
            },
        },
        Case {
            variant: "Storage",
            exit_code: 5,
            // Serde's own wording, arriving through `From<serde_json::Error>`.
            message: "expected ident",
            provoke: |env, json| {
                let project = env.project().seed_story("A story").build();
                // Unparseable bytes, as distinct from Integrity's parseable but
                // incomplete ones. Same exit code, different variant: the
                // message is the only thing that tells them apart, which is why
                // both rows are here.
                std::fs::write(story_log(project.path(), "SH-1"), "not json\n")
                    .expect("corrupting the event log");
                run(project.path(), env, &["show", "SH-1"], json)
            },
        },
        Case {
            variant: "StateConflict",
            exit_code: 9,
            message: "state conflict: expected `done`, was `todo`",
            provoke: |env, json| {
                let project = env.project().seed_story("A story").build();
                run(
                    project.path(),
                    env,
                    &["move", "SH-1", "in-progress", "--if-state", "done"],
                    json,
                )
            },
        },
    ];

    #[cfg(feature = "github-sync")]
    cases.extend([
        Case {
            variant: "GithubAuth",
            exit_code: 6,
            message: "STORYHOOK_GITHUB_TOKEN environment variable is not set",
            provoke: |env, json| {
                let project = env.project().build();
                // The config must exist first: without it `github-sync` runs
                // initial setup, which fails on the missing remote (Validation)
                // or blocks on an interactive prompt.
                std::fs::write(
                    project.path().join(".storyhook/github-sync.toml"),
                    "[github]\nowner = \"acme\"\nrepo = \"widgets\"\n\n[sync]\nmode = \"manual\"\n",
                )
                .expect("writing the github-sync config");
                // Reached before any socket is opened, so this is offline.
                // `env_remove` and not `env("")`: `env::var` returns Ok("") for
                // an empty value, which would sail past the check.
                let mut cmd = env.story(project.path());
                cmd.env_remove("STORYHOOK_GITHUB_TOKEN");
                finish(cmd, &["github-sync"], json)
            },
        },
        Case {
            variant: "GithubApi",
            exit_code: 7,
            message: "github api:",
            provoke: |env, json| {
                // No offline construction site exists — every one needs a
                // transport error or an HTTP response — so the transport is
                // made to fail deterministically instead. ureq reads ALL_PROXY
                // from the environment, and port 1 refuses instantly, so this
                // is independent of whether the machine has network at all.
                // `update --check` short-circuits before any download and needs
                // no project.
                let dir = scratch_dir();
                let mut cmd = env.story(dir.path());
                cmd.env("ALL_PROXY", "http://127.0.0.1:1");
                finish(cmd, &["update", "--check"], json)
            },
        },
    ]);

    cases
}

// ---------------------------------------------------------------------------
// The contract
// ---------------------------------------------------------------------------

/// Every reachable variant produces its exit code, on the right stream, in the
/// right shape — in both output forms.
#[test]
fn every_error_variant_holds_its_contract() {
    let env = TestEnv::shared();

    for case in cases() {
        // Both forms are provoked concurrently, on independent fixtures. It is
        // the LockTimeout row that makes this worth doing: provoking it costs a
        // real 5s wait, and running the two forms in sequence would spend that
        // twice for one row.
        let (plain, json) = std::thread::scope(|scope| {
            let plain = scope.spawn(|| (case.provoke)(env, false));
            let json = scope.spawn(|| (case.provoke)(env, true));
            (
                plain.join().expect("provoking the plain-text form"),
                json.join().expect("provoking the --json form"),
            )
        });

        // --- plain: `error: {message}` on stderr, nothing on stdout ---
        let stdout = String::from_utf8_lossy(&plain.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&plain.stderr).into_owned();

        assert_eq!(
            plain.status.code(),
            Some(case.exit_code),
            "{}: exit code. stderr was: {stderr}",
            case.variant
        );
        assert!(
            stderr.starts_with("error: "),
            "{}: the plain-text error must be prefixed `error: `; got {stderr:?}",
            case.variant
        );
        assert!(
            stderr.ends_with('\n'),
            "{}: the plain-text error must end in a newline; got {stderr:?}",
            case.variant
        );
        assert!(
            stderr.contains(case.message),
            "{}: stderr must identify the variant (looking for {:?}); got {stderr:?}",
            case.variant,
            case.message
        );
        assert!(
            stdout.is_empty(),
            "{}: stdout must stay empty so a failed run writes no machine-readable \
             result (SH-59); got {stdout:?}",
            case.variant
        );

        // --- --json: the envelope on stdout, nothing on stderr ---
        let stdout = String::from_utf8_lossy(&json.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&json.stderr).into_owned();

        assert_eq!(
            json.status.code(),
            Some(case.exit_code),
            "{}: --json must not change the exit code",
            case.variant
        );
        assert!(
            stderr.is_empty(),
            "{}: with --json the error belongs on stdout and stderr must stay \
             empty (SH-59); got {stderr:?}",
            case.variant
        );

        let envelope: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
            panic!(
                "{}: --json must print exactly one JSON document ({e}); got {stdout:?}",
                case.variant
            )
        });
        let object = envelope
            .as_object()
            .unwrap_or_else(|| panic!("{}: the envelope must be a JSON object", case.variant));

        assert_eq!(
            object.get("exit_code").and_then(|v| v.as_i64()),
            Some(i64::from(case.exit_code)),
            "{}: the envelope must report the same exit code the process returns \
             — a caller reading only stdout has nothing else to go on",
            case.variant
        );
        assert!(
            object
                .get("error")
                .and_then(|v| v.as_str())
                .is_some_and(|message| message.contains(case.message)),
            "{}: the envelope's `error` must identify the variant (looking for \
             {:?}); got {stdout}",
            case.variant,
            case.message
        );

        let expected_keys: Vec<&str> = if case.variant == "StateConflict" {
            // A lost compare-and-swap is a *result*, not a failure: `story.sh`
            // reads `actual` to report who won the claim.
            assert_eq!(
                object.get("result").and_then(|v| v.as_str()),
                Some("conflict"),
                "StateConflict must be labelled `conflict`, not `error`"
            );
            assert_eq!(
                object.get("expected").and_then(|v| v.as_str()),
                Some("done")
            );
            assert_eq!(object.get("actual").and_then(|v| v.as_str()), Some("todo"));
            vec!["actual", "error", "exit_code", "expected", "result"]
        } else {
            assert_eq!(
                object.get("result").and_then(|v| v.as_str()),
                Some("error"),
                "{}: every non-conflict failure is labelled `error`",
                case.variant
            );
            vec!["error", "exit_code", "result"]
        };

        let actual_keys: Vec<&str> = object.keys().map(String::as_str).collect();
        assert_eq!(
            actual_keys, expected_keys,
            "{}: the error envelope's key set is part of the contract — a new key \
             is a compatible addition only once every consumer tolerates it",
            case.variant
        );
    }
}

/// The exit codes themselves, asserted directly on the enum.
///
/// Covers `SyncConflict`, which no CLI path constructs, and gives every other
/// variant a second, invocation-independent witness: if a refactor renumbers a
/// code, this fails even for a variant whose trigger has moved.
#[test]
fn unreachable_variants_still_hold_their_exit_codes() {
    let expected = [
        (AppError::Usage(String::new()), 2),
        (AppError::Validation(String::new()), 2),
        (AppError::NotFound(String::new()), 3),
        (AppError::LockTimeout(String::new()), 4),
        (AppError::Integrity(String::new()), 5),
        (AppError::Storage(String::new()), 5),
        (AppError::GithubAuth(String::new()), 6),
        (AppError::GithubApi(String::new()), 7),
        (AppError::SyncConflict(String::new()), 8),
        (AppError::StateConflict(String::new(), String::new()), 9),
    ];
    for (error, code) in &expected {
        assert_eq!(
            error.exit_code(),
            *code,
            "{} must keep exit code {code}",
            variant_name(error)
        );
    }
}

/// Every variant is either exercised by [`cases`] or listed as unreachable.
///
/// The guard that matters is [`variant_name`]: its `match` is exhaustive, so an
/// eleventh `AppError` variant stops this file compiling until someone decides
/// which list it belongs in.
#[test]
fn the_table_covers_every_variant() {
    /// Constructed nowhere in `src/`; `web.rs` only maps it to HTTP 409. If a
    /// CLI path ever raises it, move it into [`cases`] with a real invocation.
    const UNREACHABLE: &[&str] = &["SyncConflict"];
    /// Compiled out with `--no-default-features`, so they cannot be required.
    const FEATURE_GATED: &[&str] = &["GithubAuth", "GithubApi"];

    let all = [
        AppError::Usage(String::new()),
        AppError::Validation(String::new()),
        AppError::NotFound(String::new()),
        AppError::LockTimeout(String::new()),
        AppError::Integrity(String::new()),
        AppError::Storage(String::new()),
        AppError::GithubAuth(String::new()),
        AppError::GithubApi(String::new()),
        AppError::SyncConflict(String::new()),
        AppError::StateConflict(String::new(), String::new()),
    ];
    let covered: Vec<&str> = cases().iter().map(|case| case.variant).collect();

    for error in &all {
        let name = variant_name(error);
        if UNREACHABLE.contains(&name) {
            assert!(
                !covered.contains(&name),
                "{name} is listed unreachable but the table provokes it — delete \
                 it from UNREACHABLE"
            );
            continue;
        }
        if !cfg!(feature = "github-sync") && FEATURE_GATED.contains(&name) {
            continue;
        }
        assert!(
            covered.contains(&name),
            "{name} has no row in the error table: every reachable AppError \
             variant needs a real invocation that produces it"
        );
    }
}

/// Exhaustive over `AppError` **on purpose**: adding a variant breaks this
/// match, which is what forces the new variant into the table above rather than
/// letting it ship with an unpinned exit code.
fn variant_name(error: &AppError) -> &'static str {
    match error {
        AppError::Usage(_) => "Usage",
        AppError::Validation(_) => "Validation",
        AppError::NotFound(_) => "NotFound",
        AppError::LockTimeout(_) => "LockTimeout",
        AppError::Integrity(_) => "Integrity",
        AppError::Storage(_) => "Storage",
        AppError::GithubAuth(_) => "GithubAuth",
        AppError::GithubApi(_) => "GithubApi",
        AppError::SyncConflict(_) => "SyncConflict",
        AppError::StateConflict(..) => "StateConflict",
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Runs `story <args>` in `cwd`, appending `--json` when asked.
fn run(cwd: &Path, env: &TestEnv, args: &[&str], json: bool) -> std::process::Output {
    finish(env.story(cwd), args, json)
}

/// Completes a prepared command with its arguments and output form.
fn finish(mut cmd: assert_cmd::Command, args: &[&str], json: bool) -> std::process::Output {
    cmd.args(args);
    if json {
        cmd.arg("--json");
    }
    cmd.output()
        .unwrap_or_else(|e| panic!("running `story {}`: {e}", args.join(" ")))
}

/// The on-disk event log for `id` — the file a row corrupts to provoke a
/// storage or integrity fault.
fn story_log(root: &Path, id: &str) -> std::path::PathBuf {
    root.join(".storyhook/open/stories")
        .join(format!("{id}.jsonl"))
}
