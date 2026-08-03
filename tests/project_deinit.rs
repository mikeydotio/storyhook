//! `story project deinit` — the one command that destroys work.
//!
//! It removes the project, every story, every event, every checkout
//! registration, and the repository-side files `init` generated. There is no
//! undo, so almost everything here is about the gate rather than the deletion:
//! what the warning says, what happens when nobody can answer it, and what
//! survives when the answer is no.
//!
//! The confirmation is deliberately a **typed slug** rather than `[y/N]`. A
//! single keystroke is the right weight for "reopen this deleted story" and the
//! wrong weight for "erase four thousand events".

use std::path::Path;

use storyhook_test_support::TestEnv;

/// A project with `stories` stories in it, at `<home>/<name>`.
fn project_with(env: &TestEnv, name: &str, stories: usize) -> std::path::PathBuf {
    let dir = env.home().join(name);
    std::fs::create_dir_all(&dir).expect("creating the repository");
    env.story(&dir)
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    for index in 1..=stories {
        env.story(&dir)
            .args(["new", &format!("Story {index}")])
            .assert()
            .success();
    }
    dir
}

/// Runs `story project deinit …` with stdin closed — no terminal, which is what
/// every test process has anyway.
fn deinit(env: &TestEnv, cwd: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = env.story(cwd);
    cmd.args(["project", "deinit"]);
    cmd.args(args);
    cmd.output().expect("running story project deinit")
}

/// Whether the store still knows a project by this slug.
///
/// Matches the listing's row shape rather than searching the whole output for
/// the slug. A substring search passes for the wrong reason here: the empty
/// catalog's own message — "run `story project init` in a repository" —
/// contains "repo", so a test whose fixture is called `repo` would see its
/// project as still present precisely when it had successfully deleted it.
fn store_has_project(env: &TestEnv, cwd: &Path, slug: &str) -> bool {
    let out = env
        .story(cwd)
        .args(["project", "list"])
        .output()
        .expect("listing projects");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|line| line.trim_start().starts_with(&format!("{slug} — ")))
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

#[test]
fn without_force_and_without_a_terminal_it_refuses_and_names_the_flag() {
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", 2);

    let out = deinit(&env, &dir, &[]);

    assert_eq!(
        out.status.code(),
        Some(2),
        "a refusal is a usage error; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--force"),
        "the refusal must name the way past it: {stderr}"
    );
    assert!(
        store_has_project(&env, &dir, "repo"),
        "a refused deinit destroys nothing"
    );
    assert!(dir.join(".storyhook.toml").exists());
}

#[test]
fn the_refusal_says_what_would_have_been_destroyed() {
    // A flag named without the stakes is a flag people pass reflexively.
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", 3);

    let out = deinit(&env, &dir, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(stderr.contains('3'), "the story count is stated: {stderr}");
    assert!(stderr.contains("repo"), "the project is named: {stderr}");
}

#[test]
fn json_without_force_refuses_rather_than_prompting_into_the_stream() {
    // `--json` promises one self-describing document on stdout. A prompt
    // written there would corrupt it for every scripted caller.
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", 1);

    let out = deinit(&env, &dir, &["--json"]);

    assert_eq!(out.status.code(), Some(2));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.trim().is_empty() || serde_json::from_str::<serde_json::Value>(&stdout).is_ok(),
        "stdout must stay machine-readable: {stdout}"
    );
    assert!(store_has_project(&env, &dir, "repo"));
}

// ---------------------------------------------------------------------------
// The deletion
// ---------------------------------------------------------------------------

#[test]
fn force_destroys_the_project_its_stories_and_its_events() {
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", 3);
    let elsewhere = env.home().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();

    let out = deinit(&env, &dir, &["--force"]);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!store_has_project(&env, &elsewhere, "repo"));
    // The directory stops resolving: no project here any more.
    env.story(&dir).arg("summary").assert().failure();
}

#[test]
fn force_removes_the_pointer_file() {
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", 1);
    assert!(dir.join(".storyhook.toml").exists());

    deinit(&env, &dir, &["--force"]);

    assert!(!dir.join(".storyhook.toml").exists());
}

#[test]
fn a_neighbouring_project_is_untouched() {
    let env = TestEnv::isolated();
    let doomed = project_with(&env, "doomed", 2);
    let kept = project_with(&env, "kept", 2);

    deinit(&env, &doomed, &["--force"]);

    assert!(store_has_project(&env, &kept, "kept"));
    // Both projects took the default prefix, so both minted `SH-1`. That is the
    // shape a scope bug would show up in: a delete that reached across projects
    // by story number rather than by project would take this one with it.
    for number in 1..=2 {
        env.story(&kept)
            .args(["show", &format!("SH-{number}")])
            .assert()
            .success()
            .stdout(predicates::str::contains(format!("Story {number}")));
    }
}

// ---------------------------------------------------------------------------
// AGENTS.md — generated content, possibly edited since
// ---------------------------------------------------------------------------

#[test]
fn an_untouched_agents_md_is_removed() {
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", 0);
    assert!(dir.join("AGENTS.md").exists());

    deinit(&env, &dir, &["--force"]);

    assert!(
        !dir.join("AGENTS.md").exists(),
        "a file storyhook generated and nobody touched is storyhook's to remove"
    );
}

