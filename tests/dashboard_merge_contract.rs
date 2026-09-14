//! Structural launch regression: merged dialogs must retain unique DOM targets
//! and one implementation for each top-level handler. Browser tests exercise
//! the reset confirmation and asynchronous operation against the real server.

use std::collections::BTreeMap;

const DASHBOARD: &str = include_str!("../src/web_dashboard.html");

/// Returns duplicate names so failures identify every conflicting definition.
fn duplicates<'a>(names: impl Iterator<Item = &'a str>) -> Vec<(&'a str, usize)> {
    let mut counts = BTreeMap::new();
    for name in names {
        *counts.entry(name).or_insert(0) += 1;
    }
    counts.into_iter().filter(|(_, count)| *count > 1).collect()
}

#[test]
fn static_dashboard_ids_have_one_target() {
    let markup = DASHBOARD.split("<script>").next().unwrap();
    let names = markup.split(" id=\"").skip(1).map(|tail| {
        tail.split_once('"')
            .expect("an HTML id must have a closing quote")
            .0
    });
    assert_eq!(
        duplicates(names),
        [],
        "duplicate DOM ids bind the wrong dialog"
    );
}

#[test]
fn top_level_dashboard_handlers_have_one_definition() {
    let names = DASHBOARD.lines().filter_map(|line| {
        line.strip_prefix("  function ")
            .and_then(|rest| rest.split_once('('))
            .map(|(name, _)| name)
    });
    assert_eq!(
        duplicates(names),
        [],
        "duplicate handlers silently shadow behavior"
    );
}
