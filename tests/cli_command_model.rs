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

#[test]
fn every_registered_path_has_a_real_parser_witness_or_explicit_early_boundary() {
    let mut paths = BTreeSet::new();
    for path in model::paths() {
        assert!(
            paths.insert(path.words.clone()),
            "duplicate path {:?}",
            path.words
        );
        let found = model::path(&path.words).unwrap();
        assert_eq!(found.grammar.syntax, path.grammar.syntax);
        model::grammar::expression(path.grammar.syntax)
            .unwrap_or_else(|error| panic!("{:?}: {error}", path.words));
        let result = parse_invocation(&path.example());
        match path.grammar.kind {
            model::FormKind::Command => assert!(result.is_ok(), "{:?}: {result:?}", path.example()),
            model::FormKind::Retired => assert!(result.is_err(), "retired {:?}", path.words),
            model::FormKind::Group => {
                assert!(
                    model::paths()
                        .iter()
                        .any(|child| child.words.len() > path.words.len()
                            && child.words.starts_with(&path.words)),
                    "empty group {:?}",
                    path.words
                );
                assert!(result.is_err(), "unexpected group default {:?}", path.words);
            }
            model::FormKind::Early => assert!(path.command.handler() != FamilyHandler::Parsed),
        }
        for alias in path.aliases() {
            assert_eq!(model::path(&alias).unwrap().words, path.words);
        }
    }
    assert!(model::path(&["project", "link", "typo"]).is_none());
    assert!(model::path(&["show", "SH-1"]).is_none());
}

#[test]
fn local_invocation_handlers_are_selected_without_execution() {
    let parsed = |args: &[&str]| parse_invocation(&argv(args)).unwrap();
    assert!(matches!(
        model::before_environment(&parsed(&["daemon", "logs", "--follow"])),
        Some(model::BeforeEnvironment::Logs {
            follow: true,
            directory: None
        })
    ));
    assert!(matches!(
        model::before_environment(&parsed(&["daemon", "--serve", "--port", "0"])),
        Some(model::BeforeEnvironment::Serve {
            port: Some(0),
            owner: None
        })
    ));
    assert!(matches!(
        model::before_environment(&parsed(&["web", "--serve"])),
        Some(model::BeforeEnvironment::Serve {
            port: None,
            owner: None
        })
    ));
    assert!(matches!(
        model::before_store(&parsed(&["plugin", "run", "codex", "--", "context"])),
        Some(model::BeforeStore::Plugin {
            target: "codex",
            ..
        })
    ));
    assert!(matches!(
        model::before_store(&parsed(&["store", "new", "example.db"])),
        Some(model::BeforeStore::StoreNew { path: "example.db" })
    ));
    assert!(model::needs_questionnaire(&parsed(&["project", "new"])));
    assert!(!model::needs_questionnaire(&parsed(&[
        "project", "new", "--prefix", "EX"
    ])));
    assert!(model::before_environment(&parsed(&["show", "SH-1"])).is_none());
    assert!(model::before_store(&parsed(&["show", "SH-1"])).is_none());
}

#[test]
fn structured_syntax_exposes_multiplicity_alternatives_and_dynamic_sources() {
    use model::grammar::{Domain, Expression as E};
    let expression =
        model::grammar::expression("<id:stories> [--on <blocker:stories>]...").unwrap();
    let E::Sequence(items) = expression else {
        panic!("expected sequence")
    };
    assert!(matches!(
        &items[0],
        E::Operand {
            domain: Some(Domain::Dynamic {
                source: "story list --all",
                ..
            }),
            ..
        }
    ));
    assert!(matches!(&items[1], E::Repeated(inner) if matches!(**inner, E::Optional(_))));
    assert!(model::grammar::expression("[<id>").is_err());
    assert!(model::grammar::expression("<id:invented>").is_err());
    let E::Sequence(items) = model::grammar::expression("(one | two)").unwrap() else {
        panic!()
    };
    assert!(matches!(items[0], E::Choice(_)));
    for value in ["low", "medium", "high"] {
        assert!(parse_invocation(&argv(&["new", "example", "--complexity", value])).is_ok());
    }
    // Story field values are validated by the service's domain layer; the
    // compatible parser must continue to carry them unchanged to that layer.
    assert!(parse_invocation(&argv(&["new", "example", "--complexity", "invented"])).is_ok());
    assert!(storyhook::domain::Complexity::parse("invented").is_err());
}

#[test]
fn option_metadata_covers_the_validation_gate_without_inventing_leaf_options() {
    for flags in model::FLAG_PATHS {
        if matches!(flags.command, CommandId::Purge)
            || matches!(flags.subcommand, Some("init" | "deinit"))
        {
            continue;
        }
        let applicable: Vec<_> = model::paths()
            .into_iter()
            .filter(|path| {
                path.command == flags.command
                    && flags
                        .subcommand
                        .is_none_or(|sub| path.words.get(1) == Some(&sub))
            })
            .collect();
        for flag in flags.flags {
            let spelling = format!("--{}", flag.name);
            assert!(
                applicable
                    .iter()
                    .any(|path| path.grammar.syntax.contains(&spelling)
                        || path.words.contains(&spelling.as_str())),
                "missing grammar for {:?} {:?} {spelling}",
                flags.command,
                flags.subcommand
            );
        }
    }
    for (words, forbidden, example) in [
        (
            vec!["daemon", "status"],
            "--force",
            vec!["daemon", "status", "--force"],
        ),
        (
            vec!["attachment", "list"],
            "--name",
            vec!["attachment", "list", "SH-1", "--name", "example"],
        ),
        (
            vec!["project", "unlink", "checkout"],
            "--dry-run",
            vec!["project", "unlink", "checkout", "--dry-run"],
        ),
        (
            vec!["verifier", "status"],
            "--input",
            vec!["verifier", "status", "--input", "file"],
        ),
    ] {
        assert!(
            !model::path(&words)
                .unwrap()
                .grammar
                .syntax
                .contains(forbidden)
        );
        assert!(parse_invocation(&argv(&example)).is_err(), "{example:?}");
    }
}

#[test]
fn static_domains_and_help_only_aliases_use_real_runtime_definitions() {
    use model::grammar::{Domain, domain};
    let Domain::Static { values, .. } = domain("relationships").unwrap() else {
        panic!()
    };
    for value in values {
        assert!(storyhook::domain::is_relation_input(&value), "{value}");
    }
    let Domain::Static { values, .. } = domain("complexity").unwrap() else {
        panic!()
    };
    for value in values {
        assert!(storyhook::domain::Complexity::parse(&value).is_ok());
    }
    for alias in model::HELP_ALIASES {
        assert_eq!(
            storyhook::help_topics::get_help_topic(alias.name),
            storyhook::help_topics::get_help_topic(alias.command.help_topic())
        );
        assert!(model::path(&[alias.name]).is_none());
        assert!(parse_invocation(&argv(&[alias.name])).is_err());
    }
}
