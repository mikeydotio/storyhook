//! A session hook's `--deadline` must leave it room to finish before the
//! manifest that ships it is killed — SH-182's regression guard.
//!
//! # Why this file exists
//!
//! `plugins/story/hooks/hooks.json` gives each hook a wall-clock
//! `timeout`; the script inside declares its own `--deadline` to
//! `story`. Once SH-182 made that the mechanism, the two numbers can drift
//! apart again exactly the way the original bug did — a script's own
//! `--deadline` raised without raising the manifest's `timeout` to match,
//! or a fresh hook added to the manifest with no `--deadline` in its script
//! at all. Both are the same defect this story fixed, reintroduced.
//!
//! # Whose obligation it is (SH-460)
//!
//! The rule is *"a hook that waits on the daemon must bound that wait"*, and it
//! was written when every hook did. `full-auto.sh` calls no `story` command at
//! all -- it reads a payload, decides, and prints -- so it structurally cannot
//! have SH-182's defect and has no deadline to declare. The exemption is
//! **derived** from what each script actually does ([`invokes_story`]), never
//! hand-listed: SH-343 already had to un-hand-list this very file once, and a
//! named exemption is how the next hook that *does* call `story` slips through
//! unbounded. The obligation runs in both directions -- a script that invokes
//! `story` must declare a deadline, and one that does not must declare none, so
//! the two files can never quietly disagree about which case a hook is in.
//!
//! # Static, and costs no wall clock
//!
//! Every assertion here reads `hooks.json` and the hook scripts as text; none
//! of them run a `story` command or a daemon. That is the property the story
//! itself asked for in its own description: "the regression test is static
//! and costs no wall clock". The *behavioural* proof that a short deadline
//! actually returns fast under contention lives in `tests/cli_deadline.rs`,
//! which exercises `--deadline` directly against a held spawn lock; this file
//! only pins that every hook asks for one, and asks for enough margin to use
//! it.

use std::time::Duration;

use storyhook_test_support::DeclaredHook;

/// How much of the manifest's `timeout` a hook script's own `--deadline` must
/// leave unclaimed.
///
/// Covers the wrapper around the `story` call: bash startup, the stdin read,
/// `python3` interpreter startup (two of the three scripts pay it),
/// `exec`ing `story`, and rendering the result. Measured loosely rather than
/// tightly — the property this pins is "meaningful headroom exists", not a
/// specific millisecond count, because the latter would make this test
/// change every time the wrapper picked up or dropped a `python3` call for
/// reasons that have nothing to do with SH-182.
const MIN_WRAPPER_MARGIN: Duration = Duration::from_secs(1);

/// The `--deadline <n>` a hook script declares in its `story` invocation, if
/// it declares one.
///
/// A plain substring search on the source text rather than a shell parse:
/// these are short, hand-written scripts with one `story` call apiece, and
/// the alternative — actually invoking the script to observe its behaviour —
/// is exactly the wall-clock cost this file exists to avoid paying.
///
/// Comment lines are excluded before the search runs. Each script documents
/// its own `--deadline` in a `#`-prefixed line right above the call (so a
/// reader sees the budget without opening `hooks.json`), and a naive search
/// over the whole file matches that prose first — `--deadline 3: this hook
/// has 5s...` parses "3:" as the value and fails to parse as a number, which
/// is a comment-wording accident, not the thing this test is for.
fn deadline_flag(script: &str) -> Option<Duration> {
    let functional: String = script
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let after = functional.split("--deadline").nth(1)?;
    let value = after.split_whitespace().next()?;
    value.parse::<u64>().ok().map(Duration::from_secs)
}

/// Shell words that may legitimately stand in front of a command, and so are
/// skipped before asking what the command is.
///
/// Deliberately short. Every extra entry is one more English word that lets the
/// prose after it masquerade as a command name, and this file reads scripts
/// whose functional lines include a here-document full of sentences.
const COMMAND_PREFIXES: &[&str] = &["if", "then", "elif", "else", "do", "while", "until", "!"];

/// Whether a hook script actually invokes the `story` CLI.
///
/// Asked at a **command position**, never as a bare word search. `full-auto.sh`
/// tells the model to "record the decision as a comment on <id>", and its own
/// generic wording names a story in English, on a functional line -- a substring
/// match would read that as an invocation and demand a `--deadline` for a script
/// that never waits on anything.
///
/// So: comment lines go first, the remainder is split on the shell's own command
/// separators, any leading [`COMMAND_PREFIXES`] word is dropped, and a segment
/// counts when what is left begins `story` or `command -v story`. That second
/// form is not decoration -- it is how all three of the CLI-calling hooks check
/// for the binary before using it, and `if ! command -v story &>/dev/null` puts
/// two prefixes and a separator between the segment start and the word.
fn invokes_story(source: &str) -> bool {
    let functional: String = source
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    functional
        .split(['\n', ';', '|', '&', '(', ')', '`'])
        .any(|segment| {
            let mut words = segment
                .split_whitespace()
                .skip_while(|word| COMMAND_PREFIXES.contains(word));
            match words.next() {
                Some("story") => true,
                Some("command") => words.next() == Some("-v") && words.next() == Some("story"),
                _ => false,
            }
        })
}

