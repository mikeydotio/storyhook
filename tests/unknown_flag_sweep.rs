//! A verb refuses a flag it does not declare — for every verb, without
//! exception (SH-62).
//!
//! `story new --typo x` created a story titled `--typo x`, exited 0, and said
//! nothing. Measured against a real binary before the fix, **eight** verbs
//! accepted a flag-shaped token as data and exited 0. Four of them minted a
//! durable object nobody asked for:
//!
//! | invocation | what it created |
//! |---|---|
//! | `story new --typo x` | a story titled `--typo x` |
//! | `story epic create --typo` | an epic titled `--typo` |
//! | `story type add --typo` | a permanent story type, in every `--type` list |
//! | `story member add --typo` | a member named `typo` |
//!
//! and four attached junk to a write the user did ask for: `comment`'s body,
//! `block`'s reason, `delete`'s audit reason, and a literal `--typo` label.
//!
//! This is SH-52's defect one wave later. That one was `--help` specifically,
//! answered ahead of every verb parser; this is the rest of the flag space. The
//! sweep is table-driven over the whole verb list for the same reason its
//! sibling is: the defect regresses one verb at a time, and a new
//! positional-taking verb inherits it unless the rule lives ahead of them all.

use assert_cmd::Command;

use storyhook_test_support::{Project, TestEnv};

/// A `story` command in this fixture, carrying its isolated environment.
fn story(project: &Project<'_>) -> Command {
    project.story()
}

/// A project with one story, so an accidental write is visible.
fn project() -> Project<'static> {
    TestEnv::shared()
        .project()
        .seed_story("A real story")
        .build()
}

/// The eight verbs measured writing junk silently, plus the shapes that share
/// their parser. Each is invoked with a flag no verb declares.
const SWALLOWERS: &[&[&str]] = &[
    &["new", "--zzz-not-a-flag", "x"],
    &["epic", "create", "--zzz-not-a-flag"],
    &["type", "add", "--zzz-not-a-flag"],
    &["member", "add", "--zzz-not-a-flag"],
    &["comment", "SH-1", "--zzz-not-a-flag"],
    &["block", "SH-1", "--zzz-not-a-flag"],
    &["delete", "SH-1", "--zzz-not-a-flag"],
    &["label", "SH-1", "--zzz-not-a-flag"],
    &["unlabel", "SH-1", "--zzz-not-a-flag"],
    &["search", "--zzz-not-a-flag"],
    &["assign", "SH-1", "--zzz-not-a-flag"],
    &["move", "SH-1", "--zzz-not-a-flag"],
    &["show", "--zzz-not-a-flag"],
    &["prioritize", "SH-1", "--zzz-not-a-flag"],
    &["relate", "SH-1", "blocks", "--zzz-not-a-flag"],
    &["reopen", "--zzz-not-a-flag"],
    &["archive", "SH-1", "--zzz-not-a-flag"],
    &["unarchive", "SH-1", "--zzz-not-a-flag"],
    &["publish", "SH-1", "--zzz-not-a-flag"],
    &["archive-state", "done", "--zzz-not-a-flag"],
    &["unblock", "--zzz-not-a-flag"],
    &["import", "--zzz-not-a-flag"],
    &["import-project", "--zzz-not-a-flag"],
    &["hooks", "test", "--zzz-not-a-flag"],
    &["plugin", "install", "--zzz-not-a-flag"],
    &["project", "new", "--zzz-not-a-flag"],
    // `link` and `unlink` declare an *empty* flag list, which is a different
    // statement from having no entry: it says the verb takes no flags, and
    // these two rows are what proves the statement is enforced.
    &["project", "link", "origin", "--zzz-not-a-flag"],
    &["project", "unlink", "origin", "--zzz-not-a-flag"],
    &["project", "settings", "get", "--zzz-not-a-flag"],
    &["state", "remove", "--zzz-not-a-flag"],
    &["phase", "remove", "--zzz-not-a-flag"],
    &["graph", "--zzz-not-a-flag"],
    &["list", "--zzz-not-a-flag"],
    &["set", "SH-1", "--zzz-not-a-flag"],
];

/// Every verb refuses an undeclared flag, and says which token it refused.
///
/// Asserting on the *message* rather than only on a non-zero exit is what makes
/// this test worth running: more than half these verbs already exited non-zero
/// before the fix, because the parser swallowed `--zzz-not-a-flag` as an id and
/// the *domain* layer said "story not found". An exit-code-only assertion would
/// have passed identically before and after and proved nothing landed.
#[test]
fn every_verb_refuses_a_flag_it_does_not_declare() {
    let dir = project();

    for invocation in SWALLOWERS {
        let out = story(&dir).args(*invocation).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let rendered = format!("story {}", invocation.join(" "));

        assert_eq!(
            out.status.code(),
            Some(2),
            "`{rendered}` must exit 2 (usage); stdout: {}, stderr: {stderr}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(
            stderr.contains("unknown flag") && stderr.contains("--zzz-not-a-flag"),
            "`{rendered}` must name the flag it refused, not fail downstream: {stderr}"
        );
    }
}

