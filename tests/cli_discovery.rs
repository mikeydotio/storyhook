//! SH-900 offline discovery and truthful capabilities, using only isolated fixtures.
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;
use storyhook::cli::{
    discovery::{self, Access, Audience, OutputClass},
    model, parse_invocation,
};
use storyhook_test_support::TestEnv;

fn argv(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).into()).collect()
}
fn describe(values: &[&str]) -> discovery::Document {
    discovery::describe(&argv(values)).unwrap()
}
fn entry(words: &[&str]) -> discovery::Descriptor {
    let mut args = words.to_vec();
    args.extend(["--audience", "all"]);
    describe(&args)
        .commands
        .into_iter()
        .find(|d| d.path == words)
        .unwrap()
}
fn tree(root: &Path) -> BTreeSet<std::path::PathBuf> {
    let mut result = BTreeSet::new();
    fn walk(path: &Path, out: &mut BTreeSet<std::path::PathBuf>) {
        for item in std::fs::read_dir(path).unwrap() {
            let item = item.unwrap();
            out.insert(item.path());
            if item.file_type().unwrap().is_dir() {
                walk(&item.path(), out);
            }
        }
    }
    walk(root, &mut result);
    result
}

#[test]
fn binary_discovery_works_without_project_store_helpers_or_daemon() {
    let env = TestEnv::isolated();
    let absent = env.home().join("nonexistent-parent/store.db");
    let mut command = env.raw_story(env.home());
    command
        .args([
            "--json",
            "--project",
            "SH900_PRIVATE_SENTINEL",
            "--store-path",
        ])
        .arg(&absent)
        .args(["describe", "github", "--audience", "all"])
        .env("PATH", env.home().join("no-executables"));
    let root = env.home().parent().unwrap();
    let before = tree(root);
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["schema_version"], 1);
    assert!(
        document["commands"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["path"] == serde_json::json!(["github", "merge"]))
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("SH900_PRIVATE_SENTINEL"));
    assert_eq!(before, tree(root));
    assert!(!absent.exists());
    assert!(!env.store_path().exists());
    assert!(!env.daemon_is_live());
}

#[test]
fn unknown_paths_and_audiences_fail_with_exit_two_before_runtime() {
    let env = TestEnv::isolated();
    for args in [
        vec!["describe", "not-a-command", "--json"],
        vec!["describe", "project", "link", "typo", "--json"],
        vec!["describe", "--audience", "typo", "--json"],
        vec![
            "describe",
            "--audience",
            "all",
            "--audience",
            "task",
            "--json",
        ],
        vec!["describe", "--audience"],
    ] {
        let output = env.raw_story(env.home()).args(&args).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        if args.contains(&"--json") {
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value["exit_code"], 2);
            assert!(output.stderr.is_empty());
        } else {
            assert!(output.stdout.is_empty());
            assert!(String::from_utf8_lossy(&output.stderr).starts_with("error:"));
        }
        assert!(!env.store_path().exists());
        assert!(!env.daemon_is_live());
    }
}

#[test]
fn audiences_partition_every_registered_path_and_filter_subcommands() {
    let all = describe(&["--audience", "all"]);
    let expected: BTreeSet<_> = model::paths().into_iter().map(|p| p.words).collect();
    assert_eq!(
        all.commands
            .iter()
            .map(|d| d.path.clone())
            .collect::<BTreeSet<_>>(),
        expected
    );
    let mut partition = BTreeSet::new();
    for name in ["task", "operator", "internal"] {
        for d in describe(&["--audience", name]).commands {
            assert!(partition.insert(d.path));
        }
    }
    assert_eq!(partition, expected);
    assert_eq!(entry(&["verifier", "status"]).audience, Audience::Operator);
    assert_eq!(
        entry(&["verifier", "repair-admit"]).audience,
        Audience::Internal
    );
    assert_eq!(entry(&["plugin", "run"]).audience, Audience::Internal);
    assert_eq!(entry(&["daemon", "--serve"]).audience, Audience::Internal);
    assert!(
        describe(&[])
            .commands
            .iter()
            .all(|d| d.audience == Audience::Task)
    );
    assert!(describe(&["verifier", "repair-admit"]).commands.is_empty());
}

#[test]
fn aliases_resolve_to_canonical_paths_and_help_aliases_are_not_commands() {
    assert_eq!(describe(&["context"]).requested_path, vec!["load-context"]);
    assert_eq!(describe(&["link"]).requested_path, vec!["relate"]);
    assert!(entry(&["relate"]).aliases.contains(&vec!["link"]));
    assert!(entry(&["project", "link"]).aliases.is_empty());
    for alias in ["states", "is", "awaits", "priority"] {
        assert!(discovery::describe(&argv(&[alias])).is_err());
    }
    assert_eq!(entry(&["move"]).help_only_aliases, vec!["is"]);
}

