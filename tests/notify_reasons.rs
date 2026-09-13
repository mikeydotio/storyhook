//! Fences the verifier's classification of `story.sh notify` refusals against
//! the helper's own vocabulary (SH-650).
//!
//! `return_for_repair` (`src/daemon/verification.rs`) re-dispatches a returned
//! story into its own window when — and only when — `notify` refused with a
//! slug that means "no live dispatched agent is there" (`pane-dead`,
//! `pane-unavailable`, `pane-changed`); every other slug parks the story,
//! because a `dispatch --resume` respawns over whatever the pane holds and a
//! respawn over a live agent kills it. That decision is a table,
//! `NOTIFY_REFUSALS`, keyed by the exact slugs `cmd_notify` emits. A slug the
//! helper grows that the table does not know is classified "not absent" —
//! which is safe, and which is also silent: the verifier would park every
//! such story for ever and nobody would be told a classification was missing.
//! A slug the table names that the helper no longer emits is a classification
//! of nothing.
//!
//! So the two are derived against each other in both directions, in the
//! SH-198/SH-360/SH-364 style: DECLARED is every `refuse "<slug>"` (and
//! `refuse_with`) literal inside `cmd_notify`'s body, read from the tracked
//! `plugins/story/bin/story.sh`; CLASSIFIED is `NOTIFY_REFUSALS`'s keys. The
//! scanner carries a positive control so a parser that stopped matching
//! cannot report a clean tree by accident.
//!
//! The second pin is the probe spelling: `pane_is_dead` (`lib/session.sh`)
//! asks tmux the same composite question the Full Auto reconciler asks
//! (`WINDOW_PROBE_FORMAT`), because the fake tmux answers that one format in
//! one arm and a second spelling would be a third format for every fixture
//! to learn (SH-136). The two literals must stay byte-identical.

use std::collections::BTreeSet;
use std::path::PathBuf;

use storyhook::daemon::verification::NOTIFY_REFUSALS;
use storyhook::service::engine::WINDOW_PROBE_FORMAT;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
}

/// The body of one top-level bash function: from `name() {` to the first
/// line that is exactly `}`.
fn function_body<'a>(script: &'a str, name: &str) -> &'a str {
    let header = format!("\n{name}() {{\n");
    let start = script
        .find(&header)
        .unwrap_or_else(|| panic!("{name}() must be defined at top level"))
        + header.len();
    let end = script[start..]
        .find("\n}\n")
        .unwrap_or_else(|| panic!("{name}() must end with a bare closing brace"));
    &script[start..start + end]
}

/// Every slug passed as the first argument of `refuse` / `refuse_with` in
/// `body`, in the shape `refuse "slug"` or `refuse_with slug` (the helper
/// writes the slug as a double-quoted or bare word, never a variable).
fn refusal_slugs(body: &str) -> BTreeSet<String> {
    let mut slugs = BTreeSet::new();
    for verb in ["refuse_with", "refuse"] {
        let mut rest = body;
        while let Some(at) = rest.find(verb) {
            let after = &rest[at + verb.len()..];
            rest = after;
            // `refuse_with` also matches the prefix `refuse`; only accept a
            // call, which is the verb followed by whitespace.
            let Some(after) = after.strip_prefix(' ') else {
                continue;
            };
            let word = after.trim_start();
            let word = word.strip_prefix('"').unwrap_or(word);
            let end = word
                .find(|c: char| !(c.is_ascii_lowercase() || c == '-'))
                .unwrap_or(word.len());
            if end > 0 {
                slugs.insert(word[..end].to_string());
            }
        }
    }
    slugs
}

#[test]
fn the_scanner_reads_refusal_slugs_out_of_a_function_body() {
    let script = "\nother() {\n  refuse \"not-this-one\" \"x\"\n}\n\ncmd_probe() {\n  [ -n \"$x\" ] \\\n    || refuse \"pane-unavailable\" \"no window\"\n  refuse_with \"pane-changed\" \"$msg\" \"$json\"\n  refuse pane-dead \"bare word\"\n}\n";
    let body = function_body(script, "cmd_probe");
    assert_eq!(
        refusal_slugs(body),
        ["pane-changed", "pane-dead", "pane-unavailable"]
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>()
    );
}

#[test]
fn every_notify_refusal_is_classified_by_name_and_nothing_else_is() {
    let script = read("plugins/story/bin/story.sh");
    let mut declared = refusal_slugs(function_body(&script, "cmd_notify"));
    let resources = read("plugins/story/lib/resources.sh");
    declared.extend(refusal_slugs(function_body(
        &resources,
        "load_story_resources",
    )));
    declared.extend(refusal_slugs(function_body(
        &resources,
        "revalidate_story_resources",
    )));
    assert!(
        declared.len() >= 4,
        "corpus floor: cmd_notify is expected to refuse in several named ways, found {declared:?}"
    );
    let classified: BTreeSet<String> = NOTIFY_REFUSALS
        .iter()
        .map(|(slug, _)| (*slug).to_string())
        .collect();
    assert_eq!(
        classified.len(),
        NOTIFY_REFUSALS.len(),
        "a slug classified twice is a table that disagrees with itself"
    );
    let unclassified: Vec<_> = declared.difference(&classified).collect();
    assert!(
        unclassified.is_empty(),
        "cmd_notify emits {unclassified:?} which NOTIFY_REFUSALS does not classify; add it, saying whether it means the agent is absent"
    );
    let phantom: Vec<_> = classified.difference(&declared).collect();
    assert!(
        phantom.is_empty(),
        "NOTIFY_REFUSALS classifies {phantom:?} which cmd_notify never emits"
    );
}

#[test]
fn the_shell_pane_probe_asks_the_engines_own_question() {
    let session = read("plugins/story/lib/session.sh");
    let line = session
        .lines()
        .find(|line| line.starts_with("PANE_PROBE_FORMAT="))
        .expect("lib/session.sh defines PANE_PROBE_FORMAT");
    let literal = line
        .trim_start_matches("PANE_PROBE_FORMAT=")
        .trim_matches('\'');
    assert_eq!(
        literal, WINDOW_PROBE_FORMAT,
        "pane_is_dead must ask the composite format the reconciler and the fake tmux already speak"
    );
    assert!(
        function_body(&session, "pane_probe").contains("\"$PANE_PROBE_FORMAT\""),
        "pane_probe must ask with the named constant, not a second spelling"
    );
}
