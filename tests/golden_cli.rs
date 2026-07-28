//! The byte-compatibility contract for the data-layer rearchitecture.
//!
//! Every read-surface command is run against **one** seeded fixture project and
//! its stdout is frozen in an `insta` snapshot. The rearchitecture replaces the
//! storage engine, the service layer, and eventually the transport (a daemon
//! hop) underneath these commands; none of that is allowed to change a single
//! byte a user or a script sees. When a wave changes an output, the diff here is
//! the review — and accepting it is a deliberate act, not a silent one.
//!
//! **This is the only file in the project that uses snapshot tests.** A snapshot
//! is the right tool for a frozen contract and the wrong tool for a behavioral
//! assertion, which is what every other test file is doing.
//!
//! ## How it is organized
//!
//! One snapshot per command family per output form, with each invocation
//! labelled inside it (`$ story list --state todo`). Grouping is deliberate: 130
//! single-invocation snapshot files are unreviewable, and a grouped diff still
//! names the exact invocation that moved. Human forms are snapshotted verbatim
//! as text; `--json` forms are parsed and snapshotted as JSON so that
//! nondeterministic *values* can be redacted by path rather than by regex.
//!
//! ## Nondeterminism
//!
//! Everything nondeterministic is redacted declaratively — see
//! `assert_json_golden!` (JSON, by path) and [`filters`] (text, by regex). The
//! two sets deliberately overlap: a command that starts emitting a timestamp in
//! a *new* place is redacted automatically rather than turning the suite red on
//! a clock.
//!
//! Two read-surface commands are deliberately absent:
//!
//! - `story --version` — its output changes on every release bump, so freezing
//!   it buys nothing and costs a snapshot update per release.
//! - `story session-start` — its body is a copy of the help text, so it would
//!   couple this corpus to `src/help_topics.rs` and re-freeze it a second time.
//!
//! `story report --html` is snapshotted in its human form only: its `--json`
//! form is the same HTML document escaped into a single JSON string, which is
//! unreviewable and pins nothing the human form does not.

use std::sync::LazyLock;

use storyhook_test_support::{Project, TestEnv, scratch_root};

// ---------------------------------------------------------------------------
// The fixture
// ---------------------------------------------------------------------------

/// The one seeded project every snapshot in this file reads.
///
/// Built once per test binary. All ~130 invocations below are read-only, so
/// they can share it; building a fixture per snapshot would cost ~130 `story
/// init`s and several hundred more child processes for no isolation gain.
static CORPUS: LazyLock<Project<'static>> = LazyLock::new(build_corpus);