#[test]
fn all_output_classes_and_their_exception_contracts_are_explicit() {
    let classes = describe(&["--audience", "all"])
        .commands
        .into_iter()
        .flat_map(|d| d.output)
        .map(|o| format!("{:?}", o.class))
        .collect::<BTreeSet<_>>();
    assert_eq!(classes.len(), 5);
    assert_eq!(entry(&["export"]).output[0].class, OutputClass::RawDocument);
    assert!(entry(&["export"]).output[0].quiet.contains("Ignored"));
    assert_eq!(
        entry(&["daemon", "logs"]).output[0].class,
        OutputClass::JsonLines
    );
    assert!(entry(&["daemon", "logs"]).output[0].follow);
    assert_eq!(
        entry(&["plugin", "run"]).output[0].class,
        OutputClass::DelegatedHelper
    );
    assert!(
        entry(&["github", "merge"]).output[0]
            .exit_status
            .contains("mapped AppError")
    );
    assert_eq!(entry(&["tui"]).output[0].class, OutputClass::Terminal);
    assert!(
        entry(&["next"]).output[0]
            .schema
            .contains("even if only one")
    );
}

#[test]
fn preview_and_confirmation_claims_agree_with_actual_invocations() {
    for path in model::paths() {
        if path.grammar.kind != model::FormKind::Command {
            continue;
        }
        let descriptor = entry(&path.words);
        let original = parse_invocation(&path.example()).unwrap();
        assert_eq!(
            descriptor.capabilities.confirmation.is_some(),
            original.clone().forced() != original,
            "{:?}",
            path.words
        );
        if descriptor.capabilities.dry_run.is_some() {
            let mut args = path.example();
            args.push("--dry-run".into());
            assert!(parse_invocation(&args).is_ok(), "{args:?}");
        }
    }
    for words in [vec!["show"], vec!["delete"], vec!["daemon", "status"]] {
        assert!(entry(&words).capabilities.dry_run.is_none());
    }
    assert!(
        entry(&["move"])
            .capabilities
            .guarded_write
            .unwrap()
            .contains("--if-state")
    );
    assert!(entry(&["set"]).capabilities.guarded_write.is_none());
    assert!(entry(&["reset"]).capabilities.confirmation.is_none());
}

#[test]
fn read_store_access_is_distinct_from_daemon_start_and_offline_operations() {
    let show = entry(&["show"]).capabilities.effects;
    assert_eq!(show.store, Access::Read);
    assert!(show.may_start_daemon);
    for words in [
        vec!["describe"],
        vec!["help"],
        vec!["daemon", "logs"],
        vec!["github", "resolve"],
        vec!["web", "address"],
    ] {
        assert!(
            !entry(&words).capabilities.effects.may_start_daemon,
            "{words:?}"
        );
    }
    assert_eq!(
        entry(&["describe"]).capabilities.effects.filesystem,
        Access::None
    );
    assert_eq!(
        entry(&["doctor", "abandoned", "clear"])
            .capabilities
            .effects
            .store,
        Access::None
    );
    assert_eq!(
        entry(&["doctor", "abandoned", "clear"])
            .capabilities
            .effects
            .filesystem,
        Access::Write
    );
}

#[test]
fn explicit_json_input_is_discovered_and_survives_dependency_integration() {
    let set = entry(&["set"]);
    assert!(set.syntax.contains("--input-json"));
    assert!(set.syntax.contains("--json"));
    let (flags, rest) = storyhook::cli::split_global_flags(&argv(&[
        "--json",
        "set",
        "SH-1",
        "--input-json",
        r#"{"title":"example"}"#,
    ]))
    .unwrap();
    assert!(flags.json);
    assert!(matches!(
        parse_invocation(&rest).unwrap(),
        storyhook::cli::Invocation::SetFields { json: Some(_), .. }
    ));
    assert!(storyhook::cli::HELP_TEXT.contains("--input-json"));
    assert_eq!(
        storyhook::cli::HELP_TEXT,
        include_str!("fixtures/cli-model-help.txt")
    );
}

#[test]
fn lifecycle_effects_are_distinct_and_timeout_guidance_does_not_promise_retry() {
    let mut details = BTreeSet::new();
    for words in [
        vec!["close"],
        vec!["archive"],
        vec!["delete"],
        vec!["reset"],
        vec!["unclaim"],
    ] {
        let d = entry(&words);
        assert!(details.insert(d.capabilities.effects.detail));
        assert!(
            d.capabilities
                .uncertain_outcome
                .contains("never blindly replay")
        );
    }
}

#[test]
fn discovery_can_describe_help_flags_without_changing_legacy_help_precedence() {
    let env = TestEnv::isolated();
    let output = env
        .raw_story(env.home())
        .args(["describe", "--help", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["requested_path"], serde_json::json!(["-h"]));
    assert!(
        matches!(parse_invocation(&argv(&["new","--","--help"])).unwrap(),storyhook::cli::Invocation::HelpTopic{topic} if topic=="new")
    );
}

#[test]
fn discovery_output_is_deterministic_versioned_and_raw_even_when_quiet() {
    let env = TestEnv::isolated();
    let run = |args: &[&str]| {
        let output = env.raw_story(env.home()).args(args).output().unwrap();
        assert!(output.status.success());
        output.stdout
    };
    let one = run(&["describe", "set", "--json"]);
    assert_eq!(one, run(&["describe", "set", "--json", "--quiet"]));
    let value: Value = serde_json::from_slice(&one).unwrap();
    assert_eq!(value["schema"], "storyhook.command-discovery");
    assert_eq!(value["cli_contract"], "legacy-compatible");
    assert_eq!(value["visibility_is_authorization"], false);
}
