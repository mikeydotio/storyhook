//! Artifact classification and real resource boundaries for domain operations.

use super::protect_launcher::{
    INTERPRETER_PREFIXES, PROJECT_SELECTORS, ask_hook, assert_denied_by, fixture,
    install_checkout_helpers, quoted, shell,
};
use super::*;
use storyhook_test_support::{STORY_COMMAND_DEADLINE, run_bounded};

#[test]
fn domain_operations_preserve_artifacts_through_both_installed_doors() {
    let harness = fixture();
    let cache = install_checkout_helpers(&harness);
    let hook = cache.join("hooks/protect-install.sh");
    let calls = harness.codex_log();
    harness.install_fake(
        "story",
        "#!/bin/sh\ntouch \"$HOME/unexpected-call\"\nexit 99\n",
    );
    for entry in [harness.codex_launcher(), cache.join("bin/story.sh")] {
        for interpreter in INTERPRETER_PREFIXES {
            for project in PROJECT_SELECTORS {
                for args in [
                    "reset SH-698",
                    "reset 1 --force --no-comment",
                    "reset --comment 'Recovery requested' TST-1 --force",
                    "unclaim TST-1",
                    "unclaim --no-comment 1",
                    "unclaim TST-1 --comment 'Release requested'",
                    "create --title 'New report'",
                    "create --title Report --description-file /private/tmp/report.md --type bug --priority medium",
                    "create --title Report --description 'Plain text' --labels bug,hooks",
                    "create --label hooks --title Report",
                ] {
                    let text = format!("{interpreter}{} {project}{args}", quoted(&entry));
                    for codex in [false, true] {
                        assert_eq!(
                            ask_hook(&harness, &hook, &text, codex),
                            serde_json::json!({}),
                            "{text}"
                        );
                    }
                }
            }
        }
    }
    assert_eq!(harness.codex_log(), calls);
    assert!(!harness.home.join("unexpected-call").exists());
}

#[test]
fn domain_operations_reject_ambiguous_arguments_and_identities() {
    let harness = fixture();
    let cache = install_checkout_helpers(&harness);
    let hook = cache.join("hooks/protect-install.sh");
    for entry in [harness.codex_launcher(), cache.join("bin/story.sh")] {
        for args in [
            "reset",
            "reset ../TST-1",
            "reset TST-1 TST-2",
            "reset TST-1 --unknown",
            "reset TST-1 --force --force",
            "reset TST-1 --comment",
            "reset TST-1 --comment x --no-comment",
            "reset TST-1 --comment x --comment y",
            "unclaim TST-1 --force",
            "unclaim TST-1 --no-comment --no-comment",
            "create",
            "create --title",
            "create --title ''",
            "create --title x --title y",
            "create --title x --description a --description-file /tmp/a",
            "create --title x --label a --labels b",
            "create --title x --unknown y",
            "create --title x --priority",
            "create --title x --type ../bug",
            "create --title x --priority extreme",
            "create --title x --description-file ''",
            "reset TST-1; story new x",
            "reset TST-1 | cat",
            "reset TST-1 > /tmp/x",
            "reset TST-1\ntrue",
            "reset '$(echo TST-1)'",
            "unclaim TST-1 && true",
            "create --title `hostname`",
        ] {
            assert_denied_by(&harness, &hook, &format!("bash {} {args}", quoted(&entry)));
        }
        for args in [
            format!("reset {}", quoted(&entry)),
            format!("unclaim TST-1 --comment {}", quoted(&entry)),
            format!("create --title x --description-file {}", quoted(&entry)),
            format!("--project {} reset TST-1", quoted(&entry)),
        ] {
            assert_denied_by(&harness, &hook, &format!("bash {} {args}", quoted(&entry)));
        }
    }
    let launcher = harness.codex_launcher();
    fs::write(&launcher, "exec arbitrary-program\n").unwrap();
    assert_denied_by(
        &harness,
        &hook,
        &format!("bash {} reset TST-1", quoted(&launcher)),
    );
    let helper = cache.join("bin/story.sh");
    let moved = harness.home.join("redirected-helper");
    fs::rename(&helper, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &helper).unwrap();
    assert_denied_by(
        &harness,
        &hook,
        &format!("bash {} unclaim TST-1", quoted(&helper)),
    );
}