/// Seeds a project deliberately shaped to trip the things that break.
///
/// **Fourteen stories**, so ids run past `SH-9` — `SH-1` vs `SH-10` is where
/// lexicographic and numeric ordering diverge, and the corpus records which
/// commands do which (see the `graph` and `handoff` tables, where today's answer
/// is a defect).
///
/// Coverage: four states including a custom one, all five priorities plus
/// unset, all five built-in types plus a custom one, labels, `parent-of` and
/// `blocks` relations, comments, assignees, two members, two archived (closed)
/// stories, one soft-deleted story, an `awaiting` block, and two phases.
fn build_corpus() -> Project<'static> {
    let project = TestEnv::shared().project().build();
    let run = |args: &[&str]| {
        project.run(args).success();
    };

    run(&["member", "add", "Ada Lovelace <ada@example.com>"]);
    run(&["member", "add", "-g", "grace-hopper"]);
    run(&[
        "state",
        "add",
        "review",
        "--super",
        "OPEN",
        "--description",
        "Awaiting code review",
    ]);
    run(&[
        "type",
        "add",
        "spike",
        "--description",
        "A timeboxed investigation",
    ]);

    // Ids are assigned in creation order, so this block *is* the id map.
    for (title, extra) in [
        (
            "Design the storage engine",
            vec!["--type", "epic", "--priority", "critical"],
        ),
        (
            "Define the Store trait",
            vec![
                "--type",
                "story",
                "--priority",
                "high",
                "--labels",
                "backend,api",
            ],
        ),
        (
            "Implement the SQLite engine",
            vec![
                "--type",
                "story",
                "--priority",
                "high",
                "--label",
                "backend",
            ],
        ),
        (
            "Write the migration runner",
            vec![
                "--type",
                "task",
                "--priority",
                "medium",
                "--label",
                "backend",
            ],
        ),
        (
            "Fix the id collision across worktrees",
            vec![
                "--type",
                "bug",
                "--priority",
                "critical",
                "--label",
                "defect",
            ],
        ),
        (
            "Benchmark the rebuild path",
            vec!["--type", "spike", "--priority", "low"],
        ),
        // SH-7 carries no priority and no labels: the "everything unset" row.
        ("Document the data layer", vec!["--type", "chore"]),
        (
            "Retire the JSONL reader",
            vec!["--type", "chore", "--priority", "low"],
        ),
        (
            "Add the daemon transport",
            vec!["--type", "story", "--priority", "medium"],
        ),
        (
            "Ship the CLI shim",
            vec!["--type", "task", "--priority", "medium"],
        ),
        (
            "Audit error codes",
            vec!["--type", "task", "--priority", "low"],
        ),
        (
            "Harden concurrent writes",
            vec!["--type", "story", "--priority", "high"],
        ),
        (
            "Old approach: file-per-story",
            vec!["--type", "chore", "--priority", "low"],
        ),
        (
            "Prototype the archive format",
            vec!["--type", "spike", "--priority", "medium"],
        ),
    ] {
        let mut args = vec!["new", title];
        args.extend(extra);
        run(&args);
        // KNOWN-DEFECT (needs a story; ids cannot be minted from this worktree):
        // the ready-list sort is `priority ASC, then created_at ASC`
        // (`src/app.rs:302`, and the same comparator at 335/664/1033/2326), but
        // `created_at` has SECOND precision. Stories created within one second
        // tie on both keys, and the stable sort then falls back to the order the
        // story files happened to be read in -- so `story next`, `story
        // summary`, `story context` and `story handoff` return DIFFERENT
        // orderings for identical input depending only on whether the writes
        // straddled a second boundary. Observed live: SH-2 and SH-12 (both
        // `high`) swap places in roughly a third of runs.
        //
        // This sleep makes the fixture immune rather than fixing the defect
        // (this step ships no production changes). Sleeping >= 1s guarantees the
        // stories before it and after it land in different seconds, and the
        // ready set is arranged so that every same-priority PAIR straddles this
        // point -- {SH-1,SH-5} critical, {SH-2,SH-12} high, {SH-4,SH-10} medium.
        // Within each half no two ready stories share a priority, so no tie can
        // form there and the ordering is fully determined by the documented
        // comparator. One sleep, not fourteen, for that reason.
        if title == "Write the migration runner" {
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }
    }

    run(&["relate", "SH-1", "parent-of", "SH-2"]);
    run(&["relate", "SH-1", "parent-of", "SH-3"]);
    run(&["relate", "SH-1", "parent-of", "SH-4"]);
    run(&["relate", "SH-5", "blocks", "SH-9"]);
    run(&["relate", "SH-2", "blocks", "SH-3"]);

    run(&["assign", "SH-2", "ada-lovelace"]);
    run(&["assign", "SH-5", "grace-hopper"]);

    run(&[
        "comment",
        "SH-2",
        "Trait shape settled: one method per verb.",
    ]);
    run(&["comment", "SH-5", "Reproduced with two linked worktrees."]);
    run(&[
        "comment",
        "SH-5",
        "Root cause is the committed next-id counter.",
    ]);

    run(&["phase", "add", "SH-2", "1"]);
    run(&["phase", "add", "SH-3", "1"]);
    run(&["phase", "add", "SH-9", "2"]);

    run(&["block", "SH-6", "waiting on the benchmark harness"]);

    run(&["move", "SH-3", "in-progress"]);
    run(&["move", "SH-4", "review"]);
    run(&["move", "SH-8", "done"]);
    run(&["move", "SH-14", "done"]);
    run(&["delete", "SH-13", "superseded by the global store"]);

    project
}

// ---------------------------------------------------------------------------
// Redaction
// ---------------------------------------------------------------------------