/// [`invokes_story`] answers about a command position, and says so in both
/// directions.
///
/// The positive control is the point (SH-364's lesson, one predicate over): a
/// detector that silently stopped recognising invocations would exempt every
/// hook at once and report a clean tree, and nothing else in this file would
/// notice. So the three scripts that really do call `story` are asserted to be
/// seen, alongside constructed cases for the shapes that make this hard --
/// prose that merely names a story, a commented-out call, and an assignment
/// whose value happens to be the word.
#[test]
fn invokes_story_reads_command_position_and_not_prose() {
    for script in ["session-start.sh", "post-git.sh", "stop-handoff.sh"] {
        let path = storyhook_test_support::hook_script(script);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} should be readable: {e}", path.display()));
        assert!(
            invokes_story(&source),
            "{script} calls `story` and the detector missed it -- every exemption \
             this file grants depends on this predicate seeing real invocations"
        );
    }

    for (label, sample) in [
        ("a bare call", "story list\n"),
        (
            "a pipeline",
            "printf '%s' \"$x\" | story --deadline 3 session-start\n",
        ),
        (
            "a substitution",
            "out=$(story --deadline 8 commit-sync --quiet)\n",
        ),
        (
            "a guarded lookup",
            "if ! command -v story &>/dev/null; then\n",
        ),
        ("a loop body", "for f in a b; do story show \"$f\"; done\n"),
    ] {
        assert!(
            invokes_story(sample),
            "{label} should read as an invocation"
        );
    }

    for (label, sample) in [
        ("a comment", "# story list\n"),
        ("indented prose in a comment", "  # run story next first\n"),
        (
            "prose naming a story",
            "reason=\"a comment on this lane's story\"\n",
        ),
        (
            "prose mid-sentence",
            "printf 'record it on the story you are working'\n",
        ),
        ("an assignment", "story_id=SH-1\n"),
    ] {
        assert!(
            !invokes_story(sample),
            "{label} is not an invocation and must not demand a --deadline"
        );
    }

    // The real negative, and the one the exemption actually rests on. A
    // constructed sample only proves the predicate handles a shape somebody
    // thought of; this proves it handles the file it is exempting -- whose
    // functional lines carry a here-document of English prose that names a
    // story twice.
    let full_auto = storyhook_test_support::hook_script("full-auto.sh");
    let source = std::fs::read_to_string(&full_auto)
        .unwrap_or_else(|e| panic!("{} should be readable: {e}", full_auto.display()));
    assert!(
        !invokes_story(&source),
        "full-auto.sh reads as calling `story`. If it genuinely now does, give it a \
         --deadline; if the predicate is matching its denial text, the predicate is \
         what needs fixing -- the exemption this file grants rests on this answer"
    );
    assert!(
        source.contains("comment on"),
        "the negative above is only meaningful while full-auto.sh's feedback still \
         names a story in prose -- otherwise it passes for a reason that has nothing \
         to do with command-position matching"
    );
}

/// The predicate's own boundary, pinned rather than claimed away.
///
/// Splitting on the shell's separators does not know about quoting, so a `;`
/// inside a string starts a new segment: `printf 'ask the owner; story time'`
/// reads as an invocation. It is a shell heuristic, exactly as `deadline_flag`
/// above is a substring heuristic, and the direction of the error is the point
/// — it over-approximates, so the worst case is demanding a `--deadline` from a
/// script that does not need one, which fails loudly and names itself. The
/// opposite error would silently exempt a hook that really does wait on the
/// daemon, which is SH-182 all over again.
#[test]
fn invokes_story_over_approximates_across_a_quoted_separator() {
    assert!(
        invokes_story("printf 'ask the story owner; story ownership matters'\n"),
        "if this stopped being true the predicate learned about quoting — good, but \
         say so here rather than leaving a stale claim about its limits"
    );
}

