//! Which process the GitHub token comes from — SH-153.
//!
//! # The defect, in one sentence
//!
//! `get_github_token` reads `$STORYHOOK_GITHUB_TOKEN` with `std::env::var`, and
//! since SH-114 that read happens inside the daemon — so it sees the environment
//! of whichever client happened to spawn the daemon rather than the environment
//! of the person running the command.
//!
//! Two symptoms, and this file is one test for each. A user who exports the
//! token is told it is not set, because the daemon was started by some earlier
//! command that did not have it. And a daemon started by a client that *did*
//! have it lends that credential to every later caller, including one who
//! deliberately unset it — which is the same defect wearing its security hat,
//! and the one that outlives a rotated token.
//!
//! # Why these run offline, and the trap that makes that non-obvious
//!
//! Both commands get past the token check after the fix, and the next thing
//! `run_sync_with` does is call `api.github.com`. So the fixture has to make an
//! outbound request fail instantly without touching the network, and
//! `tests/error_contract.rs` already has the idiom: `ureq` reads `ALL_PROXY`
//! from the environment and port 1 refuses immediately.
//!
//! **`ALL_PROXY` alone is not usable here, and that is measured rather than
//! assumed.** `ureq` proxies loopback too, so a client that sets only
//! `ALL_PROXY` cannot reach its own daemon — it dies with "the storyhook daemon
//! stopped answering: io: Connection refused" before the command runs at all.
//! `error_contract.rs` gets away with it because `update --check` is a
//! `needs_no_store` command answered entirely in the client, which never opens a
//! socket to a daemon. Every test whose command crosses `/api/v1/invoke` needs
//! the `NO_PROXY` companion, which exempts loopback while leaving
//! `api.github.com` pointed at a refused port.
//!
//! The variables are set on the client that **spawns** the daemon as well as on
//! the one under test, because the daemon inherits its spawner's environment —
//! which is, after all, the thing this story is about. That is not a
//! precaution. The first draft of this file set them only on the command under
//! test, and the daemon — started earlier by the project build, which had no
//! proxy — made a real request to `api.github.com` and came back carrying
//! GitHub's own `Bad credentials`. The fixture was bitten by the defect it
//! tests. Hence [`respawn_daemon_offline`], which every test here calls before
//! the command it is actually about.

#![cfg(feature = "github-sync")]

use storyhook_test_support::{Project, TestEnv};

/// A proxy that refuses instantly, and a loopback exemption so the client can
/// still reach its own daemon.
///
/// Applied to every command in this file. See the module docs: without the
/// second entry the client never gets as far as sending its request.
const OFFLINE: [(&str, &str); 2] = [
    ("ALL_PROXY", "http://127.0.0.1:1"),
    ("NO_PROXY", "127.0.0.1,localhost"),
];

/// Gives `project` a github-sync configuration without going near the wizard.
///
/// `story github-sync` is the only writer of the `github.sync` document, and
/// reaching it is exactly what these tests are about — so the configuration is
/// seeded straight into the store, the way `tests/error_contract.rs` seeds it to
/// provoke `GithubAuth`. `acme/widgets` is a repository that does not exist;
/// nothing in either test depends on it existing, because the transport dies
/// before the name is resolved.
fn configure_github_sync(project: &Project<'_>) {
    use storyhook::store::{ReadOps as _, Store as _, WriteOps as _};

    let store = project.open_store();
    let id = project.project_id(&store);
    store
        .write(|tx| {
            let mut settings = tx.settings(id)?;
            settings.github_sync = Some(serde_json::json!({
                "github": { "owner": "acme", "repo": "widgets" },
                "sync": { "mode": "manual" },
            }));
            tx.put_settings(id, &settings)
        })
        .expect("seeding the github-sync configuration");
}

/// Stands the daemon down and starts a fresh one from a client carrying
/// [`OFFLINE`], optionally with a credential of its own.
///
/// Every test in this file needs this, because a daemon keeps the environment
/// of whichever client started it — so the proxy that makes these tests offline
/// only applies if the *spawning* client had it. `token` is what that spawning
/// client exports, which is the second thing these tests need to control.
fn respawn_daemon_offline(project: &Project<'_>, token: Option<&str>) {
    let env = project.env();
    env.stop_daemon();
    let mut cmd = env.story(project.path());
    cmd.envs(OFFLINE);
    match token {
        Some(token) => cmd.env("STORYHOOK_GITHUB_TOKEN", token),
        None => cmd.env_remove("STORYHOOK_GITHUB_TOKEN"),
    };
    cmd.args(["summary"]).assert().success();
}

/// **The measured repro.** A token exported by the caller must reach the work,
/// even though the daemon that runs the work was started by a shell that had no
/// token.
///
/// This is the ordinary case rather than an exotic one: the daemon is started by
/// whatever `story` command happens to run first — a git hook, an editor, an
/// agent, a `story list` — and only a user whose login shell exports the token
/// is spared. Here the project build is that first command, and it deliberately
/// carries no token.
///
/// The assertion is about the *reason* for the failure, not about success. The
/// run still fails, because there is no network and no real repository; what it
/// must not do is claim the variable is unset while the caller has it set.
#[test]
fn a_token_the_client_exported_reaches_the_daemon() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    configure_github_sync(&project);
    // The daemon is started by a client with no credential — the ordinary
    // case, and the one the story was filed about.
    respawn_daemon_offline(&project, None);

    let mut cmd = env.story(project.path());
    cmd.envs(OFFLINE);
    cmd.env("STORYHOOK_GITHUB_TOKEN", "ghp_exported_by_this_client");
    let output = cmd.args(["github-sync"]).output().expect("running the sync");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("environment variable is not set"),
        "the caller exported a token; the daemon must not report it missing: {stderr}"
    );
    assert_ne!(
        output.status.code(),
        Some(6),
        "exit 6 is `GithubAuth`, and the only auth failure available here is the \
         one this story is about: {stderr}"
    );
    assert!(
        stderr.contains("github api"),
        "the token got through, so the run should now fail at the transport the \
         fixture broke on purpose: {stderr}"
    );
}

/// **The security half.** A daemon that holds a token must not hand it to a
/// caller who does not.
///
/// The credential is the spawning client's, and the daemon keeps it for its
/// whole life — so without this, every later `story github-sync` on the machine
/// runs as whoever started the daemon, including after that token has been
/// rotated away, and including for a caller who unset it deliberately.
///
/// Deliberately the mirror image of the test above: there the answer must stop
/// being `GithubAuth`, here it must start being it.
#[test]
fn a_daemon_that_holds_a_token_does_not_lend_it_to_a_client_without_one() {
    let env = TestEnv::isolated();
    let project = env.project().build();
    configure_github_sync(&project);

    // The client that starts the daemon is the one with the credential. Any
    // command will do; what matters is that this is the process the daemon
    // inherits from.
    respawn_daemon_offline(&project, Some("ghp_belonging_to_the_spawner"));

    // A second caller, with no token of its own.
    let mut cmd = env.story(project.path());
    cmd.envs(OFFLINE);
    cmd.env_remove("STORYHOOK_GITHUB_TOKEN");
    let output = cmd.args(["github-sync"]).output().expect("running the sync");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(6),
        "this caller has no token, so the sync must refuse rather than spend the \
         daemon's: {stderr}"
    );
    assert!(
        stderr.contains("environment variable is not set"),
        "and the refusal must be the one that names what to do about it: {stderr}"
    );
}