/// Regex filters applied to every **text** snapshot.
///
/// Order matters: the RFC3339 pattern runs before the bare-date pattern, so a
/// full timestamp is never half-eaten by the date rule.
///
/// The path and SHA rules are guards rather than fixes — no read-surface command
/// emits either today. They are declared anyway because the rearchitecture moves
/// the store to a global path, and the first command that starts printing one
/// should be caught by review of *this* file, not by a snapshot that is red on
/// one machine and green on another.
fn filters() -> Vec<(String, &'static str)> {
    vec![
        // 2026-07-28T15:06:14Z — every `at`, `created_at`, `closed_at`, …
        (
            r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z".to_string(),
            "[timestamp]",
        ),
        // `report --html`: "Generated 2026-07-28 15:06 UTC"
        (
            r"\d{4}-\d{2}-\d{2} \d{2}:\d{2} UTC".to_string(),
            "[timestamp]",
        ),
        // `report --html`'s Updated column is a bare date. Scoped to that cell
        // on purpose: an unanchored `\d{4}-\d{2}-\d{2}` also rewrites the
        // `$ story list --created-after 2000-01-01` labels, which would erase
        // from the record which date each filter was actually given.
        (
            r#"<td class="col-date">\d{4}-\d{2}-\d{2}</td>"#.to_string(),
            r#"<td class="col-date">[date]</td>"#,
        ),
        // `list --stale` only: "[stale 12d, last: comment]".
        (r"\[stale \d+d,".to_string(), "[stale [days]d,"),
        // Fixture paths, should one ever reach an output.
        (
            format!("{}/[A-Za-z0-9._-]+", scratch_root().display()),
            "[fixture]",
        ),
        // Full git SHAs. Short SHAs are deliberately NOT filtered: a 7-hex
        // pattern also matches ordinary words, and over-redaction silently
        // weakens the contract.
        (r"\b[0-9a-f]{40}\b".to_string(), "[sha]"),
    ]
}

/// An `insta` configuration carrying [`filters`], scoped to this file's
/// `tests/snapshots/` directory.
fn settings() -> insta::Settings {
    let mut settings = insta::Settings::clone_current();
    for (pattern, replacement) in filters() {
        settings.add_filter(&pattern, replacement);
    }
    // The invocation is written into the snapshot body, so the header's copy of
    // it is noise that doubles every diff.
    settings.set_omit_expression(true);
    settings
}

/// `insta::assert_json_snapshot!` with this file's redactions applied.
///
/// The redaction list lives here, in one place, rather than at each call site:
/// a rearchitecture wave that introduces a new timestamped field must add it
/// once, and cannot add it to some snapshots and forget others.
///
/// `.**.x` matches `x` at any depth including the root, so one entry covers a
/// key wherever a response happens to nest it — which matters most for
/// `export`, whose `at` fields sit inside event variants this file never names.
macro_rules! assert_json_golden {
    ($name:expr, $value:expr) => {
        insta::assert_json_snapshot!($name, $value, {
            ".**.created_at" => "[timestamp]",
            ".**.updated_at" => "[timestamp]",
            ".**.closed_at" => "[timestamp]",
            ".**.at" => "[timestamp]",
            ".**.last_activity_at" => "[timestamp]",
            ".**.days_stale" => "[days]",
        })
    };
}

// ---------------------------------------------------------------------------
// Runners
// ---------------------------------------------------------------------------

/// Runs `story <args>` in the corpus project, asserting it succeeded, and
/// returns stdout.
fn stdout_of(args: &[&str]) -> String {
    let out = CORPUS
        .story()
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("running `story {}`: {e}", args.join(" ")));
    assert!(
        out.status.success(),
        "`story {}` must succeed in the corpus fixture (exit {:?}): {}",
        args.join(" "),
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap_or_else(|e| panic!("`story {}` printed non-UTF-8: {e}", args.join(" ")))
}

/// Snapshots the human-form stdout of every invocation in `table`, as one
/// labelled text document.
fn snapshot_human(name: &str, table: &[&[&str]]) {
    let mut body = String::new();
    for args in table {
        body.push_str(&format!("$ story {}\n", args.join(" ")));
        body.push_str(&stdout_of(args));
        body.push('\n');
    }
    settings().bind(|| insta::assert_snapshot!(name, body));
}