/// Nothing the sweep ran created, deleted, or edited anything.
///
/// The reported defect was a *silent write*, so a refusal that still wrote
/// would be no fix at all.
#[test]
fn a_refused_flag_writes_nothing() {
    let dir = project();

    let before = story(&dir).args(["export"]).output().unwrap().stdout;
    for invocation in SWALLOWERS {
        story(&dir).args(*invocation).output().unwrap();
    }
    let after = story(&dir).args(["export"]).output().unwrap().stdout;

    assert_eq!(
        String::from_utf8_lossy(&before),
        String::from_utf8_lossy(&after),
        "a refused invocation must not change the project"
    );
}

/// The export fingerprint is not vacuous.
///
/// `a_refused_flag_writes_nothing` compares two exports; if `export` answered
/// the same bytes regardless, it would pass forever while testing nothing —
/// the trap `the_fingerprint_notices_a_real_change` documents in the sibling
/// sweep.
#[test]
fn the_export_fingerprint_notices_a_real_change() {
    let dir = project();

    let before = story(&dir).args(["export"]).output().unwrap().stdout;
    story(&dir)
        .args(["new", "A story the sweep did not ask for"])
        .assert()
        .success();
    let after = story(&dir).args(["export"]).output().unwrap().stdout;

    assert_ne!(
        String::from_utf8_lossy(&before),
        String::from_utf8_lossy(&after),
        "creating a story must move the fingerprint this sweep relies on"
    );
}