/// Every hook that declares a manifest `timeout` **and calls `story`** also
/// declares its own `--deadline`, strictly less than that timeout.
///
/// This is the regression itself, stated as a fact about the two files
/// together: a hook with a manifest timeout and no `--deadline` (or one at
/// least as large) is a hook whose budget is once again set by whichever
/// number Claude Code enforces from outside, with nothing inside storyhook
/// bounding the daemon wait first.
///
/// The hook set itself is read fresh from `hooks.json` every run
/// ([`DeclaredHook`], SH-343) rather than kept as a list in this file — a
/// fourth manifest entry is checked the moment it exists, not the moment
/// someone remembers to add it here too.
#[test]
fn every_hook_declares_a_deadline_inside_its_manifest_timeout() {
    for hook in storyhook_test_support::all_declared_hooks() {
        let DeclaredHook {
            event,
            script,
            timeout,
        } = hook;
        let script_path = storyhook_test_support::hook_script(&script);
        let source = std::fs::read_to_string(&script_path)
            .unwrap_or_else(|e| panic!("{} should be readable: {e}", script_path.display()));

        if !invokes_story(&source) {
            // The other direction, so the two files cannot disagree about which
            // case a hook is in: a script that waits on nothing must not carry a
            // budget for a wait, and a stray one means either the script grew a
            // call the detector missed or the flag is decoration.
            assert!(
                deadline_flag(&source).is_none(),
                "{script} declares a --deadline but calls no `story` command. Either \
                 the invocation is written in a shape invokes_story does not read -- in \
                 which case fix the predicate, not this assertion -- or the flag is \
                 decoration and should go."
            );
            continue;
        }

        let deadline = deadline_flag(&source).unwrap_or_else(|| {
            panic!(
                "{script} declares no --deadline, but {} gives it a {}s \
                 timeout. Without --deadline the script's `story` call is bounded only \
                 by SPAWN_LOCK_DEADLINE + SERVED_DEADLINE (150s) — this is the exact \
                 shape of SH-182.",
                storyhook_test_support::HOOKS_MANIFEST,
                timeout.as_secs(),
            )
        });

        assert!(
            deadline < timeout,
            "{script} declares --deadline {}s against a manifest timeout of \
             {}s for {event}. The deadline must leave room for the wrapper around it \
             (bash startup, the stdin read, exec'ing story, rendering) to finish before \
             the agent host kills the whole hook.",
            deadline.as_secs(),
            timeout.as_secs(),
        );

        let margin = timeout - deadline;
        assert!(
            margin >= MIN_WRAPPER_MARGIN,
            "{script} leaves only {margin:?} between its --deadline ({}s) and \
             {event}'s manifest timeout ({}s) — under the {MIN_WRAPPER_MARGIN:?} this test \
             treats as meaningful wrapper headroom.",
            deadline.as_secs(),
            timeout.as_secs(),
        );
    }
}

/// The manifest names a real script for every hook it declares, and that
/// script declares the flag by its long form.
///
/// A narrower guard against a typo passing the test above by accident: if a
/// script were renamed or moved without updating `hooks.json`, the source
/// read above already panics loudly — this instead catches the quieter
/// mistake of a script whose `--deadline` is misspelled or spelled as an
/// environment variable, which `deadline_flag` would read as "no deadline"
/// and the test above would still catch, but with a less specific message
/// than this one gives.
#[test]
fn every_declared_hook_script_exists_and_spells_deadline_as_a_flag() {
    for hook in storyhook_test_support::all_declared_hooks() {
        let script_path = storyhook_test_support::hook_script(&hook.script);
        assert!(
            script_path.is_file(),
            "{} does not exist, but hooks.json's {} entry names it",
            script_path.display(),
            hook.event,
        );
        let source = std::fs::read_to_string(&script_path).unwrap();
        if !invokes_story(&source) {
            continue;
        }
        assert!(
            source.contains("--deadline "),
            "{} must spell the flag as `--deadline <seconds>`, space-separated \
             (not `--deadline=`, which this file's parser does not read): {}",
            hook.script,
            script_path.display()
        );
    }
}

/// The scripts `hooks.json` currently names, one row per declared entry, pinned
/// so a change to the manifest is a visible diff in this test rather than silent.
///
/// `full-auto.sh` appears four times: three `PreToolUse` matchers (SH-460) and
/// one `PermissionRequest` matcher (SH-511). Enumeration is per entry, not per
/// file, so losing either plan-review path or one of the question matchers is a
/// visible diff here -- the same property the hand-maintained list used to
/// give, minus the failure mode where the list and the manifest could disagree.
#[test]
fn the_manifest_currently_declares_exactly_these_four_scripts() {
    let mut scripts: Vec<String> = storyhook_test_support::all_declared_hooks()
        .into_iter()
        .map(|hook| hook.script)
        .collect();
    scripts.sort();
    assert_eq!(
        scripts,
        vec![
            "full-auto.sh",
            "full-auto.sh",
            "full-auto.sh",
            "full-auto.sh",
            "post-git.sh",
            "session-start.sh",
            "stop-handoff.sh",
        ],
        "hooks.json's declared scripts have changed — if this is a real new hook, \
         the tests above already cover it automatically; update this list to match."
    );
}

