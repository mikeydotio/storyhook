//! The test-environment parameter set, and everything that has to agree with it.
//!
//! `storyhook::env::test_environment::TEST_ENVIRONMENT` is the one definition of
//! what isolating a storyhook run means. Before it existed the same list sat in
//! seven hand-copied places that had already drifted; these tests are what stop
//! an eighth appearing, and what stop the copies that remain from disagreeing.
//!
//! Every scan here is **derived** — from the table, or from `git ls-files` — for
//! the reason a hand-kept list is exactly the failure this story exists to fix.

use storyhook::env::test_environment::{Disposition, TEST_ENVIRONMENT};
use storyhook::help_topics::get_help_topic;

/// The topic's own key. Named once here rather than typed at each assertion.
const TOPIC: &str = "test-environment";

fn topic_body() -> &'static str {
    get_help_topic(TOPIC).unwrap_or_else(|| {
        panic!("`story help {TOPIC}` must exist — it is how a suite in another repository learns to isolate itself")
    })
}

/// Every parameter reaches the shipped text.
///
/// Trivially true today, because the topic is rendered from the table. That is
/// the point: this test is what fails if somebody replaces the rendering with a
/// hand-written copy, which is the change that would look like an
/// improvement and would silently freeze the documentation at today's list.
#[test]
fn the_help_topic_names_every_parameter() {
    let body = topic_body();
    let missing: Vec<&str> = TEST_ENVIRONMENT
        .iter()
        .map(|parameter| parameter.name)
        .filter(|name| !body.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "`story help {TOPIC}` does not mention {missing:?}. A suite reading that \
         topic would isolate itself incompletely and never be told."
    );
}

/// …and the topic names no storyhook variable the table does not.
///
/// The other direction, and the one that catches stale prose: a paragraph that
/// still names a variable the parameter set has dropped tells a reader to set
/// something storyhook no longer reads, which is worse than saying nothing.
#[test]
fn the_help_topic_names_no_variable_the_parameter_set_omits() {
    let body = topic_body();
    let known: Vec<&str> = TEST_ENVIRONMENT
        .iter()
        .map(|parameter| parameter.name)
        .collect();

    let mut strays: Vec<String> = Vec::new();
    for word in body.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        if !word.starts_with("STORYHOOK_") {
            continue;
        }
        if !known.contains(&word) && !strays.iter().any(|s| s == word) {
            strays.push(word.to_string());
        }
    }
    assert!(
        strays.is_empty(),
        "`story help {TOPIC}` names {strays:?}, which is not in TEST_ENVIRONMENT. \
         Either add the parameter or stop naming it in the topic — a variable \
         documented as part of the contract and absent from the code is one a \
         reader will set for no effect."
    );
}

/// The scan above can only prove anything if it actually finds variable names.
///
/// A positive control in the SH-364 shape: a parser that stopped recognising
/// `STORYHOOK_*` words would report a clean tree forever. The fixture is
/// assembled at run time so this file's own source carries no stray name for
/// the scan to trip over.
#[test]
fn the_stray_variable_scan_can_see_a_stray() {
    let prefix = "STORYHOOK";
    let planted = format!("{prefix}_A_VARIABLE_NOBODY_DEFINED");
    let body = format!("some prose mentioning ${planted} in passing");

    let known: Vec<&str> = TEST_ENVIRONMENT
        .iter()
        .map(|parameter| parameter.name)
        .collect();
    let found: Vec<&str> = body
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|word| word.starts_with("STORYHOOK_"))
        .filter(|word| !known.contains(word))
        .collect();
    assert_eq!(
        found,
        [planted.as_str()],
        "the scan in the test above cannot see a planted stray, so its silence \
         proves nothing"
    );
}

/// The topic's first line has to parse as an invocation.
///
/// `tests/help_topic_usage.rs` reads the leading block of every topic as usage
/// lines, exactly as it already does for `priority-rubric` and `scope-rubric`.
/// Asserted here too, next to the topic it is about, because the failure over
/// there names a parsing rule rather than this topic.
#[test]
fn the_help_topic_opens_with_its_own_invocation() {
    let first = topic_body().lines().next().expect("a non-empty topic");
    assert_eq!(first, format!("story help {TOPIC}"));
}

/// The shipped text is written for a stranger's repository.
///
/// `story scaffold` points other projects at storyhook's help, so a topic that
/// cites this tracker's own story ids is telling a reader to look up something
/// they cannot see. The same boundary `tests/priority_rubric.rs` keeps.
#[test]
fn the_help_topic_cites_no_story_of_this_projects_own() {
    let body = topic_body();
    let cited: Vec<&str> = body
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .filter(|word| {
            word.strip_prefix("SH-")
                .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
        })
        .collect();
    assert!(
        cited.is_empty(),
        "`story help {TOPIC}` cites {cited:?}. This text ships to repositories \
         whose trackers have no such ids; put the case study in CLAUDE.md and \
         keep the rule here."
    );
}

/// A parameter that names a *file* is the one shape `directories()` has to
/// treat differently, and there is exactly one of them.
///
/// Pinned so that adding a second file-valued parameter is a decision somebody
/// makes on purpose: `directories()` infers "this tail is a file" from it
/// carrying an extension, which is true of this layout and would stop being
/// true silently.
#[test]
fn only_the_store_names_a_file() {
    let with_extension: Vec<&str> = TEST_ENVIRONMENT
        .iter()
        .filter(|parameter| match parameter.disposition {
            Disposition::Root(tail) => std::path::Path::new(tail).extension().is_some(),
            _ => false,
        })
        .map(|parameter| parameter.name)
        .collect();
    assert_eq!(
        with_extension,
        ["STORYHOOK_STORE_PATH"],
        "`directories()` reads a trailing extension as \"this parameter names a \
         file, so create its parent\". A second one is fine, but say so here."
    );
}
