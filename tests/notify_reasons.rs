//! Fences the verifier's classification of `story.sh notify` refusals against
//! the helper's own vocabulary (SH-650).
//!
//! `return_for_repair` (`src/daemon/verification.rs`) re-dispatches a returned
//! story into its own window when — and only when — `notify` refused with a
//! slug that means "no live dispatched agent is there" (`pane-dead`,
//! `pane-unavailable`); every other slug parks the story,
//! because a `dispatch --resume` respawns over whatever the pane holds and a
//! respawn over a live agent kills it. That decision is a table,
//! `NOTIFY_REFUSALS`, keyed by the exact slugs `cmd_notify` emits. A slug the
//! helper grows that the table does not know is classified "not absent" —
//! which is safe, and which is also silent: the verifier would park every
//! such story for ever and nobody would be told a classification was missing.
//! A slug the table names that the helper no longer emits is a classification
//! of nothing.
//!
//! So the two are derived against each other in both directions, in the
//! SH-198/SH-360/SH-364 style: DECLARED is every `refuse "<slug>"` (and
//! `refuse_with`) literal inside `cmd_notify`'s body, read from the tracked
//! `plugins/story/bin/story.sh`; CLASSIFIED is `NOTIFY_REFUSALS`'s keys. The
//! scanner carries a positive control so a parser that stopped matching
//! cannot report a clean tree by accident.
//!
//! The second pin is the probe spelling: `pane_is_dead` (`lib/session.sh`)
//! asks tmux the same composite question the Full Auto reconciler asks
//! (`WINDOW_PROBE_FORMAT`), because the fake tmux answers that one format in
//! one arm and a second spelling would be a third format for every fixture
//! to learn (SH-136). The two literals must stay byte-identical.
//!
//! The third pin is the key senders (SH-780). `notify` pasted a remediation
//! and pressed Enter without looking, and Enter on a dialog approves it for
//! the person. `KEY_SENDERS` lists every place the plugin presses a key in a
//! pane, with the reason it may, and the scan demands exactly that set; the
//! last test pins the one guarded delivery shape that both prompt senders —
//! `cmd_notify` and dispatch's `send_prompt_confirmed` (SH-799) — must keep:
//! an idle check, one paste, a receipt of this prompt, and a submit key only
//! directly after `composer_holds` sees the prompt, on every try.

use std::collections::BTreeSet;
use std::path::PathBuf;

use storyhook::daemon::verification::NOTIFY_REFUSALS;
use storyhook::service::engine::WINDOW_PROBE_FORMAT;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
}

/// The body of one top-level bash function: from `name() {` to the first
/// line that is exactly `}`.
fn function_body<'a>(script: &'a str, name: &str) -> &'a str {
    let header = format!("\n{name}() {{\n");
    let start = script
        .find(&header)
        .unwrap_or_else(|| panic!("{name}() must be defined at top level"))
        + header.len();
    let end = script[start..]
        .find("\n}\n")
        .unwrap_or_else(|| panic!("{name}() must end with a bare closing brace"));
    &script[start..start + end]
}

/// Every slug passed as the first argument of `refuse` / `refuse_with` in
/// `body`, in the shape `refuse "slug"` or `refuse_with slug` (the helper
/// writes the slug as a double-quoted or bare word, never a variable).
fn refusal_slugs(body: &str) -> BTreeSet<String> {
    let mut slugs = BTreeSet::new();
    for verb in ["refuse_with", "refuse"] {
        let mut rest = body;
        while let Some(at) = rest.find(verb) {
            let after = &rest[at + verb.len()..];
            rest = after;
            // `refuse_with` also matches the prefix `refuse`; only accept a
            // call, which is the verb followed by whitespace.
            let Some(after) = after.strip_prefix(' ') else {
                continue;
            };
            let word = after.trim_start();
            let word = word.strip_prefix('"').unwrap_or(word);
            let end = word
                .find(|c: char| !(c.is_ascii_lowercase() || c == '-'))
                .unwrap_or(word.len());
            if end > 0 {
                slugs.insert(word[..end].to_string());
            }
        }
    }
    slugs
}