/// Snapshots the `--json` stdout of every invocation in `table`, as one array of
/// `{argv, stdout}` records.
///
/// An array (not an object) because `serde_json` sorts object keys, which would
/// scramble the invocations out of the order the table declares them in.
fn snapshot_json(name: &str, table: &[&[&str]]) {
    let mut records = Vec::new();
    for args in table {
        let with_json: Vec<&str> = args.iter().copied().chain(["--json"]).collect();
        let raw = stdout_of(&with_json);
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|e| {
            panic!(
                "`story {}` did not print JSON ({e}): {raw}",
                with_json.join(" ")
            )
        });
        records.push(serde_json::json!({
            "argv": format!("story {}", with_json.join(" ")),
            "stdout": parsed,
        }));
    }
    settings().bind(|| assert_json_golden!(name, records));
}

/// Snapshots every invocation in `table` as a *failure*: exit code, stdout and
/// stderr, each labelled.
///
/// Recording all three is the point. The SH-59 ruling puts plain-text errors on
/// stderr and `--json` envelopes on stdout, and the daemon hop in W5 is exactly
/// the kind of change that quietly moves a byte from one stream to the other.
fn snapshot_failures(name: &str, table: &[&[&str]], json: bool) {
    let mut body = String::new();
    for args in table {
        let argv: Vec<&str> = if json {
            args.iter().copied().chain(["--json"]).collect()
        } else {
            args.to_vec()
        };
        let out = CORPUS
            .story()
            .args(&argv)
            .output()
            .unwrap_or_else(|e| panic!("running `story {}`: {e}", argv.join(" ")));
        assert!(
            !out.status.success(),
            "`story {}` is in the failure table but exited 0",
            argv.join(" ")
        );
        body.push_str(&format!("$ story {}\n", argv.join(" ")));
        body.push_str(&format!(
            "exit: {}\n",
            out.status
                .code()
                .expect("the CLI must exit, not be signalled")
        ));
        body.push_str(&format!(
            "stdout: {}\n",
            render_stream(&String::from_utf8_lossy(&out.stdout))
        ));
        body.push_str(&format!(
            "stderr: {}\n\n",
            render_stream(&String::from_utf8_lossy(&out.stderr))
        ));
    }
    settings().bind(|| insta::assert_snapshot!(name, body));
}

