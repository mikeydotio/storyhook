//! `story project new` — the verb that replaces `story project init`
//! (SH-117, C5 of the server-owned epic SH-112).
//!
//! # Three differences, and this file is about all three
//!
//! * **No positional of any kind.** A bare word after `new` could be a display
//!   name or a directory with equal plausibility, and `deinit` already showed
//!   what happens when a parser has to guess. The checkout is named by
//!   `--attach PATH`, defaulting to the directory the client ran in.
//! * **`--no-attach` exists**, so "a project whose repository is elsewhere, or
//!   nowhere yet" is finally sayable. It writes the store record and touches
//!   nothing else.
//! * **`--prefix` is required** whenever any switch is present. `init`'s silent
//!   `SH` is SH-109, and a prefix is minted into every id a project ever
//!   creates: it is the one field here that cannot be undone.
//!
//! # What is deliberately unchanged
//!
//! `new --attach` does exactly what `init` does — the same registration, the
//! same origin claim, the same files, and the same idempotence. That identity
//! is not decoration: it is the premise the 251-site fixture sweep rests on,
//! and `attaching_twice_adopts_rather_than_refusing` is the assertion that
//! keeps it true.

use std::path::Path;

use storyhook_test_support::TestEnv;

/// Runs `story project new …` in `cwd` and returns the raw output.
fn new(env: &TestEnv, cwd: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = env.story(cwd);
    cmd.args(["project", "new"]);
    cmd.args(args);
    cmd.output().expect("running `story project new`")
}

/// A directory inside the environment's scratch space, created but not
/// initialized.
fn bare_dir(env: &TestEnv, name: &str) -> std::path::PathBuf {
    let dir = env.home().join(name);
    std::fs::create_dir_all(&dir).expect("creating a bare directory");
    dir
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Everything `story project list` printed, run from `cwd`.
fn listing(env: &TestEnv, cwd: &Path) -> String {
    let out = env
        .story(cwd)
        .args(["project", "list"])
        .output()
        .expect("running `story project list`");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ---------------------------------------------------------------------------
// the grammar
// ---------------------------------------------------------------------------

#[test]
fn new_creates_a_usable_project_in_the_working_directory() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");

    let out = new(&env, &dir, &["--prefix", "ACME"]);
    assert!(out.status.success(), "{}", stderr(&out));

    env.story(&dir).arg("summary").assert().success();
    env.story(&dir)
        .args(["new", "The first story"])
        .assert()
        .success()
        .stdout(predicates::str::contains("ACME-1"));
    assert!(dir.join("AGENTS.md").exists(), "new generates AGENTS.md");
}

/// `--attach` is what a positional path used to be, and it reaches a directory
/// other than the one the command ran in — the property SH-95's two-sided guard
/// cannot be expressed without.
#[test]
fn attach_names_a_directory_other_than_the_one_the_client_ran_in() {
    let env = TestEnv::isolated();
    let here = bare_dir(&env, "here");
    let there = bare_dir(&env, "there");

    let out = new(
        &env,
        &here,
        &["--prefix", "SH", "--attach", there.to_str().unwrap()],
    );
    assert!(out.status.success(), "{}", stderr(&out));

    env.story(&there).arg("summary").assert().success();
    assert!(there.join(".storyhook.toml").exists());
    assert!(!here.join(".storyhook.toml").exists());
    env.story(&here).arg("summary").assert().failure();
}

/// The daemon's own working directory is wherever it was spawned. A relative
/// `--attach` resolved there would create a project at a path nobody named, and
/// the failure would be invisible: a project *would* appear, just not here.
#[test]
fn a_relative_attach_resolves_against_the_directory_the_client_ran_in() {
    let env = TestEnv::isolated();
    let here = bare_dir(&env, "here");
    let sub = bare_dir(&env, "here/sub");

    let out = new(&env, &here, &["--prefix", "SH", "--attach", "./sub"]);
    assert!(out.status.success(), "{}", stderr(&out));

    assert!(sub.join(".storyhook.toml").exists(), "created under `here`");
    env.story(&sub).arg("summary").assert().success();
}

/// Idempotence is unchanged from `init`, and that is load-bearing: it is what
/// makes rewriting 251 fixture call sites a text substitution rather than 251
/// judgement calls.
#[test]
fn attaching_twice_adopts_rather_than_refusing() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");
    new(&env, &dir, &["--prefix", "SH"]);
    env.story(&dir).args(["new", "First"]).assert().success();

    let again = new(&env, &dir, &["--prefix", "SH"]);
    assert!(again.status.success(), "{}", stderr(&again));

    env.story(&dir)
        .args(["new", "Second"])
        .assert()
        .success()
        .stdout(predicates::str::contains("SH-2"));
}

// ---------------------------------------------------------------------------
// --no-attach
// ---------------------------------------------------------------------------

/// The store record and nothing else. Asserted as four separate absences
/// rather than one, because each of them is a different subsystem deciding on
/// its own to leave the filesystem alone.
#[test]
fn no_attach_writes_the_store_record_and_touches_nothing_else() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "elsewhere");

    let out = new(&env, &dir, &["--prefix", "REM", "--name", "remote-only"]);
    assert!(out.status.success(), "{}", stderr(&out));
    // Sanity: the attached form *would* have written both of these here.
    assert!(dir.join(".storyhook.toml").exists());

    let detached = bare_dir(&env, "detached");
    let out = new(
        &env,
        &detached,
        &["--prefix", "OFF", "--name", "off-machine", "--no-attach"],
    );
    assert!(out.status.success(), "{}", stderr(&out));

    assert!(
        !detached.join(".storyhook.toml").exists(),
        "--no-attach must write no pointer file"
    );
    assert!(
        !detached.join("AGENTS.md").exists(),
        "--no-attach must write no AGENTS.md"
    );
    // No resolution row: standing in the directory resolves nothing.
    env.story(&detached).arg("summary").assert().failure();
    // …and no checkout is recorded either.
    let listed = listing(&env, &dir);
    let canonical = detached.canonicalize().expect("canonicalizing");
    assert!(
        !listed.contains(&canonical.display().to_string()),
        "--no-attach must record no path anywhere:\n{listed}"
    );
}