#[test]
fn the_scanner_reads_refusal_slugs_out_of_a_function_body() {
    let script = "\nother() {\n  refuse \"not-this-one\" \"x\"\n}\n\ncmd_probe() {\n  [ -n \"$x\" ] \\\n    || refuse \"pane-unavailable\" \"no window\"\n  refuse_with \"pane-changed\" \"$msg\" \"$json\"\n  refuse pane-dead \"bare word\"\n}\n";
    let body = function_body(script, "cmd_probe");
    assert_eq!(
        refusal_slugs(body),
        ["pane-changed", "pane-dead", "pane-unavailable"]
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>()
    );
}

#[test]
fn every_notify_refusal_is_classified_by_name_and_nothing_else_is() {
    let script = read("plugins/story/bin/story.sh");
    let mut declared = refusal_slugs(function_body(&script, "cmd_notify"));
    let resources = read("plugins/story/lib/resources.sh");
    declared.extend(refusal_slugs(function_body(
        &resources,
        "load_story_resources",
    )));
    declared.extend(refusal_slugs(function_body(
        &resources,
        "revalidate_story_resources",
    )));
    assert!(
        declared.len() >= 4,
        "corpus floor: cmd_notify is expected to refuse in several named ways, found {declared:?}"
    );
    let classified: BTreeSet<String> = NOTIFY_REFUSALS
        .iter()
        .map(|(slug, _)| (*slug).to_string())
        .collect();
    assert_eq!(
        classified.len(),
        NOTIFY_REFUSALS.len(),
        "a slug classified twice is a table that disagrees with itself"
    );
    let unclassified: Vec<_> = declared.difference(&classified).collect();
    assert!(
        unclassified.is_empty(),
        "cmd_notify emits {unclassified:?} which NOTIFY_REFUSALS does not classify; add it, saying whether it means the agent is absent"
    );
    let phantom: Vec<_> = classified.difference(&declared).collect();
    assert!(
        phantom.is_empty(),
        "NOTIFY_REFUSALS classifies {phantom:?} which cmd_notify never emits"
    );
}

#[test]
fn the_shell_pane_probe_asks_the_engines_own_question() {
    let session = read("plugins/story/lib/session.sh");
    let line = session
        .lines()
        .find(|line| line.starts_with("PANE_PROBE_FORMAT="))
        .expect("lib/session.sh defines PANE_PROBE_FORMAT");
    let literal = line
        .trim_start_matches("PANE_PROBE_FORMAT=")
        .trim_matches('\'');
    assert_eq!(
        literal, WINDOW_PROBE_FORMAT,
        "pane_is_dead must ask the composite format the reconciler and the fake tmux already speak"
    );
    assert!(
        function_body(&session, "pane_probe").contains("\"$PANE_PROBE_FORMAT\""),
        "pane_probe must ask with the named constant, not a second spelling"
    );
}

/// Every place the plugin presses a key in a pane: `(file, function, why it
/// may)`.
///
/// SH-780: `notify` pasted a remediation and pressed Enter without looking at
/// the pane, and Enter on a dialog's cursor row (`❯ 1. Yes`) approves the
/// dialog for the person. The defect class is a key sent into an agent's pane
/// with no proof of what it will act on. So every sender is written down here
/// with its reason, and [`every_key_sent_into_a_pane_is_a_listed_sender`]
/// demands that the plugin's source holds exactly these: a new sender fails
/// the build until someone states why it is safe.
const KEY_SENDERS: [(&str, &str, &str); 6] = [
    (
        "plugins/story/bin/story.sh",
        "cmd_notify",
        "the submit key, and only while composer_holds sees this very prompt",
    ),
    (
        "plugins/story/bin/story.sh",
        "ensure_provider_plan_mode",
        "Codex's plan-mode key before any prompt is typed, at a composer the readiness gate matched",
    ),
    (
        "plugins/story/hooks/full-auto.sh",
        "approve_claude_plan",
        "Enter on a plan-approval prompt that the Full Auto watcher matched exactly",
    ),
    (
        "plugins/story/hooks/full-auto.sh",
        "approve_codex_plan",
        "Enter on a plan-approval prompt that the Full Auto watcher matched exactly",
    ),
    (
        "plugins/story/lib/interrupt-agent.py",
        "interrupt",
        "Escape, the native interrupt, after the session is bound to the expected target",
    ),
    (
        "plugins/story/lib/session.sh",
        "send_prompt_confirmed",
        "the dispatch submit key, only into a composer that read idle and only while composer_holds sees this very prompt",
    ),
];

