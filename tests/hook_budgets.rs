//! A session hook's `--deadline` must leave it room to finish before the
//! manifest that ships it is killed — SH-182's regression guard.
//!
//! # Why this file exists
//!
//! `plugin/claude-code/hooks/hooks.json` gives each hook a wall-clock
//! `timeout`; the script inside declares its own `--deadline` to
//! `story`. Once SH-182 made that the mechanism, the two numbers can drift
//! apart again exactly the way the original bug did — a script's own
//! `--deadline` raised without raising the manifest's `timeout` to match,
//! or a fresh hook added to the manifest with no `--deadline` in its script
//! at all. Both are the same defect this story fixed, reintroduced.
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

/// Every hook `hooks.json` declares, and the script it runs.
///
/// `command_substring` is how `hook_budgets::declared_timeout` and this
/// file's own `deadline_flag` both locate one entry among several — matching
/// `tests/session_start_hook.rs`'s existing rule that a manifest gaining a
/// new hook under the same event must not quietly hand a test somebody
/// else's number.
const HOOKS: &[(&str, &str)] = &[
    ("SessionStart", "session-start.sh"),
    ("PostToolUse", "post-git.sh"),
    ("Stop", "stop-handoff.sh"),
];

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

/// Every hook that declares a manifest `timeout` also declares its own
/// `--deadline`, strictly less than that timeout.
///
/// This is the regression itself, stated as a fact about the two files
/// together: a hook with a manifest timeout and no `--deadline` (or one at
/// least as large) is a hook whose budget is once again set by whichever
/// number Claude Code enforces from outside, with nothing inside storyhook
/// bounding the daemon wait first.
#[test]
fn every_hook_declares_a_deadline_inside_its_manifest_timeout() {
    for (event, command_substring) in HOOKS {
        let timeout = storyhook_test_support::declared_timeout(event, command_substring);
        let script_path = storyhook_test_support::hook_script(command_substring);
        let script = std::fs::read_to_string(&script_path)
            .unwrap_or_else(|e| panic!("{} should be readable: {e}", script_path.display()));

        let deadline = deadline_flag(&script).unwrap_or_else(|| {
            panic!(
                "{command_substring} declares no --deadline, but {} gives it a {}s \
                 timeout. Without --deadline the script's `story` call is bounded only \
                 by SPAWN_LOCK_DEADLINE + SERVED_DEADLINE (150s) — this is the exact \
                 shape of SH-182.",
                storyhook_test_support::HOOKS_MANIFEST,
                timeout.as_secs(),
            )
        });

        assert!(
            deadline < timeout,
            "{command_substring} declares --deadline {}s against a manifest timeout of \
             {}s for {event}. The deadline must leave room for the wrapper around it \
             (bash startup, the stdin read, exec'ing story, rendering) to finish before \
             Claude Code kills the whole hook.",
            deadline.as_secs(),
            timeout.as_secs(),
        );

        let margin = timeout - deadline;
        assert!(
            margin >= MIN_WRAPPER_MARGIN,
            "{command_substring} leaves only {margin:?} between its --deadline ({}s) and \
             {event}'s manifest timeout ({}s) — under the {MIN_WRAPPER_MARGIN:?} this test \
             treats as meaningful wrapper headroom.",
            deadline.as_secs(),
            timeout.as_secs(),
        );
    }
}

/// The manifest names a real script for every hook this file checks, and
/// that script declares the flag by its long form.
///
/// A narrower guard against a typo passing the test above by accident: if
/// `command_substring` stopped matching anything in `hooks.json` (a rename,
/// a moved file), `declared_timeout` already panics loudly — this instead
/// catches the quieter mistake of a script whose `--deadline` is misspelled
/// or spelled as an environment variable, which `deadline_flag` would read as
/// "no deadline" and the test above would still catch, but with a less
/// specific message than this one gives.
#[test]
fn every_checked_hook_script_exists_and_spells_deadline_as_a_flag() {
    for (_, command_substring) in HOOKS {
        let script_path = storyhook_test_support::hook_script(command_substring);
        assert!(
            script_path.is_file(),
            "{} does not exist, but hook_budgets.rs checks it",
            script_path.display()
        );
        let script = std::fs::read_to_string(&script_path).unwrap();
        assert!(
            script.contains("--deadline "),
            "{} must spell the flag as `--deadline <seconds>`, space-separated \
             (not `--deadline=`, which this file's parser does not read): {}",
            command_substring,
            script_path.display()
        );
    }
}