/// Renders a captured stream so that "empty" is visibly distinct from "one
/// blank line" — the difference between a stream that was written to and one
/// that was not.
fn render_stream(text: &str) -> String {
    if text.is_empty() {
        "<empty>".to_string()
    } else {
        format!("\n{text}")
    }
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

/// Every filter combination that changes the result set, plus the two that
/// select nothing (an empty result is a contract too).
const LIST: &[&[&str]] = &[
    &["list"],
    &["list", "--state", "todo"],
    &["list", "--state", "review"],
    &["list", "--state", "done"],
    &["list", "--type", "bug"],
    &["list", "--type", "spike"],
    &["list", "--type", "none"],
    &["list", "--priority", "critical,high"],
    &["list", "--priority", "none"],
    &["list", "--label", "backend"],
    &["list", "--label", "api,defect"],
    &["list", "--assignee", "ada-lovelace"],
    &["list", "--assignee", "nobody"],
    &["list", "--flagged"],
    &["list", "--blocked"],
    &["list", "--ready"],
    &["list", "--phase", "1"],
    &["list", "--phase", "9"],
    // Deterministic because the fixture's activity is all "now": nothing is an
    // hour stale. Declared so the grammar is frozen even though `days_stale`
    // itself cannot be tripped without a fixture that can age.
    &["list", "--stale", "1h"],
    &["list", "--created-after", "2000-01-01"],
    &["list", "--created-after", "2999-01-01"],
    &["list", "--updated-after", "2000-01-01"],
    // Two filters at once: they must intersect, not replace each other.
    &["list", "--state", "todo", "--priority", "critical"],
    &["list", "--type", "story", "--label", "backend"],
];

#[test]
fn list_human() {
    snapshot_human("list_human", LIST);
}

#[test]
fn list_json() {
    snapshot_json("list_json", LIST);
}

// ---------------------------------------------------------------------------
// show
// ---------------------------------------------------------------------------

/// One story per interesting shape, so every optional line of `render_story`
/// appears in the snapshot at least once.
const SHOW: &[&[&str]] = &[
    // parent-of ×3 + a progress rollup.
    &["show", "SH-1"],
    // assignee, labels, a comment, an outgoing `blocks`.
    &["show", "SH-5"],
    // `awaiting`, and every optional field unset.
    &["show", "SH-6"],
    // No priority, no labels, no relations — the minimal story.
    &["show", "SH-7"],
    // child-of + blocked-by, both DERIVED from the other end of the relation.
    &["show", "SH-3"],
    // Archived: closed, with a closed_at.
    &["show", "SH-8"],
    // Soft-deleted: CLOSED-but-todo, with a deleted_reason.
    &["show", "SH-13"],
];

#[test]
fn show_human() {
    snapshot_human("show_human", SHOW);
}

#[test]
fn show_json() {
    snapshot_json("show_json", SHOW);
}

// ---------------------------------------------------------------------------
// next / summary
// ---------------------------------------------------------------------------

/// `next` changes response *shape* at the count boundary — one story is a
/// `Story`, several are `Stories`, none is a `Message` — so all three are here.
const NEXT: &[&[&str]] = &[
    &["next"],
    &["next", "--count", "3"],
    &["next", "--count", "99"],
    &["next", "--phase", "1"],
    &["next", "--phase", "9"],
];

#[test]
fn next_human() {
    snapshot_human("next_human", NEXT);
}

#[test]
fn next_json() {
    snapshot_json("next_json", NEXT);
}

const SUMMARY: &[&[&str]] = &[&["summary"], &["report"]];

#[test]
fn summary_human() {
    snapshot_human("summary_human", SUMMARY);
}

#[test]
fn summary_json() {
    snapshot_json("summary_json", SUMMARY);
}

/// `report --html` is a whole HTML document. Human form only — see the module
/// header.
#[test]
fn report_html_human() {
    snapshot_human("report_html_human", &[&["report", "--html"]]);
}

// ---------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------

/// Search matches titles, comment bodies and labels; one query per field, plus
/// a miss and a multi-word query.
const SEARCH: &[&[&str]] = &[
    &["search", "engine"],
    &["search", "worktrees"],
    &["search", "backend"],
    &["search", "ENGINE"],
    &["search", "the storage engine"],
    &["search", "nothing-matches-this"],
];

#[test]
fn search_human() {
    snapshot_human("search_human", SEARCH);
}

#[test]
fn search_json() {
    snapshot_json("search_json", SEARCH);
}

// ---------------------------------------------------------------------------
// graph
// ---------------------------------------------------------------------------

// KNOWN-DEFECT (needs a story; ids cannot be minted from this worktree):
// `graph` sorts its id lists LEXICOGRAPHICALLY — "SH-1, SH-10, SH-11, SH-12,
// SH-2, …" — while `list`, `search` and `phase show` sort NUMERICALLY via
// `sort_story_views`. The same lexicographic ordering shows up in `handoff` and
// in the ready lists of `summary` and `context`. Snapshotted as-is, deliberately:
// this corpus freezes current behavior, and fixing it here would mix a behavior
// change into a test-only step. The 14-story fixture exists to make it visible.
const GRAPH: &[&[&str]] = &[
    &["graph"],
    &["graph", "--critical-path"],
    &["graph", "--blocked-by", "SH-5"],
    &["graph", "--blocked-by", "SH-1"],
    &["graph", "--parallel-groups"],
];

#[test]
fn graph_human() {
    snapshot_human("graph_human", GRAPH);
}

#[test]
fn graph_json() {
    snapshot_json("graph_json", GRAPH);
}

// ---------------------------------------------------------------------------
// context / handoff / export
// ---------------------------------------------------------------------------

/// `context` and `handoff` are what an agent reads at session start — the two
/// outputs most likely to be regenerated by a rewritten query layer, and the
/// ones whose drift would be least visible in ordinary use.
const NARRATIVE: &[&[&str]] = &[
    &["context"],
    &["context", "--format", "json"],
    &["load-context"],
    &["handoff"],
    &["handoff", "--since", "7d"],
    &["handoff", "--since", "2h"],
];

#[test]
fn narrative_human() {
    snapshot_human("narrative_human", NARRATIVE);
}

#[test]
fn narrative_json() {
    snapshot_json("narrative_json", NARRATIVE);
}

/// The whole event log, every member and every config table in one document —
/// the densest surface in the CLI and the one W3's importer has to reproduce.
///
/// Snapshotted as JSON rather than text: `export` prints raw JSON (no envelope),
/// so parsing it lets the timestamps be redacted by path instead of by regex,
/// and the redaction reaches every `at` in every event without knowing the event
/// variants.
#[test]
fn export_document() {
    let raw = stdout_of(&["export"]);
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).expect("`story export` must print JSON");
    settings().bind(|| assert_json_golden!("export_document", parsed));
}