/// The `(file, function)` of every key-sending line in one plugin source file.
///
/// Bash: a code line (not a comment) that runs `tmux send-keys`, with no
/// double quote before it on the line — so the `"tmux send-keys -t <pane> …"`
/// strings that dispatch prints as a dry-run plan are not senders. Its
/// function is the nearest `name() {` above it. Python: a line naming
/// `"send-keys"`, in the nearest `def` above it.
fn key_senders(relative: &str, source: &str) -> Vec<(String, String)> {
    let python = relative.ends_with(".py");
    let mut function = String::from("<top level>");
    let mut found = Vec::new();
    for line in source.lines() {
        let code = line.trim_start();
        if python {
            if let Some(rest) = code.strip_prefix("def ") {
                function = rest.split('(').next().unwrap_or(rest).to_string();
            }
            if !code.starts_with('#') && code.contains("\"send-keys\"") {
                found.push((relative.to_string(), function.clone()));
            }
            continue;
        }
        if let Some(name) = line.strip_suffix("() {")
            && !name.contains(' ')
        {
            function = name.to_string();
        }
        if code.starts_with('#') {
            continue;
        }
        if let Some(at) = code.find("tmux send-keys")
            && !code[..at].contains('"')
        {
            found.push((relative.to_string(), function.clone()));
        }
    }
    found
}

/// Every regular source file under the plugin's executable directories.
fn plugin_sources() -> Vec<String> {
    let mut files = Vec::new();
    for dir in [
        "plugins/story/bin",
        "plugins/story/lib",
        "plugins/story/hooks",
    ] {
        let mut entries: Vec<_> = std::fs::read_dir(repo_root().join(dir))
            .unwrap_or_else(|error| panic!("{dir} must be readable: {error}"))
            .map(|entry| entry.expect("directory entry").path())
            .filter(|path| path.is_file())
            .collect();
        entries.sort();
        for path in entries {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name.ends_with(".sh") || name.ends_with(".py") {
                files.push(format!("{dir}/{name}"));
            }
        }
    }
    files
}

#[test]
fn the_sender_scanner_finds_code_and_skips_plans_and_comments() {
    let bash = "\nsend_it() {\n  # tmux send-keys -t \"$pane\" Enter is only a comment\n  printf '%s' \"(\\\"tmux send-keys -t <pane> \\\" + $key)\"\n  if tmux send-keys -t \"$pane\" \"$SUBMIT_KEY\"; then :; fi\n}\n";
    assert_eq!(
        key_senders("x.sh", bash),
        vec![("x.sh".to_string(), "send_it".to_string())]
    );
    let python = "def press(pane):\n    # proc.run(\"tmux\", \"send-keys\") in a comment\n    proc.run(\"tmux\", \"send-keys\", \"-t\", pane, \"Escape\")\n";
    assert_eq!(
        key_senders("x.py", python),
        vec![("x.py".to_string(), "press".to_string())]
    );
}

#[test]
fn every_key_sent_into_a_pane_is_a_listed_sender() {
    let mut found = Vec::new();
    for file in plugin_sources() {
        found.extend(key_senders(&file, &read(&file)));
    }
    found.sort();
    let mut listed: Vec<(String, String)> = KEY_SENDERS
        .iter()
        .map(|(file, function, _)| ((*file).to_string(), (*function).to_string()))
        .collect();
    listed.sort();
    assert_eq!(
        found, listed,
        "the plugin's key senders changed; a key sent into an agent's pane needs proof of what it acts on (SH-780) — list the sender in KEY_SENDERS with that reason, or remove it"
    );
}

/// A loop header or its `done`: the lines [`assert_guarded_delivery`] counts to
/// find the loop that holds the submit key.
fn opens_loop(line: &str) -> bool {
    let line = line.trim();
    (line.starts_with("while ") || line.starts_with("until ") || line.starts_with("for "))
        && line.ends_with("do")
}

