//! SH-898 registration, dispatch, grammar and legacy-help parity.
use std::collections::BTreeSet;

use storyhook::cli::model::{self, CommandId, FamilyHandler};
use storyhook::cli::{Invocation, parse_invocation};

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn registered_spellings_resolve_uniquely_to_real_parser_bindings() {
    let mut names = BTreeSet::new();
    for command in CommandId::ALL {
        for name in command.names() {
            assert!(names.insert(*name), "duplicate registration: {name}");
            assert_eq!(CommandId::find(name), Some(*command));
            let result = parse_invocation(&argv(&[name]));
            if command.handler() == FamilyHandler::Parsed {
                assert!(
                    !matches!(result, Err(ref error) if error.to_string().contains("unknown command")),
                    "registered parser missing: {name}"
                );
            }
        }
    }
    for help_only in ["states", "is", "awaits", "priority"] {
        assert_eq!(CommandId::find(help_only), None);
        assert!(parse_invocation(&argv(&[help_only])).is_err());
        assert!(storyhook::help_topics::get_help_topic(help_only).is_some());
    }
}

#[test]
fn runnable_aliases_share_the_registered_parser_and_help_topic() {
    for command in CommandId::ALL {
        let canonical = command.names()[0];
        for alias in &command.names()[1..] {
            for tail in [vec![], vec!["--help"], vec!["SH-1", "blocks", "SH-2"]] {
                let make = |name| argv(&[vec![name], tail.clone()].concat());
                let render =
                    |args: Vec<String>| parse_invocation(&args).map_err(|error| error.to_string());
                assert_eq!(
                    render(make(canonical)),
                    render(make(alias)),
                    "alias {alias}"
                );
            }
        }
    }
}

#[test]
fn early_handlers_use_the_registration_at_the_original_boundaries() {
    assert!(matches!(
        model::before_globals(&argv(&["github", "exec", "--json"])),
        Some(model::BeforeGlobals::Github)
    ));
    assert!(model::before_globals(&argv(&["--json", "github", "exec"])).is_none());
    assert!(matches!(
        model::before_invocation(&argv(&["tui"])),
        Some(model::BeforeInvocation::Tui)
    ));
    assert!(model::before_invocation(&argv(&["tui", "--", "--help"])).is_none());
    assert!(matches!(
        parse_invocation(&argv(&["tui", "--help"])).unwrap(),
        Invocation::Help | Invocation::HelpTopic { .. }
    ));
    assert!(parse_invocation(&argv(&["github", "--help"])).is_err());
}

#[test]
fn moved_help_syntax_preserves_the_complete_legacy_help_bytes() {
    assert_eq!(
        storyhook::cli::HELP_TEXT,
        include_str!("fixtures/cli-model-help.txt")
    );
    for syntax in model::HELP_SYNTAX {
        let name = syntax
            .text
            .trim_start()
            .strip_prefix("story ")
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        assert_eq!(CommandId::find(name), Some(syntax.command));
    }
}

#[test]
fn typed_flag_paths_still_reject_unknown_options_before_data_parsing() {
    for entry in model::FLAG_PATHS {
        let mut args = vec![entry.command.names()[0].to_string()];
        if let Some(subcommand) = entry.subcommand {
            args.push(subcommand.to_string());
        }
        args.push("--sh898-unknown".to_string());
        let error = parse_invocation(&args).unwrap_err().to_string();
        assert!(
            error.contains("unknown flag `--sh898-unknown`"),
            "{args:?}: {error}"
        );
    }
    let text = parse_invocation(&argv(&["new", "--", "--sh898-unknown"])).unwrap();
    assert!(matches!(text, Invocation::New { title, .. } if title == "--sh898-unknown"));
}

#[test]
fn subcommand_flags_retain_their_scope_and_arity() {
    for args in [
        vec!["state", "set", "review", "--no-description"],
        vec!["daemon", "install", "--this-binary"],
        vec![
            "dispatch-policy",
            "reset",
            "--agent",
            "codex",
            "--complexity",
            "low",
            "--model",
        ],
        vec![
            "new",
            "dependent",
            "--blocked-by",
            "SH-1",
            "--blocked-by",
            "SH-2",
        ],
    ] {
        assert!(parse_invocation(&argv(&args)).is_ok(), "{args:?}");
    }
    for args in [
        vec!["state", "add", "review", "--no-description"],
        vec!["daemon", "status", "--this-binary"],
        vec![
            "dispatch-policy",
            "set",
            "--agent",
            "codex",
            "--complexity",
            "low",
            "--model",
        ],
    ] {
        assert!(parse_invocation(&argv(&args)).is_err(), "{args:?}");
    }
}

#[test]
fn legacy_help_terminator_and_unknown_command_precedence_are_retained() {
    assert!(
        matches!(parse_invocation(&argv(&["new", "--", "--help"])).unwrap(),
                     Invocation::HelpTopic { topic } if topic == "new")
    );
    let error = parse_invocation(&argv(&["not-a-command", "--help"]))
        .unwrap_err()
        .to_string();
    assert!(error.contains("unknown command `not-a-command`"));
    let value =
        parse_invocation(&argv(&["comment", "SH-1", "--", "--json", "is", "literal"])).unwrap();
    assert!(matches!(value, Invocation::Comment { text, .. } if text == "--json is literal"));
}

#[test]
fn registered_reset_and_recovery_protocols_keep_distinct_invocations() {
    assert!(matches!(
        parse_invocation(&argv(&["reset", "SH-1", "--dry-run"])).unwrap(),
        Invocation::ResetPreview { .. }
    ));
    assert!(matches!(
        parse_invocation(&argv(&["internal", "supersede-continuations", "SH-1"])).unwrap(),
        Invocation::SupersedeContinuations { .. }
    ));
    assert!(matches!(
        parse_invocation(&argv(&["verifier", "repair", "show", "recovery-id"])).unwrap(),
        Invocation::Verifier { .. }
    ));
}
