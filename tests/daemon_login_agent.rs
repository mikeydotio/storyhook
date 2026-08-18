//! `story daemon status` names the login agent, and says when it is wrong.
//!
//! SH-411's install-time gate (`storyhook::daemon::install_guard`) stops a
//! binary `$PATH` does not run from being *given* a permanent seat. It cannot
//! reach a plist that is already on disk — `install()` is the only code that
//! ever writes one — so every machine registered before that gate existed stays
//! exactly as it was. Reporting is what reaches those, and this is the test of
//! it.
//!
//! Two properties are load-bearing and are asserted rather than assumed.
//!
//! **A deleted agent binary is reported without consulting `$PATH`.** The
//! machine SH-411 is about is one where `$PATH` names no `story` at all — that
//! is the whole reason the migration guard cannot see a launchd-started daemon
//! — so a report that could only compare against `$PATH` would have nothing to
//! say on precisely the machine that needs it. These fixtures run with `$PATH`
//! carrying the test binary's directory, so the comparison is live; the
//! `Missing` case is judged ahead of it either way.
//!
//! **The block appears in every branch of `status`.** The not-running branch is
//! where a deleted agent exe is the likeliest cause of what the reader came to
//! ask about.
//!
//! Nothing here runs `launchctl`. A fixture that bootstrapped for real would
//! register an agent pointing at the test binary into the developer's own login
//! session, under the one label this project owns — so the plist is planted by
//! hand and only ever read back.

use storyhook_test_support::{Project, TestEnv};

/// The label `storyhook::daemon::agent` writes under. Spelled out rather than
/// imported so this test would notice the constant changing, which is a change
/// to every installed machine's file name.
const LABEL: &str = "io.mikey.storyhook.daemon";

/// Plants a plist naming `exe`, in the shape `agent::plist` writes.
fn plant_agent(env: &TestEnv, exe: &str) -> std::path::PathBuf {
    let dir = env.home().join("Library/LaunchAgents");
    std::fs::create_dir_all(&dir).expect("the LaunchAgents directory");
    let path = dir.join(format!("{LABEL}.plist"));
    std::fs::write(
        &path,
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>daemon</string>
        <string>--serve</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
"#
        ),
    )
    .expect("planting the agent");
    path
}

fn project(env: &TestEnv) -> Project<'_> {
    env.project().seed_story("A real story").build()
}

/// Plants a plist at the bare label naming `exe`, carrying `--store-path
/// store` — the exact shape a pre-SH-414 `--store-path store daemon install`
/// wrote. Deliberately beside [`plant_agent`] rather than a parameter added to
/// it, so every existing call site stays byte-identical.
fn plant_agent_for_store(env: &TestEnv, exe: &str, store: &std::path::Path) -> std::path::PathBuf {
    let dir = env.home().join("Library/LaunchAgents");
    std::fs::create_dir_all(&dir).expect("the LaunchAgents directory");
    let path = dir.join(format!("{LABEL}.plist"));
    std::fs::write(
        &path,
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>--store-path</string>
        <string>{}</string>
        <string>daemon</string>
        <string>--serve</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
"#,
            store.display()
        ),
    )
    .expect("planting a foreign-store agent");
    path
}

/// The reported state a machine registered from a worktree is left in: the
/// agent names a binary that is gone, and nothing else would ever say so.
#[test]
fn status_names_a_login_agent_whose_binary_no_longer_exists() {
    let env = TestEnv::isolated();
    let project = project(&env);
    let gone = env.home().join("deleted-worktree/target/debug/story");
    plant_agent(&env, &gone.display().to_string());

    let reported = project
        .story()
        .args(["daemon", "status"])
        .output()
        .expect("status");
    let said = String::from_utf8_lossy(&reported.stdout).to_string();
    assert!(said.contains("login agent"), "{said}");
    assert!(
        said.contains(&gone.display().to_string()),
        "the report must name the binary the agent runs: {said}"
    );
    assert!(
        said.contains("no longer exists"),
        "and say what is wrong with it: {said}"
    );
    assert!(
        said.contains("story daemon install"),
        "a report with no remedy is unactionable: {said}"
    );
}