/// A project nothing resolves to is reachable only by name, so the message that
/// creates one has to say the name. Without it a user is holding a project they
/// cannot address.
#[test]
fn no_attach_reports_the_slug_the_project_must_be_named_by() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "anywhere");

    let out = new(
        &env,
        &dir,
        &["--prefix", "OFF", "--name", "off-machine", "--no-attach"],
    );
    let message = stdout(&out);
    assert!(
        message.contains("off-machine"),
        "the slug is the only handle this project has:\n{message}"
    );

    env.story(&dir)
        .args(["--project", "off-machine", "summary"])
        .assert()
        .success();
}

// ---------------------------------------------------------------------------
// refusals
// ---------------------------------------------------------------------------

#[test]
fn attach_and_no_attach_contradict_each_other() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");

    let out = new(
        &env,
        &dir,
        &["--prefix", "SH", "--attach", ".", "--no-attach"],
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("--no-attach"), "{}", stderr(&out));
}

/// A bare word is refused rather than guessed at, and the refusal names the two
/// flags it could have meant.
#[test]
fn a_bare_word_after_new_is_refused_naming_the_flags_it_might_have_been() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");

    let out = new(&env, &dir, &["my-project", "--prefix", "SH"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let message = stderr(&out);
    assert!(message.contains("--name"), "{message}");
    assert!(message.contains("--attach"), "{message}");
    assert!(
        listing(&env, &dir).contains("No projects yet"),
        "a refused invocation must create nothing"
    );
}

/// The defect SH-109 is about, closed at the one place it can be closed. The
/// refusal names both the missing flag and the switch that made asking
/// impossible, because "pass --prefix" is advice and "you passed --name, so I
/// will not ask" is a diagnosis.
#[test]
fn a_switch_without_a_prefix_is_refused_naming_the_switch_that_did_it() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");

    let out = new(&env, &dir, &["--name", "my-project"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let message = stderr(&out);
    assert!(message.contains("--prefix"), "{message}");
    assert!(message.contains("--name"), "{message}");
    assert!(
        listing(&env, &dir).contains("No projects yet"),
        "a refused invocation must create nothing"
    );
}

/// Every switch is a trigger, not just the interesting ones.
#[test]
fn every_verb_local_switch_makes_the_prefix_required() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");

    for switch in [
        vec!["--name", "x"],
        vec!["--attach", "."],
        vec!["--no-attach"],
        vec!["--no-agents-md"],
    ] {
        let out = new(&env, &dir, &switch);
        assert_eq!(
            out.status.code(),
            Some(2),
            "`{switch:?}` must require --prefix: {}",
            stderr(&out)
        );
        assert!(stderr(&out).contains("--prefix"), "{}", stderr(&out));
    }
}

/// There is no prefix validator anywhere before SH-117: `--prefix 'hello
/// world'` was accepted and minted `hello world-1`, an id the CLI cannot parse
/// back. Validation, not Usage — the value was understood and rejected.
#[test]
fn an_invalid_prefix_is_refused_before_anything_is_created() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");

    for bad in ["hello world", "SH-X", "1SH", "ABCDEFGHIJK", ""] {
        let out = new(&env, &dir, &["--prefix", bad]);
        assert_eq!(
            out.status.code(),
            Some(2),
            "`{bad}` must be refused: {}",
            stderr(&out)
        );
        assert!(
            stderr(&out).contains("prefix"),
            "the refusal must name the field: {}",
            stderr(&out)
        );
    }
    assert!(
        listing(&env, &dir).contains("No projects yet"),
        "no refused prefix may leave a project behind"
    );
}

/// Case is folded rather than refused, so `--prefix sh` and `--prefix SH` are
/// the same answer and neither mints lowercase ids.
#[test]
fn a_lowercase_prefix_is_canonicalized_rather_than_refused() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");

    new(&env, &dir, &["--prefix", "acme"]);
    env.story(&dir)
        .args(["new", "The first story"])
        .assert()
        .success()
        .stdout(predicates::str::contains("ACME-1"));
}

#[test]
fn attaching_a_directory_that_is_not_there_refuses_naming_it() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");

    let out = new(&env, &dir, &["--prefix", "SH", "--attach", "/no/such/dir"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("/no/such/dir"), "{}", stderr(&out));
}

/// SH-62's fail-closed gate covers this verb because D13 declared its flags.
/// An undeclared flag-shaped token is refused ahead of the parser.
#[test]
fn new_refuses_a_flag_it_does_not_declare() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");

    let out = new(&env, &dir, &["--prefix", "SH", "--no-such-flag"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("--no-such-flag"), "{}", stderr(&out));
}

/// With no switch there is nothing to work from, and over the daemon there is
/// nobody to ask. Refused rather than defaulted: quietly supplying a prefix
/// nobody chose is SH-109's silent `SH` wearing a new verb.
#[test]
fn a_bare_new_with_nobody_to_ask_refuses_and_creates_nothing() {
    let env = TestEnv::isolated();
    let dir = bare_dir(&env, "repo");

    let out = new(&env, &dir, &[]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("--prefix"), "{}", stderr(&out));
    assert!(
        listing(&env, &dir).contains("No projects yet"),
        "nothing may be created by a refusal"
    );
}