#[test]
fn real_installed_domain_flows_preserve_artifacts_and_refuse_redirected_reset() {
    let harness = fixture();
    let cache = install_checkout_helpers(&harness);
    let artifacts = regular_files(&harness.home.join(".codex"));
    for entry in [harness.codex_launcher(), cache.join("bin/story.sh")] {
        let mut command = shell(&harness);
        command.args(["-c", r#"
source "$2/plugins/story/tests/lib.sh"
# Native resource queries use the daemon endpoint environment from startup.
export PATH="$TESTS_DIR/fakes:$PATH" STORY_AGENT=codex
story daemon stop --force >/dev/null || exit 1
repo=$(mk_story_repo)
cd "$repo" || exit 1
printf 'A report about installed files\n' > "$repo/report.md"
out=$(bash "$1" create --title 'Guard fixture' --description-file "$repo/report.md" --type bug --priority medium)
assert_eq "$(jqf "$out" .ok)" true 'real create'
id=$(jqf "$out" .id)
story claim "$id" --no-comment >/dev/null || fail_test claim
out=$(bash "$1" unclaim "$id" --no-comment)
assert_eq "$(jqf "$out" .ok)" true "real unclaim: $out"
assert_eq "$(story show "$id" --json | jq -r '.story.story.state')" todo 'unclaim persisted'
wname=$(wname_for "$repo" "$id")
git worktree add -q --no-track -b "worktree-$wname" ".claude/worktrees/$wname" HEAD || exit 1
story claim "$id" --no-comment >/dev/null || fail_test claim
manifest="$STORYHOOK_DATA_DIR/managed-paths"
cp "$manifest" "$repo/manifest.backup"
printf '%s\n' "$repo/.claude/worktrees/$wname/protected" >> "$manifest"
mkdir -p "$repo/.claude/worktrees/$wname/protected"
printf sentinel > "$repo/.claude/worktrees/$wname/protected/file"
out=$(bash "$1" reset "$id" --force)
assert_eq "$(jqf "$out" .reason)" installed-artifact-resource 'ancestor removal refused even with force'
assert_eq "$(story show "$id" --json | jq -r '.story.story.state')" in-progress 'refusal precedes release'
assert_eq "$(cat "$repo/.claude/worktrees/$wname/protected/file")" sentinel 'artifact survived'
cp "$repo/manifest.backup" "$manifest"
ln -s "$HOME/.codex" "$repo/redirect"
for container in redirect/storyhook "../$(basename "$repo")/redirect/storyhook"; do
  out=$(STORY_WORKTREE_IGNORE_PATH="$container" bash "$1" reset "$id" --force)
  assert_eq "$(jqf "$out" .reason)" installed-artifact-resource 'redirected configuration refused'
  assert_eq "$(story show "$id" --json | jq -r '.story.story.state')" in-progress 'redirect refusal precedes release'
done
out=$(bash "$1" reset "$id" --force --no-comment)
assert_eq "$(jqf "$out" .ok)" true 'real reset'
[ ! -d "$repo/.claude/worktrees/$wname" ] || fail_test 'worktree survived reset'
git show-ref --verify --quiet "refs/heads/worktree-$wname" && fail_test 'branch survived reset'
assert_eq "$(story show "$id" --json | jq -r '.story.story.state')" todo 'reset persisted'
[ "$_FAILED" -eq 0 ]
"#, "installed-domain-test"])
            .arg(entry).arg(env!("CARGO_MANIFEST_DIR"))
            .env("STORYHOOK_TEST_HOME", &harness.home);
        let output = run_bounded(
            command,
            "real installed domain operations",
            STORY_COMMAND_DEADLINE,
        );
        assert!(output.status.success(), "{}", combined(&output));
    }
    assert_eq!(regular_files(&harness.home.join(".codex")), artifacts);
}

#[test]
fn resource_guard_contract() {
    let mut command = Command::new("python3");
    command.args(["-B", "-W", "error"]).arg(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("plugins/story/tests/test-artifact-resources.py"),
    );
    let output = run_bounded(
        command,
        "artifact resource contract",
        STORY_COMMAND_DEADLINE,
    );
    assert!(output.status.success(), "{}", combined(&output));
}

#[test]
fn installed_hook_admits_literal_reports_without_changing_installations() {
    let harness = fixture();
    let cache = install_checkout_helpers(&harness);
    let hook = cache.join("hooks/protect-install.sh");
    let artifacts = regular_files(&harness.home.join(".codex"));
    let report = harness.root.join("report.md");
    let body = format!(
        "Installed launcher: {}\nHelper: {}\n",
        harness.codex_launcher().display(),
        cache.join("bin/story.sh").display()
    );
    let text = format!("cat > {} <<'REPORT'\n{body}REPORT\n", quoted(&report));
    for codex in [false, true] {
        assert_eq!(
            ask_hook(&harness, &hook, &text, codex),
            serde_json::json!({})
        );
    }
    let mut command = shell(&harness);
    command.args(["-c", &text]);
    let output = run_bounded(
        command,
        "installed-hook literal report",
        STORY_COMMAND_DEADLINE,
    );
    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(fs::read_to_string(report).unwrap(), body);
    assert_eq!(regular_files(&harness.home.join(".codex")), artifacts);
}