/// The reported case, asserted precisely.
#[test]
fn new_refuses_a_typo_instead_of_titling_a_story_with_it() {
    let dir = project();

    let out = story(&dir).args(["new", "--typo", "x"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    assert_eq!(out.status.code(), Some(2), "must refuse: {stderr}");
    assert!(
        stderr.contains("--priority"),
        "the refusal lists the flags `new` really takes: {stderr}"
    );

    let listed =
        String::from_utf8_lossy(&story(&dir).args(["list"]).output().unwrap().stdout).into_owned();
    assert!(
        !listed.contains("--typo"),
        "no story titled `--typo` may exist: {listed}"
    );
}

/// Constraint (a), pinned: quoted free text beginning with dashes is data.
///
/// This is the guarantee `src/cli.rs`'s `parse_move` comment protects, and the
/// reason the rule tests *shape* rather than *prefix* — a real phrase is one
/// argv token containing a space, so it can never be mistaken for a flag. If
/// this test ever fails, the fix went too far.
#[test]
fn quoted_free_text_beginning_with_dashes_is_still_data() {
    let dir = project();

    story(&dir)
        .args(["new", "--fix the ingest path"])
        .assert()
        .success();
    story(&dir)
        .args(["comment", "SH-1", "--fix the ingest path"])
        .assert()
        .success();
    story(&dir)
        .args(["block", "SH-1", "--waiting on infra"])
        .assert()
        .success();

    let listed =
        String::from_utf8_lossy(&story(&dir).args(["list"]).output().unwrap().stdout).into_owned();
    assert!(
        listed.contains("--fix the ingest path"),
        "the title survived verbatim: {listed}"
    );
}

/// `--` makes a dash-leading value expressible, which is what keeps the
/// refusal honest rather than merely strict.
#[test]
fn a_terminator_makes_everything_after_it_data() {
    let dir = project();

    story(&dir)
        .args(["new", "--", "--typo", "x"])
        .assert()
        .success();

    let listed =
        String::from_utf8_lossy(&story(&dir).args(["list"]).output().unwrap().stdout).into_owned();
    assert!(
        listed.contains("--typo x"),
        "`story new -- --typo x` must create that exact title: {listed}"
    );
}

/// The terminator stops the *global* flag scan too.
///
/// Before this, `--json` was harvested from any position, so a comment naming
/// it lost the word and flipped stdout to a machine envelope. An escape hatch
/// that leaks is worse than none.
#[test]
fn a_terminator_stops_the_global_flag_scan() {
    let dir = project();

    let out = story(&dir)
        .args(["comment", "SH-1", "--", "--json is great"])
        .output()
        .unwrap();
    assert!(out.status.success(), "the comment must land");

    let shown =
        String::from_utf8_lossy(&story(&dir).args(["show", "SH-1"]).output().unwrap().stdout)
            .into_owned();
    assert!(
        shown.contains("--json is great"),
        "the text survived the global scan: {shown}"
    );
}

/// A help request outranks a flag complaint, and an unknown *command* is still
/// reported as one.
#[test]
fn the_gate_yields_to_help_and_to_unknown_commands() {
    let dir = project();

    story(&dir).args(["new", "--help"]).assert().success();
    story(&dir)
        .args(["new", "--typo", "--help"])
        .assert()
        .success();

    let out = story(&dir).args(["frobnicate", "--typo"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stderr.contains("unknown command"),
        "a mistyped verb reports the command, not its flags: {stderr}"
    );
}

/// Every flag the table declares is actually accepted by its verb.
///
/// This is the drift guard that matters. The table is a second source of truth
/// about flags, and the direction that fails *silently* is a declared flag that
/// no longer exists — the gate would wave it through and the parser would
/// reject it with a usage error nobody expected.
///
/// The reverse direction — scraping each verb's help text to find flags the
/// table forgot — was tried and rejected as an oracle: help topics are prose
/// that mention sibling flags (`graph`'s topic names `--ready` and `--blocked`,
/// which belong to `list`), and two verbs name no flags at all. Fail-closed
/// covers that direction instead: a forgotten flag is refused loudly the first
/// time anyone uses it.
#[test]
fn every_declared_flag_is_accepted_by_its_verb() {
    let dir = project();

    // (verb path, flag, a value if the flag takes one)
    let declared: &[(&[&str], &str, Option<&str>)] = &[
        (&["new"], "--state", Some("todo")),
        (&["new"], "--type", Some("bug")),
        (&["new"], "--description", Some("text")),
        (&["new"], "--priority", Some("high")),
        (&["new"], "--label", Some("tag")),
        (&["new"], "--labels", Some("a,b")),
        (&["new"], "--draft", None),
        (&["list"], "--ready", None),
        (&["list"], "--blocked", None),
        (&["list"], "--flagged", None),
        (&["list"], "--drafts", None),
        (&["list"], "--state", Some("todo")),
        (&["list"], "--stale", Some("7d")),
        (&["list"], "--include-closed", None),
        (&["list"], "--include-archived", None),
        (&["list"], "--all", None),
        (&["next"], "--count", Some("2")),
        (&["claim"], "--next", None),
        (&["claim", "--next"], "--phase", Some("1")),
        (&["claim", "SH-1"], "--comment", Some("text")),
        (&["claim", "SH-1"], "--no-comment", None),
        (&["claim", "SH-1"], "--dry-run", None),
        (&["set", "SH-1"], "--title", Some("New")),
        (&["set", "SH-1"], "--unblocked", None),
        (&["move", "SH-1", "todo"], "--if-state", Some("todo")),
        (&["reopen", "SH-1"], "--force", None),
        (&["report"], "--html", None),
        (&["doctor"], "--fix", None),
        (&["handoff"], "--since", Some("1d")),
        (&["commit-sync"], "--since", Some("1d")),
        (&["graph"], "--critical-path", None),
        (&["graph"], "--parallel-groups", None),
        (&["graph"], "--blocked-by", Some("SH-1")),
        (&["load-context"], "--format", Some("json")),
        (&["help"], "--compact", None),
        (&["help"], "--all", None),
        (&["state", "add", "review"], "--super", Some("OPEN")),
        (&["state", "set", "todo"], "--no-description", None),
        (
            &["state", "remove", "review"],
            "--move-stories-to",
            Some("todo"),
        ),
        (&["type", "add", "spike"], "--description", Some("text")),
        (&["member", "add"], "--github", Some("octocat")),
        (&["project", "new"], "--prefix", Some("ZZ")),
        (&["project", "new"], "--name", Some("declared")),
        (&["project", "new"], "--attach", Some(".")),
        (&["project", "new"], "--no-attach", None),
        (&["project", "new"], "--no-agents-md", None),
        (&["store", "backup"], "--label", Some("pre-migration")),
    ];

    for (path, flag, value) in declared {
        let mut args: Vec<&str> = path.to_vec();
        args.push(flag);
        if let Some(value) = value {
            args.push(value);
        }
        let out = story(&dir).args(&args).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            !stderr.contains("unknown flag"),
            "`story {}` must not be refused by the gate — the table declares {flag} \
             but the verb no longer accepts it: {stderr}",
            args.join(" ")
        );
    }
}

/// **A script that still passes `--local` fails, loudly, rather than being
/// ignored.**
///
/// The flag was deleted in SH-114, and a deleted global flag has two possible
/// afterlives. It could be accepted and ignored, which is the shape this whole
/// file exists to prevent: a hook or a CI job would go on passing it and go on
/// appearing to run in its own process while starting a daemon on every
/// invocation. Or it can be refused by name, which is what SH-62's gate does to
/// any flag-shaped token a verb does not declare — and it needed no special case
/// to do it, which is the point of a fail-closed rule.
///
/// Both positions are checked because they are answered by different code. After
/// a verb it is the gate; before one it is the verb parser, which reads a
/// flag-shaped first token as a command name. Neither is silent, and both name
/// the token.
#[test]
fn the_deleted_local_flag_is_refused_rather_than_ignored() {
    let project = project();

    for args in [
        vec!["list", "--local"],
        vec!["--local", "list"],
        vec!["new", "--local", "a story nobody asked for"],
    ] {
        let out = project.story().args(&args).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(
            !out.status.success(),
            "`story {}` must fail: a flag that no longer exists and is quietly \
             accepted is a script that believes something untrue about where its \
             command ran",
            args.join(" ")
        );
        assert!(
            stderr.contains("--local"),
            "`story {}` must name the token it refused: {stderr}",
            args.join(" ")
        );
    }

    // And nothing was written by the one that would have written something.
    let listed = project.json(&["list"]).to_string();
    assert!(
        !listed.contains("a story nobody asked for"),
        "a refused command must write nothing: {listed}"
    );
}