/// The same fact, from the branch a reader is most likely to be standing in
/// when they ask: nothing is running, and the agent that should have started
/// something names a binary that is gone.
///
/// `stop_daemon` immediately before the assertion rather than once at the top —
/// the standing rule, since the next `story` command starts another one.
#[test]
fn the_not_running_branch_names_the_agent_too() {
    let env = TestEnv::isolated();
    let project = project(&env);
    let gone = env.home().join("deleted-worktree/target/debug/story");
    plant_agent(&env, &gone.display().to_string());

    env.stop_daemon();
    let reported = project
        .story()
        .args(["daemon", "status"])
        .output()
        .expect("status");
    let said = String::from_utf8_lossy(&reported.stdout).to_string();
    assert!(
        said.contains("login agent"),
        "the branch where a dead agent is the likeliest cause must name it: {said}"
    );
    assert!(said.contains(&gone.display().to_string()), "{said}");
}

/// The negative control. Without it, every assertion above passes equally well
/// for a report that complains unconditionally.
#[test]
fn status_says_the_agent_is_not_installed_when_it_is_not() {
    let env = TestEnv::isolated();
    let project = project(&env);

    let reported = project
        .story()
        .args(["daemon", "status"])
        .output()
        .expect("status");
    let said = String::from_utf8_lossy(&reported.stdout).to_string();
    assert!(
        said.contains("login agent  not installed"),
        "absence is stated rather than left silent: {said}"
    );
    assert!(
        !said.contains("no longer exists"),
        "and nothing is complained about: {said}"
    );
}

/// A plist this build cannot find a program in is *reported*, never guessed at
/// — SH-312's rule that an unprovable outcome is reported as unprovable.
#[test]
fn status_reports_a_plist_it_cannot_read() {
    let env = TestEnv::isolated();
    let project = project(&env);
    let dir = env.home().join("Library/LaunchAgents");
    std::fs::create_dir_all(&dir).expect("the LaunchAgents directory");
    std::fs::write(dir.join(format!("{LABEL}.plist")), "not a plist at all")
        .expect("planting a broken agent");

    let reported = project
        .story()
        .args(["daemon", "status"])
        .output()
        .expect("status");
    let said = String::from_utf8_lossy(&reported.stdout).to_string();
    assert!(
        said.contains("cannot find a program in that plist"),
        "{said}"
    );
}

/// The migration seam a council found this fix must handle explicitly: a
/// pre-SH-414 `--store-path X daemon install` left `X`'s agent at the bare
/// label, which now means "the default store's own agent". `status` for the
/// default store must name the foreign store and both remedies, never report
/// `Health::Agrees` for an agent silently serving somebody else.
#[test]
fn status_reports_a_login_agent_that_serves_another_store() {
    let env = TestEnv::isolated();
    let project = project(&env);
    let other = env.home().join("other-project/store.db");
    plant_agent_for_store(&env, "/usr/local/bin/story", &other);

    let reported = project
        .story()
        .args(["daemon", "status"])
        .output()
        .expect("status");
    let said = String::from_utf8_lossy(&reported.stdout).to_string();
    assert!(said.contains("login agent"), "{said}");
    assert!(
        said.contains(&other.display().to_string()),
        "the report must name the store the agent actually serves: {said}"
    );
    assert!(
        said.contains("story --store-path"),
        "and the remedy that gives the foreign store its own agent: {said}"
    );
    assert!(
        !said.contains("$PATH runs"),
        "a foreign agent's binary is not this store's business to report on: {said}"
    );
}

/// The negative control for the test above: this store's own agent, correctly
/// installed, must never be reported as foreign.
#[test]
fn status_does_not_report_this_stores_own_agent_as_foreign() {
    let env = TestEnv::isolated();
    let project = project(&env);
    plant_agent(&env, "/usr/local/bin/story");

    let reported = project
        .story()
        .args(["daemon", "status"])
        .output()
        .expect("status");
    let said = String::from_utf8_lossy(&reported.stdout).to_string();
    assert!(
        !said.contains("actually serves"),
        "this store's own correctly-labeled agent must not read as foreign: {said}"
    );
}
