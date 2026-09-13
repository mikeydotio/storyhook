//! The helper door: an installed plugin's own `bin/story.sh`, admitted by the
//! hook loaded from the same plugin root (SH-632).
//!
//! The Codex route reaches the helper through a byte-verified launcher because
//! Codex's command rules match exact argv prefixes. Every other host runs the
//! helper directly — `references/helper-command.md` names it as
//! `<plugin-root>/bin/story.sh`, and `<plugin-root>` is always under a managed
//! prefix — so the hook has to recognise it by identity rather than by bytes.
//! The identity is where the hook itself was loaded from: a host runs one copy
//! of a plugin's hooks per session (a `--plugin-dir` plugin overrides the
//! installed one), so the skill's `<plugin-root>` and the hook's own root are
//! the same directory. `hooks/session-start.sh` derives the same fact for the
//! dispatch sentinel.
//!
//! So the hook under test here is deliberately NOT the tracked one: it is an
//! installed copy, written from the tracked tree by the same fixture, because
//! the door's whole claim is about the hook's own location.

use super::protect_launcher::{
    ADMITTED_DISPATCH_ARGS, ADMITTED_READER_ARGS, ADMITTED_TERMINAL_ARGS, INTERPRETER_PREFIXES,
    PROJECT_SELECTORS, REJECTED_ARGS, REJECTED_DISPATCH_ARGS, ask_hook, assert_denied_by, fixture,
    install_checkout_helpers_at, quoted, rejected_compositions, rejected_dispatch_compositions,
    shell, tracked_hook,
};
use super::*;
use storyhook_test_support::{STORY_COMMAND_DEADLINE, run_bounded};

/// Every plugin root a session can be loaded from on a machine where
/// `story plugin install` has run, each under a different managed prefix: the
/// user-scope Claude cache; the release projection, which `--plugin-dir` can
/// name directly; and the Codex cache, which is what dispatch passes to
/// `--plugin-dir` — the shape of the session that filed SH-632.
fn plugin_roots(harness: &Harness) -> Vec<PathBuf> {
    let version = env!("CARGO_PKG_VERSION");
    vec![
        harness
            .home
            .join(".claude/plugins/cache/storyhook/story")
            .join(version),
        harness.release_marketplace().join("plugins/story"),
        harness
            .home
            .join(".codex/plugins/cache/storyhook/story")
            .join(version),
    ]
}

/// The user-scope Claude cache root — `installed_plugins.json`'s `installPath`.
fn claude_root(harness: &Harness) -> PathBuf {
    plugin_roots(harness).swap_remove(0)
}

fn hook_of(root: &Path) -> PathBuf {
    root.join("hooks/protect-install.sh")
}

fn helper_of(root: &Path) -> PathBuf {
    root.join("bin/story.sh")
}

fn tattling_story(harness: &Harness) {
    harness.install_fake(
        "story",
        "#!/bin/sh\nprintf invoked > \"$HOME/unexpected-story-call\"\nexit 99\n",
    );
}

#[test]
fn installed_helper_is_admitted_by_the_hook_of_its_own_plugin() {
    let harness = fixture();
    let provider_calls = harness.codex_log();
    tattling_story(&harness);
    // The whole grammar from the user-scope root; every other root proves only
    // that the door is prefix-agnostic, one command per verb family, since the
    // grammar is shared code and each hook decision is a python start.
    for (index, root) in plugin_roots(&harness).into_iter().enumerate() {
        install_checkout_helpers_at(&root);
        let hook = hook_of(&root);
        let helper = quoted(&helper_of(&root));
        let (prefixes, selectors, args): (&[&str], &[&str], Vec<&str>) = if index == 0 {
            (
                &INTERPRETER_PREFIXES,
                &PROJECT_SELECTORS,
                ADMITTED_READER_ARGS
                    .iter()
                    .chain(&ADMITTED_TERMINAL_ARGS)
                    .chain(&ADMITTED_DISPATCH_ARGS)
                    .copied()
                    .collect(),
            )
        } else {
            (
                &INTERPRETER_PREFIXES[1..2],
                &PROJECT_SELECTORS[..1],
                vec![
                    "context",
                    "context --story TST-1 --full",
                    "view TST-1",
                    "capture TST-1",
                    "doctor",
                    "dispatch TST-1 --agent=claude",
                ],
            )
        };
        for prefix in prefixes {
            for selector in selectors {
                for args in &args {
                    let text = format!("{prefix}{helper} {selector}{args}");
                    for codex in [false, true] {
                        assert_eq!(
                            ask_hook(&harness, &hook, &text, codex),
                            serde_json::json!({}),
                            "{text} (hook at {})",
                            hook.display()
                        );
                    }
                }
            }
        }
    }
    assert_eq!(
        harness.codex_log(),
        provider_calls,
        "classification must not run the helper or resolve a provider"
    );
    assert!(!harness.home.join("unexpected-story-call").exists());
}

