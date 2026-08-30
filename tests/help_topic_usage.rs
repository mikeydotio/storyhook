//! Every invocation in a shipped help topic's opening usage block must be one
//! the CLI parser accepts (SH-489).
//!
//! These topics are compiled into the binary and are what a reader sees from
//! `story help <verb>`. README coverage cannot protect them: a removed flag in
//! `src/help_topics.rs` used to remain green until somebody tried it by hand.
//!
//! A topic's usage block is its first paragraph. Each line beginning `story `
//! starts an invocation; an indented line continues the preceding invocation.
//! Topics whose first paragraph is prose or a conceptual heading have no
//! invocation to check. Expansion and parsing are shared with
//! `readme_command_reference` so the two documentation surfaces cannot drift
//! into different test grammars.

use storyhook::help_topics::{get_help_topic, list_topics};
use storyhook_test_support::command_reference::{
    DocumentedInvocation, expand_documented_invocation, parse_documented_argv,
};

struct Entry<'a> {
    topic: &'a str,
    raw: String,
}

fn usage_entries<'a>(topic: &'a str, body: &str) -> Vec<Entry<'a>> {
    let mut entries: Vec<Entry<'a>> = Vec::new();

    for line in body.lines() {
        if line.trim().is_empty() {
            break;
        }
        if line.starts_with("story ") {
            entries.push(Entry {
                topic,
                raw: line.to_string(),
            });
        } else if line.starts_with(char::is_whitespace) {
            if let Some(entry) = entries.last_mut() {
                entry.raw.push(' ');
                entry.raw.push_str(line.trim());
            }
        } else {
            break;
        }
    }

    entries
}

#[test]
fn every_help_topic_usage_invocation_parses() {
    let topics = list_topics();
    assert!(
        topics.len() > 50,
        "found only {} help topics — the corpus may no longer be complete",
        topics.len()
    );

    let entries: Vec<Entry<'_>> = topics
        .iter()
        .flat_map(|topic| {
            usage_entries(
                topic,
                get_help_topic(topic).expect("list_topics returned a missing topic"),
            )
        })
        .collect();
    assert!(
        entries.len() > 60,
        "found only {} opening usage invocations across {} topics — the extractor may have \
         broken or the reference shrank without this bound being revisited",
        entries.len(),
        topics.len()
    );

    let mut checked = 0usize;
    let mut failures = Vec::new();

    for entry in &entries {
        let without_story = entry
            .raw
            .strip_prefix("story")
            .expect("usage entries begin with story")
            .trim_start();
        let expanded = match expand_documented_invocation(without_story) {
            Ok(expanded) => expanded,
            Err(reason) => {
                failures.push(format!(
                    "topic `{}`: `{}` — {reason}",
                    entry.topic, entry.raw
                ));
                continue;
            }
        };
        let argvs = match expanded {
            DocumentedInvocation::ParsedElsewhere => continue,
            DocumentedInvocation::Argvs(argvs) => argvs,
        };

        for argv in argvs {
            checked += 1;
            if let Err(reason) = parse_documented_argv(&argv) {
                failures.push(format!(
                    "topic `{}`: `{}` (as `story {}`) — {reason}",
                    entry.topic,
                    entry.raw,
                    argv.join(" ")
                ));
            }
        }
    }

    assert!(
        checked > 100,
        "checked only {checked} expanded argv variants across {} entries — the expansion or \
         placeholder table may have broken",
        entries.len()
    );
    assert!(
        failures.is_empty(),
        "these help-topic usage invocations do not parse:\n{}",
        failures.join("\n")
    );
}