/// Codex and Claude load the same default-discovered hook manifest. Pin the
/// cross-provider protocol itself: exact events, Bash matcher, budgets, and a
/// root expression that works with Codex's documented `PLUGIN_ROOT` while
/// retaining Claude's compatibility variable.
#[test]
fn hook_manifest_has_the_shared_provider_contract() {
    let path = repo_root().join(storyhook_test_support::HOOKS_MANIFEST);
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display())),
    )
    .expect("hooks.json should be valid JSON");

    let hooks = manifest["hooks"]
        .as_object()
        .expect("hooks.json should contain a hooks object");
    let mut events: Vec<&str> = hooks.keys().map(String::as_str).collect();
    events.sort_unstable();
    assert_eq!(
        events,
        [
            "PermissionRequest",
            "PostToolUse",
            "PreToolUse",
            "SessionStart",
            "Stop"
        ]
    );

    // One row per (event, matcher). `PreToolUse` retains Claude's plan-tool
    // allow plus the two question tools, while `PermissionRequest` carries the
    // separate chooser path from SH-511. Codex's matcher semantics were
    // measured (SH-459) only for a plain tool name, and a wildcard would pay a
    // hook process on every tool call a lane makes.
    let expected = [
        ("SessionStart", "*", "session-start.sh", 5),
        ("PreToolUse", "ExitPlanMode", "full-auto.sh", 10),
        ("PreToolUse", "AskUserQuestion", "full-auto.sh", 10),
        ("PreToolUse", "request_user_input", "full-auto.sh", 10),
        ("PermissionRequest", "ExitPlanMode", "full-auto.sh", 10),
        ("PostToolUse", "Bash", "post-git.sh", 10),
        ("Stop", "*", "stop-handoff.sh", 15),
    ];

    // Compared as a set in both directions, because looking each expected entry
    // up by matcher below can only ever prove the manifest has AT LEAST these --
    // a fourth PreToolUse matcher wiring some other tool would otherwise be
    // invisible here.
    let mut declared_pairs: Vec<(String, String)> = hooks
        .iter()
        .flat_map(|(event, matchers)| {
            matchers
                .as_array()
                .unwrap_or_else(|| panic!("{event} must be an array of matchers"))
                .iter()
                .map(move |entry| {
                    (
                        event.clone(),
                        entry["matcher"]
                            .as_str()
                            .unwrap_or_else(|| panic!("{event} entry declares no matcher"))
                            .to_string(),
                    )
                })
        })
        .collect();
    declared_pairs.sort();
    let mut expected_pairs: Vec<(String, String)> = expected
        .iter()
        .map(|(event, matcher, _, _)| ((*event).to_string(), (*matcher).to_string()))
        .collect();
    expected_pairs.sort();
    assert_eq!(
        declared_pairs, expected_pairs,
        "the manifest's (event, matcher) pairs drifted"
    );

    for (event, matcher, script, timeout) in expected {
        // By matcher, never by position: PreToolUse holds three entries and an
        // index would quietly hand this loop somebody else's wiring.
        let declaration = manifest["hooks"][event]
            .as_array()
            .unwrap_or_else(|| panic!("hooks.json's {event} entry must be an array"))
            .iter()
            .find(|entry| entry["matcher"] == matcher)
            .unwrap_or_else(|| panic!("hooks.json declares no {event} matcher `{matcher}`"));
        let command = &declaration["hooks"][0];
        assert_eq!(
            command["type"], "command",
            "{event} must remain a command hook"
        );
        assert_eq!(command["timeout"], timeout, "{event} timeout drifted");
        let command_text = command["command"]
            .as_str()
            .expect("hook command should be a string");
        assert!(
            command_text.contains("${PLUGIN_ROOT:-$CLAUDE_PLUGIN_ROOT}"),
            "{event} does not resolve either provider's installed plugin root: {command_text}"
        );
        assert!(command_text.ends_with(&format!("/hooks/{script}\"")));
    }

    let codex_manifest = repo_root().join("plugins/story/.codex-plugin/plugin.json");
    let codex: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&codex_manifest)
            .unwrap_or_else(|e| panic!("reading {}: {e}", codex_manifest.display())),
    )
    .expect("Codex plugin manifest should be valid JSON");
    assert!(
        codex.get("hooks").is_none(),
        "current Codex validation rejects an explicit hooks field; hooks are default-discovered"
    );
}

fn repo_root() -> &'static std::path::Path {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
}