#[test]
fn installed_helper_is_refused_by_any_other_plugins_hook() {
    let harness = fixture();
    let roots = plugin_roots(&harness);
    for root in &roots {
        install_checkout_helpers_at(root);
    }
    let claude = &roots[0];
    let text = format!(
        "bash {} context --full --story TST-1",
        quoted(&helper_of(claude))
    );
    // The load-bearing case: the tracked hook is a plugin too — the checkout's —
    // and the installed helper is not its own. Identical bytes do not make it so.
    assert_denied_by(&harness, &tracked_hook(), &text);
    for other in &roots[1..] {
        assert_denied_by(&harness, &hook_of(other), &text);
    }
    // A retained older projection beside the loaded one, byte-identical.
    let stale = claude.parent().unwrap().join("0.0.1");
    install_checkout_helpers_at(&stale);
    assert_denied_by(
        &harness,
        &hook_of(claude),
        &format!("bash {} context --story TST-1", quoted(&helper_of(&stale))),
    );
    assert_denied_by(&harness, &hook_of(&stale), &text);
}

#[test]
fn helper_door_preserves_argument_shell_and_identity_guards() {
    let harness = fixture();
    let root = claude_root(&harness);
    install_checkout_helpers_at(&root);
    let hook = hook_of(&root);
    let helper = helper_of(&root);
    let quoted_helper = quoted(&helper);
    for args in REJECTED_ARGS.iter().chain(&REJECTED_DISPATCH_ARGS) {
        assert_denied_by(&harness, &hook, &format!("bash {quoted_helper} {args}"));
    }
    for text in rejected_compositions(&helper)
        .into_iter()
        .chain(rejected_dispatch_compositions(&helper))
    {
        assert_denied_by(&harness, &hook, &text);
    }
    // Other files of the same plugin, and a traversal that lands on the helper.
    for text in [
        format!("bash {} context", quoted(&hook)),
        format!("bash {} context", quoted(&root.join("lib/session.sh"))),
        format!(
            "bash {} dispatch TST-1",
            quoted(&root.join("bin/../bin/story.sh"))
        ),
        format!(
            "bash {} dispatch TST-1",
            quoted(
                &root
                    .parent()
                    .unwrap()
                    .join("../story")
                    .join(root.file_name().unwrap())
                    .join("bin/story.sh")
            )
        ),
        format!("STORY_AGENT=claude bash {quoted_helper} dispatch TST-1"),
        format!("bash {quoted_helper} dispatch TST-1 --agent={quoted_helper}"),
    ] {
        assert_denied_by(&harness, &hook, &text);
    }
}