/// `export --json` wraps the same document as an escaped string inside the
/// standard envelope. Snapshotted separately because the *wrapping* is the
/// contract here, not the payload.
#[test]
fn export_envelope_shape() {
    let raw = stdout_of(&["export", "--json"]);
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).expect("`story export --json` must print JSON");
    let shape = serde_json::json!({
        "keys": parsed.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()),
        "result": parsed.get("result"),
        "message_is_a_string": parsed.get("message").map(|m| m.is_string()),
        "message_parses_as_the_export_document":
            parsed.get("message").and_then(|m| m.as_str())
                .map(|s| serde_json::from_str::<serde_json::Value>(s).is_ok()),
    });
    settings().bind(|| assert_json_golden!("export_envelope_shape", shape));
}

// ---------------------------------------------------------------------------
// doctor / phase / config listings / epic
// ---------------------------------------------------------------------------

/// The corpus fixture is deliberately clean, so this pins the *healthy* answer.
/// `doctor` on a corrupt project raises `AppError::Integrity` instead of
/// returning a response; that path is pinned in `tests/error_contract.rs`.
const DOCTOR: &[&[&str]] = &[&["doctor"]];

#[test]
fn doctor_human() {
    snapshot_human("doctor_human", DOCTOR);
}

#[test]
fn doctor_json() {
    snapshot_json("doctor_json", DOCTOR);
}

const PHASE: &[&[&str]] = &[
    &["phase", "list"],
    &["phase", "show", "1"],
    &["phase", "show", "2"],
    &["phase", "show", "9"],
];

#[test]
fn phase_human() {
    snapshot_human("phase_human", PHASE);
}

#[test]
fn phase_json() {
    snapshot_json("phase_json", PHASE);
}

/// The state and type tables, including the custom entries the fixture adds and
/// the per-state open/archived counts.
const CONFIG: &[&[&str]] = &[&["state", "list"], &["type", "list"]];

#[test]
fn config_human() {
    snapshot_human("config_human", CONFIG);
}

#[test]
fn config_json() {
    snapshot_json("config_json", CONFIG);
}

const EPIC: &[&[&str]] = &[&["epic", "list"], &["epic", "show", "SH-1"]];

#[test]
fn epic_human() {
    snapshot_human("epic_human", EPIC);
}

#[test]
fn epic_json() {
    snapshot_json("epic_json", EPIC);
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// At least one failing invocation per read-surface verb: a bad flag, a missing
/// argument, an unknown id, an out-of-range value.
///
/// Snapshotted in both forms because the two differ in *stream* as well as in
/// body — the human error goes to stderr and leaves stdout empty; the `--json`
/// envelope goes to stdout and leaves stderr empty.
const ERRORS: &[&[&str]] = &[
    &["no-such-verb"],
    &["list", "--no-such-flag"],
    &["list", "--state"],
    &["show"],
    &["show", "SH-999"],
    &["show", "SH-1", "extra"],
    &["next", "--count", "0"],
    &["next", "--count", "many"],
    &["search"],
    &["graph", "--blocked-by"],
    &["graph", "--no-such-mode"],
    &["report", "--no-such-flag"],
    &["context", "--format"],
    &["handoff", "--since"],
    // Seconds are not a supported unit — only h/d/w.
    &["handoff", "--since", "1s"],
    &["list", "--stale", "1s"],
    &["doctor", "--no-such-flag"],
    &["phase", "show", "0"],
    &["phase", "show", "not-a-number"],
    &["phase", "no-such-subcommand"],
    &["type", "no-such-subcommand"],
    &["state", "no-such-subcommand"],
    &["epic", "show", "SH-999"],
    &["member", "no-such-subcommand"],
];

#[test]
fn errors_human() {
    snapshot_failures("errors_human", ERRORS, false);
}

#[test]
fn errors_json() {
    snapshot_failures("errors_json", ERRORS, true);
}
