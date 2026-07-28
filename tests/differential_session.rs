//! Legacy versus store for `session-start`, and the two documented divergences
//! in the `web` catalog.

mod differential_support;

use differential_support::Differential;
use storyhook::cli::Invocation;

/// Creates a story in both legs.
fn new_story(differential: &Differential, title: &str, priority: Option<&str>) -> String {
    differential.step_id(
        "new",
        Invocation::New {
            title: title.into(),
            state: None,
            story_type: None,
            description: None,
            priority: priority.map(str::to_string),
            labels: None,
            assignee: None,
        },
    )
}

#[test]
fn session_start_agrees_on_an_empty_project() {
    let differential = Differential::new();
    differential.step("session-start", Invocation::SessionStart);
}

#[test]
fn session_start_agrees_on_the_counts_and_the_next_story() {
    let differential = Differential::new();
    new_story(&differential, "Ordinary", None);
    new_story(&differential, "Urgent", Some("critical"));
    differential.step("session-start", Invocation::SessionStart);
}

#[test]
fn session_start_agrees_when_a_story_is_blocked() {
    let differential = Differential::new();
    let blocker = new_story(&differential, "Blocker", None);
    let blocked = new_story(&differential, "Blocked", None);
    differential.step(
        "relate",
        Invocation::Relate {
            a: blocker,
            relation: "blocks".into(),
            b: blocked,
            remove: false,
        },
    );
    differential.step("session-start", Invocation::SessionStart);
}

#[test]
fn session_start_agrees_when_the_plugin_is_switched_off() {
    let differential = Differential::new();
    new_story(&differential, "Invisible", None);
    std::fs::write(
        differential
            .legacy_path()
            .join(".storyhook/plugin-config.toml"),
        "enabled = false\n",
    )
    .expect("writing the plugin config");
    differential.step(
        "session-start with the plugin off",
        Invocation::SessionStart,
    );
}

#[test]
fn session_start_agrees_on_a_nested_plugin_table() {
    let differential = Differential::new();
    new_story(&differential, "Invisible", None);
    std::fs::write(
        differential
            .legacy_path()
            .join(".storyhook/plugin-config.toml"),
        "[plugin]\nenabled = \"false\"\n",
    )
    .expect("writing the plugin config");
    differential.step("session-start, nested table", Invocation::SessionStart);
}

#[test]
fn session_start_agrees_on_a_malformed_plugin_config() {
    let differential = Differential::new();
    new_story(&differential, "Visible", None);
    std::fs::write(
        differential
            .legacy_path()
            .join(".storyhook/plugin-config.toml"),
        "this is not toml [",
    )
    .expect("writing the plugin config");
    differential.step("session-start, malformed config", Invocation::SessionStart);
}

#[test]
fn session_start_agrees_when_every_story_is_closed() {
    let differential = Differential::new();
    let id = new_story(&differential, "Finished", None);
    differential.step(
        "close it",
        Invocation::SetState {
            id,
            state: "done".into(),
            comment: None,
            if_state: None,
        },
    );
    differential.step("session-start", Invocation::SessionStart);
}

// --- why there is no `web` differential row -------------------------------
//
// There cannot be one, and the reason is a trap worth naming. The harness runs
// *both* legs in this process, and the legacy `web` catalog reads and writes
// `$HOME/.storyhook/registry.toml`. `storyhook_test_support::TestEnv` isolates
// child processes, not in-process library calls — so a differential row here
// reads the developer's real registry and, for `register`, writes a fixture
// path into it. An early version of this file did exactly that.
//
// The store leg's catalog behaviour is covered by `tests/service_catalog.rs`,
// which never touches `$HOME`. The two documented divergences:
//
//   * `web list` — the legacy registry is a second file that `story init` never
//     writes, so a fresh project is invisible until someone runs
//     `web register`. In the store, `init` records the checkout, so the project
//     is listed from the moment it exists.
//   * `web register` — legacy rejects a second registration of the same path.
//     The store records the path at `init`, so the same rule would make the
//     command permanently unusable; it refreshes the checkout instead.
