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

/// Whether a row's two output forms may be provoked at the same time.
///
/// True for every row but one, and keyed on `variant` because that *is* this
/// table's primary key: [`variant_name`] is exhaustive over `AppError` and
/// [`the_table_covers_every_variant`] fails if a name here answers to nothing,
/// so this cannot quietly stop matching the row it is about.
///
/// `LockTimeout` takes the store's **exclusive** write lock and holds it on
/// purpose. Its two forms run the same closure, so one form's holder can be
/// sitting on the lock while the other form's fixture is still being built — and
/// `story project new` is a write. The fixture then waits out `busy_timeout` and
/// dies at exit 4 during *setup*, which reads as a product failure and is not
/// one.
///
/// **That race was always here.** It never fired because creation reached its
/// transaction faster than the sibling thread reached the lock — a margin
/// nothing declared and nothing protected. SH-116 gave `init` a `git`
/// subprocess to run first, the margin went, and it fired on every run. Running
/// this row's two forms in sequence costs one extra second — the child's own
/// `STORYHOOK_BUSY_TIMEOUT_MS` — and removes the race instead of re-widening the
/// margin, which is the difference between a fix and a postponement.
fn forms_may_run_together(variant: &str) -> bool {
    variant != "LockTimeout"
}

/// Every variant reachable through the CLI.
///
/// `SyncConflict` and `SyncErrors` are absent for a reason that changed with
/// SH-152 (`SyncConflict`) and SH-159 (`SyncErrors`) and is worth stating
/// precisely. `SyncConflict` used to be constructed nowhere at all; it is now
/// what `github-sync` answers with when a merge conflict is left undecided.
/// `SyncErrors` is what it answers with when one or more individual stories
/// failed to sync. Provoking either here needs a live GitHub issue reached
/// through this file's `provoke: fn(&TestEnv, bool) -> std::process::Output`,
/// i.e. a real `story` subprocess. SH-158 gave `GithubClient` a trait seam
/// (`github::api::GithubApi`), which is what makes both variants provokable
/// *in-process*, calling `github::run_sync_with` directly against a fake the
/// way `github::outcome_tests` and the tests beside it do — but nothing wires
/// that seam into the subprocess this file's rows spawn, so both stay
/// *reachable* and not *provokable here*, which is what [`UNPROVOKABLE`]
/// means. Their exit codes are held by
/// [`unreachable_variants_still_hold_their_exit_codes`], their refusal text by
/// `github::outcome_tests`, and [`the_table_covers_every_variant`] is what
/// stops either being forgotten.
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
            // The only row that costs wall-clock time, and it used to cost
            // five whole seconds: a writer waits `busy_timeout` for another
            // process's write lock before giving up. The deadline is a value on
            // `Environment` now rather than a constant, so this row asks the
            // child for a one-second one and the gate gets four seconds back.
            // What it proves is unchanged — the timeout fires, and exit 4 is
            // what a caller sees.
            //
            // The lock moved with the storage: it used to be `src/lock.rs`'s
            // advisory file lock over a project directory, and it is SQLite's
            // own write lock over the store now. Same contract, one storage
            // model down.
            message: "timed out",
            provoke: |env, json| {
                let project = env.project().build();

                // The lock must be *held* before the child starts, or the child
                // takes it uncontended and the row proves nothing. The channel
                // is what makes that ordering real rather than hoped for.
                let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
                let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
                let store_path = env.store_path().to_path_buf();
                let holder = std::thread::spawn(move || {
                    let conn = rusqlite::Connection::open(&store_path)
                        .expect("opening the store directly");
                    conn.busy_timeout(std::time::Duration::from_secs(30))
                        .expect("setting a busy timeout");
                    conn.execute_batch("BEGIN IMMEDIATE")
                        .expect("taking the store's write lock");
                    acquired_tx.send(()).expect("signalling lock acquisition");
                    // Held until the child has given up, so the timeout is the
                    // child's own deadline and not a race with ours.
                    let _ = release_rx.recv();
                    conn.execute_batch("ROLLBACK").expect("releasing the lock");
                });
                acquired_rx
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .expect("the holder thread never acquired the store's write lock");

                // `new` is a write, so it needs the lock; a read like `show`
                // runs happily beside a writer under WAL and would never time
                // out.
                let mut cmd = env.story(project.path());
                cmd.env("STORYHOOK_BUSY_TIMEOUT_MS", "1000");
                let out = finish(cmd, &["new", "Blocked on the lock"], json);
                let _ = release_tx.send(());
                holder.join().expect("the holder thread panicked");
                out
            },
        },
        Case {
            variant: "Integrity",
            exit_code: 5,
            // `story doctor` is the integrity surface, and this is the one
            // integrity fault the schema cannot refuse outright: two stories
            // whose *histories* disagree about an edge. The row used to be
            // provoked by emptying an event log, which folded into a story with
            // no state — a shape a store with a foreign key and a NOT NULL
            // column simply cannot hold.
            message: "missing inverse relation",
            provoke: |env, json| {
                let project = env.project().seed_story("A").seed_story("B").build();
                let store = project.open_store();
                let id = project.project_id(&store);
                storyhook::store::test_support::inject_events(
                    &store,
                    id,
                    project.story_no(&store, "SH-1"),
                    &[storyhook::domain::StoryEvent::StoryRelationshipAdded {
                        at: "2026-01-01T00:00:00Z".to_string(),
                        other_id: "SH-2".to_string(),
                        relation: "blocks".to_string(),
                    }],
                )
                .expect("injecting a one-sided relation");
                run(project.path(), env, &["doctor"], json)
            },
        },
        Case {
            variant: "Storage",
            exit_code: 5,
            // Serde's own wording, arriving through `From<serde_json::Error>`.
            // Provoked from an input document rather than from a corrupted
            // event log: the log is a table now, and its `payload` column is
            // read defensively by design — an undecodable event is *reported*
            // rather than fatal (SH-54).
            message: "expected ident",
            provoke: |env, json| {
                let project = env.project().build();
                let document = project.path().join("broken.json");
                std::fs::write(&document, "not json\n").expect("writing a broken document");
                run(
                    project.path(),
                    env,
                    &["import", &document.to_string_lossy()],
                    json,
                )
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
                // or blocks on an interactive prompt. It is a column now, not
                // `.storyhook/github-sync.toml` — same configuration, one
                // storage model down.
                let store = project.open_store();
                let id = project.project_id(&store);
                {
                    use storyhook::store::{ReadOps as _, Store as _, WriteOps as _};
                    store
                        .write(|tx| {
                            let mut settings = tx.settings(id)?;
                            settings.github_sync = Some(serde_json::json!({
                                "github": { "owner": "acme", "repo": "widgets" },
                                "sync": { "mode": "manual" },
                            }));
                            tx.put_settings(id, &settings)
                        })
                        .expect("writing the github-sync config");
                }
                // Reached before any socket is opened, so this is offline.
                //
                // No `env_remove` here any more: `TestEnv` clears
                // `STORYHOOK_GITHUB_TOKEN` from every fixture command it builds
                // (`CLEARED_VARS`). This row used to carry its own removal,
                // which was one test compensating locally for a harness-wide
                // gap — on a developer machine with a real token exported,
                // every *other* fixture inherited it (SH-153).
                finish(env.story(project.path()), &["github-sync"], json)
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
        // Both forms are provoked concurrently, on independent fixtures — except
        // for the one row whose fixtures are not independent at all, because it
        // holds the store's exclusive write lock. See `forms_may_run_together`.
        let (plain, json) = if forms_may_run_together(case.variant) {
            std::thread::scope(|scope| {
                let plain = scope.spawn(|| (case.provoke)(env, false));
                let json = scope.spawn(|| (case.provoke)(env, true));
                (
                    plain.join().expect("provoking the plain-text form"),
                    json.join().expect("provoking the --json form"),
                )
            })
        } else {
            ((case.provoke)(env, false), (case.provoke)(env, true))
        };

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
        } else if case.variant == "Integrity" {
            // An integrity report carries its findings as data (SH-244), so a
            // caller reads `field`/`persisted`/`rebuilt` off a divergence
            // instead of regexing them out of `error` — which SH-243 had to do,
            // over 1.68MB. `error` itself is unchanged: it is these findings'
            // own messages joined, so the existing consumers
            // (`plugin/claude-code/bin/story.sh::_project_integrity` reads
            // `.result` and `.error`) are untouched, and the two keys are a
            // strict addition.
            assert_eq!(
                object.get("result").and_then(|v| v.as_str()),
                Some("error"),
                "an integrity report is still a failure"
            );
            let findings = object
                .get("findings")
                .and_then(|v| v.as_array())
                .expect("findings must be an array");
            assert!(
                !findings.is_empty(),
                "an integrity error carrying no findings is unrepresentable — \
                 `IntegrityDetail::report` returns None for one"
            );
            assert!(
                findings
                    .iter()
                    .all(|finding| finding.get("code").is_some_and(|c| c.is_string())
                        && finding.get("message").is_some_and(|m| m.is_string())),
                "every finding names its check and carries its sentence: {findings:?}"
            );
            // The prose is exactly the findings' messages, then the advice —
            // an equality, so neither form can drift from the other.
            let rendered: Vec<&str> = findings
                .iter()
                .filter_map(|finding| finding.get("message").and_then(|m| m.as_str()))
                .chain(
                    object
                        .get("advice")
                        .and_then(|v| v.as_array())
                        .into_iter()
                        .flatten()
                        .filter_map(|entry| entry.as_str()),
                )
                .collect();
            assert_eq!(
                object.get("error").and_then(|v| v.as_str()),
                Some(rendered.join("\n").as_str()),
                "`error` must be its findings and advice joined, not a second rendering"
            );
            vec!["advice", "error", "exit_code", "findings", "result"]
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
/// Covers `SyncConflict` and `SyncErrors`, which no test can provoke offline,
/// and gives every other variant a second, invocation-independent witness: if
/// a refactor renumbers a code, this fails even for a variant whose trigger
/// has moved.
#[test]
fn unreachable_variants_still_hold_their_exit_codes() {
    let expected = [
        (AppError::Usage(String::new()), 2),
        (AppError::Validation(String::new()), 2),
        (AppError::NotFound(String::new()), 3),
        (AppError::LockTimeout(String::new()), 4),
        (AppError::Integrity("a finding".into()), 5),
        (AppError::Storage(String::new()), 5),
        (AppError::GithubAuth(String::new()), 6),
        (AppError::GithubApi(String::new()), 7),
        (AppError::SyncConflict(String::new()), 8),
        (AppError::StateConflict(String::new(), String::new()), 9),
        (AppError::SyncErrors(String::new()), 10),
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
    /// Raised by `github-sync` on an undecided conflict (SH-152) or a
    /// per-story sync failure (SH-159), but neither is reproducible in this
    /// file: both need a GitHub issue reached through the API client, which
    /// has no test seam wired into the subprocess this file spawns. Move
    /// either into [`cases`] with a real invocation once one exists.
    const UNPROVOKABLE: &[&str] = &["SyncConflict", "SyncErrors"];
    /// Compiled out with `--no-default-features`, so they cannot be required.
    const FEATURE_GATED: &[&str] = &["GithubAuth", "GithubApi"];

    let all = [
        AppError::Usage(String::new()),
        AppError::Validation(String::new()),
        AppError::NotFound(String::new()),
        AppError::LockTimeout(String::new()),
        AppError::Integrity("a finding".into()),
        AppError::Storage(String::new()),
        AppError::GithubAuth(String::new()),
        AppError::GithubApi(String::new()),
        AppError::SyncConflict(String::new()),
        AppError::StateConflict(String::new(), String::new()),
        AppError::SyncErrors(String::new()),
    ];
    let covered: Vec<&str> = cases().iter().map(|case| case.variant).collect();

    for error in &all {
        let name = variant_name(error);
        if UNPROVOKABLE.contains(&name) {
            assert!(
                !covered.contains(&name),
                "{name} is listed unprovokable but the table provokes it — delete \
                 it from UNPROVOKABLE"
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
        AppError::SyncErrors(_) => "SyncErrors",
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
