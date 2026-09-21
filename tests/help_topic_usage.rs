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

/// Keep all continuation operations and both receiving providers testable.
#[test]
fn continuation_usage_covers_every_operation_and_provider() {
    let body = get_help_topic("continuation").expect("continuation help topic");
    let mut actual = Vec::new();
    for entry in usage_entries("continuation", body) {
        let invocation = entry.raw.strip_prefix("story ").expect("command prefix");
        let DocumentedInvocation::Argvs(argvs) =
            expand_documented_invocation(invocation).expect("valid continuation usage grammar")
        else {
            panic!("continuation commands must be checked");
        };
        for argv in argvs {
            parse_documented_argv(&argv).expect("continuation usage must parse");
            actual.push(argv.join(" "));
        }
    }
    let request = "a2cb702b-12e8-46c4-831b-c78bf57e944b";
    let head = "0123456789abcdef0123456789abcdef01234567";
    let mut expected = vec![
        "continuation capabilities --json".to_string(),
        "continuation request SH-1 --stdin --json".to_string(),
        "continuation status SH-1 --json".to_string(),
        format!("continuation receipt SH-1 {request} --stdin --json"),
        format!("continuation retry SH-1 {request} --json"),
    ];
    for provider in ["codex", "claude"] {
        expected.push(format!(
            "continuation ack SH-1 {request} --reviewed-seq 1 --head {head} --provider {provider} --session-id receiving-root-session --json"
        ));
    }
    actual.sort();
    expected.sort();
    assert_eq!(
        actual, expected,
        "every continuation command must remain documented"
    );
}

/// A parsing-only corpus could pass after deleting the offending command.
/// Keep every verifier control, including both acknowledgement intents, visible.
#[test]
fn verifier_usage_covers_every_control() {
    let body = get_help_topic("verifier").expect("verifier help topic");
    let mut actual = Vec::new();
    for entry in usage_entries("verifier", body) {
        let invocation = entry.raw.strip_prefix("story ").expect("command prefix");
        match expand_documented_invocation(invocation).expect("valid verifier usage grammar") {
            DocumentedInvocation::Argvs(argvs) => actual.extend(argvs),
            DocumentedInvocation::ParsedElsewhere => panic!("verifier controls must be checked"),
        }
    }
    actual.sort();
    let mut expected: Vec<Vec<String>> = [
        "verifier status",
        "verifier start",
        "verifier stop",
        "verifier drain",
        "verifier ack 2:28821",
        "verifier ack 2:28821 --leave-stopped",
        "verifier gate-config /tmp/project 0123456789abcdef0123456789abcdef01234567 0123456789abcdef0123456789abcdef01234567 0123456789abcdef0123456789abcdef01234567 --json",
        "verifier repair show recovery-1 --json",
        "verifier repair decide recovery-1 --input decision.json",
    ]
    .into_iter()
    .map(|command| command.split_whitespace().map(str::to_owned).collect())
    .collect();
    expected.sort();
    assert_eq!(
        actual, expected,
        "every verifier control must remain documented"
    );
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