#[test]
fn helper_identity_requires_a_regular_file_at_its_own_unredirected_path() {
    let harness = fixture();
    let root = claude_root(&harness);
    install_checkout_helpers_at(&root);
    let hook = hook_of(&root);
    let helper = helper_of(&root);
    let text = format!("bash {} context --story TST-1 --full", quoted(&helper));
    let original = fs::read(&helper).unwrap();
    assert_eq!(
        ask_hook(&harness, &hook, &text, true),
        serde_json::json!({}),
        "{text}"
    );

    // A symlink at the helper's path, even to the identical bytes elsewhere.
    fs::remove_file(&helper).unwrap();
    let other = harness.home.join("other.sh");
    fs::write(&other, &original).unwrap();
    std::os::unix::fs::symlink(&other, &helper).unwrap();
    assert_denied_by(&harness, &hook, &text);
    // A missing helper.
    fs::remove_file(&helper).unwrap();
    assert_denied_by(&harness, &hook, &text);
    // A FIFO: classification must never block on a writer.
    let mut fifo = shell(&harness);
    fifo.arg("-c").arg(format!("mkfifo {}", quoted(&helper)));
    let output = run_bounded(fifo, "create isolated FIFO", STORY_COMMAND_DEADLINE);
    assert!(output.status.success(), "{}", combined(&output));
    assert_denied_by(&harness, &hook, &text);
    fs::remove_file(&helper).unwrap();
    fs::write(&helper, &original).unwrap();
    assert_eq!(
        ask_hook(&harness, &hook, &text, true),
        serde_json::json!({}),
        "restored"
    );

    // A redirected managed directory: the spelled path and the hook's own root
    // now both resolve through a symlink below HOME, which the launcher door
    // already refuses as a different installed identity.
    let managed = harness.home.join(".claude/plugins/cache/storyhook");
    let moved = harness.home.join("redirected-managed-directory");
    fs::rename(&managed, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &managed).unwrap();
    assert_denied_by(&harness, &hook, &text);
}

#[test]
fn admitted_claude_dispatch_claims_and_creates_worktree_without_editing_installed_artifacts() {
    let harness = fixture();
    let root = claude_root(&harness);
    install_checkout_helpers_at(&root);
    let hook = hook_of(&root);
    let helper = helper_of(&root);
    let artifacts = (
        regular_files(&harness.home.join(".claude")),
        regular_files(&harness.home.join(".codex")),
    );
    let text = format!("bash {} dispatch TST-1 --agent=claude", quoted(&helper));
    for codex in [false, true] {
        assert_eq!(
            ask_hook(&harness, &hook, &text, codex),
            serde_json::json!({}),
            "{text}"
        );
    }
    // The fake tmux models only the terminal. The helper, CLI, daemon, Git
    // worktree and story claim are real — the Claude half of the launcher
    // door's own integration test.
    let mut command = shell(&harness);
    command
        .args([
            "-c",
            r#"
source "$2/plugins/story/tests/lib.sh"
repo=$(mk_story_repo)
id=$(new_story "$repo" "Installed helper dispatch")
cd "$repo" || exit 1
export PATH="$TESTS_DIR/fakes:$PATH" TMUX=fake,0,0 TMUX_PANE=%0
export STORY_READY_DELAY=0 STORY_READY_FALLBACK_DELAY=0
export STORY_CONFIRM_DELAY=0 STORY_PASTE_SETTLE_DELAY=0
export FAKE_TMUX_CAPTURE=marker
out=$(bash "$1" dispatch "$id" --agent=claude)
tmux kill-window -t "$id"
assert_eq "$(jqf "$out" .ok)" true "installed dispatch succeeds"
assert_eq "$(jqf "$out" .claimed)" true "installed dispatch claims"
assert_eq "$(jqf "$out" .prompt_confirmed)" true "prompt was delivered"
[ -d "$repo/.claude/worktrees/$id" ] || fail_test "missing worktree"
git show-ref --verify --quiet "refs/heads/worktree-$id" || fail_test "missing branch"
assert_eq "$(story show "$id" --json | jq -r '.story.story.state')" in-progress "persisted claim"
printf '%s\n' "$out"
"#,
            "installed-helper-dispatch-test",
        ])
        .arg(&helper)
        .arg(env!("CARGO_MANIFEST_DIR"))
        .env("STORYHOOK_TEST_HOME", &harness.home);
    let output = run_bounded(
        command,
        "real installed helper dispatch",
        STORY_COMMAND_DEADLINE,
    );
    assert!(output.status.success(), "{}", combined(&output));
    assert_eq!(
        (
            regular_files(&harness.home.join(".claude")),
            regular_files(&harness.home.join(".codex")),
        ),
        artifacts,
        "dispatch changed installed artifacts"
    );
}