/// True when `line` calls `composer_holds "$pane"` itself — not a longer name
/// such as `poll_composer_holds`, which is the receipt, not the gate.
fn calls_composer_holds(line: &str) -> bool {
    line.match_indices("composer_holds \"$pane\"").any(|(at, _)| {
        !line[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The guarded delivery a prompt sender must keep (SH-780, SH-799): an idle
/// check (`idle_check`, named per function) before the one paste; the receipt
/// `poll_composer_holds "$pane"` after the paste and before the key loop;
/// exactly one `"$SUBMIT_KEY"` send, inside a `while … done`; and, inside that
/// loop at most two lines above the send, a `composer_holds "$pane"` call that
/// gates it (`||`, or `if !`). The "any text" receipt must be gone.
fn assert_guarded_delivery(name: &str, body: &str, idle_check: &str) {
    let lines: Vec<&str> = body
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect();
    let at = |needle: &str| -> Vec<usize> {
        lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.contains(needle))
            .map(|(index, _)| index)
            .collect()
    };
    assert!(
        !body.contains("poll_input \"$pane\" text"),
        "{name}: \"any text\" is not a receipt; a dialog's cursor row is text too (SH-799)"
    );
    let pastes = at("paste_prompt ");
    assert_eq!(pastes.len(), 1, "{name}: one guarded paste (SH-780)");
    let idle = at(idle_check);
    assert!(
        idle.len() == 1 && idle[0] < pastes[0],
        "{name}: the paste must follow the idle check `{idle_check}`"
    );
    let submits = at("\"$SUBMIT_KEY\"");
    assert_eq!(submits.len(), 1, "{name}: the submit key is sent from one place");
    let send = submits[0];
    let mut depth = 0;
    let header = (0..send)
        .rev()
        .find(|&index| {
            if lines[index].trim() == "done" {
                depth += 1;
            } else if opens_loop(lines[index]) {
                if depth == 0 {
                    return true;
                }
                depth -= 1;
            }
            false
        })
        .unwrap_or_else(|| panic!("{name}: the submit key must be sent inside a retry loop"));
    let receipt = at("poll_composer_holds \"$pane\"");
    assert!(
        receipt
            .iter()
            .any(|&line| line >= pastes[0] && line < header),
        "{name}: the receipt must be poll_composer_holds of this prompt, after the paste and before the key loop"
    );
    let gated = (header + 1..send).any(|index| {
        send - index <= 2
            && calls_composer_holds(lines[index])
            && (lines[index].contains("||")
                || lines[index].trim_start().starts_with("if !")
                || lines
                    .get(index + 1)
                    .is_some_and(|next| next.trim_start().starts_with("||")))
    });
    assert!(
        gated,
        "{name}: inside its loop, the submit key must directly follow a composer_holds check that stops the loop, on every try"
    );
}

#[test]
fn the_guarded_delivery_check_reads_the_gate_not_the_receipt() {
    let guarded = "if ! poll_composer_idle \"$pane\"; then return 1; fi\npaste_prompt \"$pane\" \"$t\" b && poll_composer_holds \"$pane\" \"$t\"\nwhile [ \"$try\" -le 2 ]; do\n  composer_holds \"$pane\" \"$t\" || break\n  if tmux send-keys -t \"$pane\" \"$SUBMIT_KEY\"; then return 0; fi\ndone\n";
    assert_guarded_delivery("positive control", guarded, "poll_composer_idle ");
    let receipt_only = guarded.replace("  composer_holds \"$pane\" \"$t\" || break\n", "");
    let unguarded = std::panic::catch_unwind(|| {
        assert_guarded_delivery("receipt only", &receipt_only, "poll_composer_idle ")
    });
    assert!(
        unguarded.is_err(),
        "a poll_composer_holds receipt must not count as the per-key gate"
    );
    let gate_outside_loop = guarded.replace(
        "while [ \"$try\" -le 2 ]; do\n  composer_holds \"$pane\" \"$t\" || break\n",
        "composer_holds \"$pane\" \"$t\" || return 1\nwhile [ \"$try\" -le 2 ]; do\n",
    );
    let once = std::panic::catch_unwind(|| {
        assert_guarded_delivery("gate outside the loop", &gate_outside_loop, "poll_composer_idle ")
    });
    assert!(
        once.is_err(),
        "a gate above the loop does not guard the re-sent keys"
    );
}

#[test]
fn notify_types_once_into_an_idle_composer_and_submits_only_what_it_sees() {
    let script = read("plugins/story/bin/story.sh");
    assert_guarded_delivery(
        "cmd_notify",
        function_body(&script, "cmd_notify"),
        "input_state \"$pane\" strict",
    );
}

#[test]
fn dispatch_types_once_into_an_idle_composer_and_submits_only_what_it_sees() {
    let session = read("plugins/story/lib/session.sh");
    assert_guarded_delivery(
        "send_prompt_confirmed",
        function_body(&session, "send_prompt_confirmed"),
        "poll_composer_idle \"$pane\"",
    );
}