#[test]
fn an_edited_agents_md_is_kept() {
    // `init` refuses to overwrite an existing AGENTS.md precisely because it
    // may be the user's. Deleting one it did generate but the user then edited
    // would destroy exactly what that care was protecting.
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", 0);
    let agents = dir.join("AGENTS.md");
    let original = std::fs::read_to_string(&agents).unwrap();
    std::fs::write(&agents, format!("{original}\n<!-- my notes -->\n")).unwrap();

    let out = deinit(&env, &dir, &["--force"]);

    assert!(agents.exists(), "an edited file must survive");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("AGENTS.md"),
        "and the output must say it was kept: {stdout}"
    );
}

#[test]
fn an_agents_md_the_user_wrote_themselves_is_kept() {
    let env = TestEnv::isolated();
    let dir = env.home().join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("AGENTS.md"), "mine, from before\n").unwrap();
    env.story(&dir)
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    deinit(&env, &dir, &["--force"]);

    assert_eq!(
        std::fs::read_to_string(dir.join("AGENTS.md")).unwrap(),
        "mine, from before\n"
    );
}

// ---------------------------------------------------------------------------
// Partial states, and naming a target
// ---------------------------------------------------------------------------

#[test]
fn deinit_takes_a_path_like_init_does() {
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", 1);
    let here = env.home().join("here");
    std::fs::create_dir_all(&here).unwrap();

    let out = deinit(&env, &here, &[dir.to_str().unwrap(), "--force"]);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!store_has_project(&env, &here, "repo"));
}

#[test]
fn deinit_takes_a_slug_so_a_project_with_no_checkout_can_be_reached() {
    // The project this most needs to reach is one whose directory is gone.
    // There is no path left to name it by, and it is exactly the project a
    // user wants rid of.
    let env = TestEnv::isolated();
    let gone = project_with(&env, "gone", 1);
    let here = project_with(&env, "here", 1);
    std::fs::remove_dir_all(&gone).unwrap();
    env.story(&here)
        .args(["doctor", "--fix"])
        .assert()
        .success();
    assert!(store_has_project(&env, &here, "gone"));

    let out = deinit(&env, &here, &["gone", "--force"]);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!store_has_project(&env, &here, "gone"));
    assert!(store_has_project(&env, &here, "here"));
}

#[test]
fn deinit_in_a_directory_with_no_project_says_so() {
    let env = TestEnv::isolated();
    let bare = env.home().join("bare");
    std::fs::create_dir_all(&bare).unwrap();

    let out = deinit(&env, &bare, &["--force"]);

    assert_ne!(out.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.is_empty(), "the failure must say something");
}

#[test]
fn deinit_survives_a_pointer_file_that_was_already_deleted_by_hand() {
    // A partially deinitialized checkout is a state a user can reach with `rm`,
    // and refusing to finish the job would leave them with no way to.
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", 1);
    std::fs::remove_file(dir.join(".storyhook.toml")).unwrap();

    let out = deinit(&env, &dir, &["repo", "--force"]);

    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!store_has_project(&env, &dir, "repo"));
}

#[test]
fn deinit_is_idempotent_in_the_only_sense_it_can_be() {
    // Running it twice is not a no-op — there is nothing left to delete — but
    // it must fail cleanly rather than half-succeed or panic.
    let env = TestEnv::isolated();
    let dir = project_with(&env, "repo", 1);
    let here = env.home().join("here");
    std::fs::create_dir_all(&here).unwrap();

    assert_eq!(
        deinit(&env, &here, &[dir.to_str().unwrap(), "--force"])
            .status
            .code(),
        Some(0)
    );
    let second = deinit(&env, &here, &[dir.to_str().unwrap(), "--force"]);

    assert_ne!(second.status.code(), Some(0));
    assert!(!String::from_utf8_lossy(&second.stderr).is_empty());
}

/// `main` reads standard input *before* dispatching, for the commands that
/// consume it. Deinit must not be one: draining stdin first would leave the
/// confirmation prompt reading EOF, and every deinit would silently cancel.
#[test]
fn deinit_must_not_read_stdin_before_it_prompts() {
    use storyhook::cli::{Invocation, ProjectAction};

    for force in [false, true] {
        assert!(
            !storyhook::invoke::reads_stdin(&Invocation::Project {
                action: ProjectAction::Deinit {
                    target: None,
                    force,
                },
            }),
            "the prompt needs the terminal that `reads_stdin` would consume"
        );
    }
}

/// Deinit clears the repository files of **every** recorded checkout, not only
/// the one the caller happened to name.
///
/// It destroys the project outright, so a `.storyhook.toml` left behind in a
/// sibling worktree names an identity that no longer exists — and the next
/// `story project init` there would silently resurrect that identity as an
/// empty project. Reaching into a directory the caller did not name is safe
/// only because the plan lists every file first; that listing is the consent.
#[test]
fn deinit_clears_every_checkout_it_knows_about() {
    let env = TestEnv::isolated();
    let main = project_with(&env, "multi", 1);
    let second = env.home().join("multi-worktree");
    std::fs::create_dir_all(&second).unwrap();
    std::fs::copy(main.join(".storyhook.toml"), second.join(".storyhook.toml")).unwrap();
    // Resolution deliberately does not register new checkouts, so merely
    // running a command there is not enough — `init` in a checkout carrying a
    // pointer adopts it into the project the pointer names.
    env.story(&second)
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    // The plan must name both before anything is destroyed.
    let refused = deinit(&env, &main, &[]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains(&second.display().to_string()),
        "the second checkout must be named in the warning: {stderr}"
    );

    deinit(&env, &main, &["--force"]);

    assert!(!main.join(".storyhook.toml").exists());
    assert!(
        !second.join(".storyhook.toml").exists(),
        "a pointer naming a deleted project is a file claiming an identity that is gone"
    );
}
