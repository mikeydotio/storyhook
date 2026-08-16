//! Bare integers as story ids (SH-118, C6 of the server-owned epic SH-112).
//!
//! Once the project is determined, `5` means the same story `SH-5` does. The
//! prefix survives **only** inside a canonical id — it is a rendering concern,
//! not a handle — and a canonical id naming a *different* prefix is a
//! contradiction rather than a missing story.
//!
//! # Where the rule lives, and why that is the whole design
//!
//! One exhaustive pass over [`Invocation`](storyhook::cli::Invocation) runs
//! before any dispatch arm does, so every verb inherits the rule and the 25th
//! id position is a compile error rather than a bug report. The alternative the
//! council rejected — expanding inside the resolution funnels — is unsafe for a
//! measured reason: `src/service/relation.rs` resolves both ids and then
//! compares them with a **raw string** `if a == b`, and persists `other_id` as
//! the raw string into an append-only event log. `story relate SH-1 blocks 1`
//! would have passed the self-relation guard and written `"1"` into history.
//!
//! Full verdict: recorded as a comment on SH-118.

use storyhook_test_support::TestEnv;

/// Every CLI verb that takes a story id and can be asked about a live story.
///
/// A table rather than a test each, because the point of AC-1 is that the set
/// is closed: a verb missing from here is a verb the rule was never asked
/// about. `block` and `unblock` are how `SetAwaiting` and `ClearAwaiting` are
/// spelled at the CLI; `delete`, `reopen` and `purge` need a story in a
/// particular state and have their own test below; `github-sync` needs a GitHub
/// token and is covered at the parser, which is the only place it was ever
/// broken.
fn id_verbs() -> Vec<Vec<&'static str>> {
    vec![
        vec!["show", "1"],
        vec!["comment", "1", "a comment"],
        vec!["assign", "1", "tester"],
        vec!["move", "1", "in-progress"],
        vec!["prioritize", "1", "high"],
        vec!["label", "1", "alpha"],
        vec!["unlabel", "1", "alpha"],
        vec!["block", "1", "waiting on something"],
        vec!["unblock", "1"],
        vec!["set", "1", "--title", "retitled"],
        vec!["phase", "add", "1", "3"],
        vec!["phase", "remove", "1"],
        vec!["epic", "show", "1"],
        vec!["graph", "--blocked-by", "1"],
    ]
}

/// **AC-1.** Every verb taking an id accepts the bare integer form.
///
/// Each verb is run against a story that exists, and the assertion is that it
/// *succeeded* — before this story every one of them exited 3 with
/// ``story `1` not found``.
#[test]
fn every_id_verb_accepts_a_bare_integer() {
    let env = TestEnv::isolated();
    let project = env.project().seed_story("first").build();
    project.new_story("second");
    project
        .run(&["member", "add", "Tester <tester@example.com>"])
        .success();

    for argv in id_verbs() {
        let out = project
            .story()
            .args(&argv)
            .output()
            .expect("running the verb");
        assert!(
            out.status.success(),
            "`story {}` must accept a bare integer; exit {:?}, stderr:\n{}",
            argv.join(" "),
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// **AC-1**, the three verbs that need a story in a particular state.
///
/// Run as one sequence because that is the only way to reach each state: a
/// story must be deleted before it can be reopened or purged. Every id in the
/// sequence is bare, so a verb that quietly failed would break the next one.
#[test]
fn the_destructive_verbs_accept_a_bare_integer_too() {
    let env = TestEnv::isolated();
    let project = env.project().seed_story("first").build();
    project.new_story("second");

    project.run(&["delete", "2", "first pass"]).success();
    project.run(&["reopen", "2", "--force"]).success();
    project.run(&["delete", "2", "second pass"]).success();
    project.run(&["purge", "2", "--force"]).success();

    project
        .story()
        .args(["show", "2"])
        .assert()
        .failure()
        .code(3);
}

/// **AC-1**, the two-id verbs: a mixed list is accepted, each id independently.
#[test]
fn a_command_taking_two_ids_accepts_a_mixed_pair() {
    let env = TestEnv::isolated();
    let project = env.project().seed_story("first").build();
    project.new_story("second");

    project.run(&["relate", "1", "blocks", "SH-2"]).success();
    let view = project.json(&["show", "SH-1"]);
    let relations = view["story"]["story"]["relationships"]
        .as_array()
        .expect("relationships");
    assert!(
        relations
            .iter()
            .any(|r| r["relation"] == "blocks" && r["other_id"] == "SH-2"),
        "a mixed pair must relate the same two stories a canonical pair does: {relations:?}"
    );

    project.run(&["unrelate", "SH-1", "blocks", "2"]).success();
    let view = project.json(&["show", "SH-1"]);
    assert!(
        view["story"]["story"]["relationships"]
            .as_array()
            .expect("relationships")
            .iter()
            .all(|r| r["relation"] != "blocks"),
        "the mirrored pair must remove the edge the mixed pair added"
    );
}

/// **AC-2.** `SH-5` and `5` are the same story under an explicit `--project`.
///
/// The assertion is on the *state the write left behind*, not on the exit code:
/// two commands that both succeed while touching different rows would pass a
/// weaker test.
#[test]
fn the_bare_form_and_the_canonical_form_are_one_story() {
    let env = TestEnv::isolated();
    let project = env.project().seed_story("first").build();
    let slug = project.slug();

    project
        .story()
        .args(["--project", &slug, "comment", "1", "typed bare"])
        .assert()
        .success();
    project
        .story()
        .args(["--project", &slug, "comment", "SH-1", "typed canonical"])
        .assert()
        .success();

    let view = project.json(&["--project", &slug, "show", "1"]);
    let comments = view["story"]["story"]["comments"]
        .as_array()
        .expect("comments");
    let bodies: Vec<&str> = comments.iter().filter_map(|c| c["text"].as_str()).collect();
    assert!(
        bodies.contains(&"typed bare") && bodies.contains(&"typed canonical"),
        "both forms must have written to one story: {bodies:?}"
    );
}

/// **AC-3.** A bare integer with no project determined gets the *selection*
/// refusal — not a separate "no such story".
///
/// This pins an **ordering** rather than an implementation: project resolution
/// returns before anything looks at an id, so the two forms are answered
/// identically. A design that expanded ids first would answer this one
/// "story `1` not found", which sends the reader looking for a story instead of
/// for a project.
#[test]
fn a_bare_integer_with_no_project_gets_the_selection_refusal() {
    let env = TestEnv::shared();
    let nowhere = storyhook_test_support::scratch_dir_named("sh118-nowhere-");

    for id in ["1", "SH-1"] {
        let out = env
            .story(nowhere.path())
            .args(["show", id])
            .output()
            .expect("running story");
        assert_eq!(
            out.status.code(),
            Some(3),
            "`story show {id}` with no project must refuse as a not-found"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("cannot tell which project this is"),
            "`story show {id}` must get the selection refusal, not a story lookup; got:\n{stderr}"
        );
        assert!(
            !stderr.contains("not found\n") || stderr.contains("--project"),
            "and it must name the way out; got:\n{stderr}"
        );
    }
}

/// **AC-4.** A canonical id whose prefix belongs to another project is refused,
/// naming both — and the refusal is a *validation* error, because the story is
/// not missing, the command is wrong.
#[test]
fn a_foreign_canonical_id_is_refused_naming_both() {
    let env = TestEnv::isolated();
    let here = env.project().prefix("SH").seed_story("ours").build();
    let there = env.project().prefix("OTH").seed_story("theirs").build();
    let there_slug = there.slug();

    let out = here
        .story()
        .args(["show", "OTH-1"])
        .output()
        .expect("running story");
    assert_eq!(
        out.status.code(),
        Some(2),
        "a contradicted id is a wrong command, not a missing story"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    for needle in ["OTH-1", &there_slug, &here.slug()] {
        assert!(
            stderr.contains(needle),
            "the refusal must name `{needle}`; got:\n{stderr}"
        );
    }
    assert!(
        stderr.contains("--project"),
        "and it must name the way out; got:\n{stderr}"
    );
}

/// **AC-4**, the case the story did not consider: the prefix belongs to nobody.
///
/// There is no second project to name, so the refusal says so and points at the
/// listing — rather than inventing a project or falling back to "not found",
/// which would tell the reader the story might exist somewhere.
#[test]
fn a_prefix_no_project_claims_is_refused_and_says_so() {
    let env = TestEnv::isolated();
    let here = env.project().prefix("SH").seed_story("ours").build();

    let out = here
        .story()
        .args(["show", "ZZZ-1"])
        .output()
        .expect("running story");
    assert_eq!(out.status.code(), Some(2), "still a wrong command");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("ZZZ") && stderr.contains("story project list"),
        "an unclaimed prefix must say nothing claims it and point at the listing; got:\n{stderr}"
    );
}

/// **AC-4**, the case the schema permits and nothing prevents: *two* projects
/// share the foreign prefix.
///
/// `projects.prefix` carries no unique index, and the store's own conformance
/// suite seeds three projects with prefix `SH` — so a design that assumed a
/// prefix identifies a project would fail that suite. The refusal lists every
/// claimant and refuses to choose; it never picks one.
#[test]
fn a_foreign_prefix_with_two_claimants_names_them_all() {
    let env = TestEnv::isolated();
    let here = env.project().prefix("SH").seed_story("ours").build();
    let one = env.project().prefix("DUP").build();
    let two = env.project().prefix("DUP").build();

    let out = here
        .story()
        .args(["show", "DUP-1"])
        .output()
        .expect("running story");
    assert_eq!(out.status.code(), Some(2), "still a wrong command");
    let stderr = String::from_utf8_lossy(&out.stderr);
    for slug in [one.slug(), two.slug()] {
        assert!(
            stderr.contains(&slug),
            "every claimant must be named, missing `{slug}`; got:\n{stderr}"
        );
    }
}

/// The selection is binding: a foreign id never silently switches project.
///
/// The outcome this keeps unrepresentable is `story delete OTH-5` typed in the
/// wrong shell deleting in the other project. Asserted on the *store*, not on
/// the message: the other project's story must be untouched.
#[test]
fn a_foreign_id_never_switches_project() {
    let env = TestEnv::isolated();
    let here = env.project().prefix("SH").seed_story("ours").build();
    let there = env.project().prefix("OTH").seed_story("theirs").build();

    here.story()
        .args(["delete", "OTH-1", "should not happen"])
        .assert()
        .failure();

    let view = there.json(&["show", "OTH-1"]);
    assert_eq!(
        view["story"]["story"]["state"], "todo",
        "the other project's story must be exactly as it was"
    );
}

/// A token that is neither a bare integer nor a canonical id is unchanged: it
/// is still not found, and the message still echoes what was typed.
///
/// This is the too-far guard for the *grammar*. `007` and `1x` are the shapes a
/// looser rule would have swallowed, and `-1` is the one that reaches an id
/// position at all — SH-62's gate refuses flag-*shaped* tokens, and a single
/// leading dash with a digit is not one.
#[test]
fn a_token_that_is_not_an_id_is_unchanged() {
    let env = TestEnv::isolated();
    let project = env.project().seed_story("first").build();

    for token in ["0", "007", "-1", "1x", "nonsense"] {
        let out = project
            .story()
            .args(["show", token])
            .output()
            .expect("running story");
        assert_eq!(
            out.status.code(),
            Some(3),
            "`story show {token}` must still be a not-found"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(&format!("`{token}`")),
            "and it must echo what was typed, since there is no canonical form to quote; \
             got:\n{stderr}"
        );
    }
}

/// The id storyhook *speaks* is the canonical one, whatever was typed.
///
/// `1` is meaningless the moment it leaves the shell that chose the project, so
/// a message, an event or a hook payload carrying it would be a record nobody
/// downstream can resolve. The exception is the two refusals with no canonical
/// form to quote, pinned by the test above.
#[test]
fn the_id_reported_back_is_canonical() {
    let env = TestEnv::isolated();
    let project = env.project().seed_story("first").build();
    project.new_story("second");

    let out = project
        .story()
        .args(["delete", "2", "tidying up"])
        .output()
        .expect("running story");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("SH-2"),
        "a message about a story typed as `2` must name SH-2: {stdout}"
    );

    let missing = project
        .story()
        .args(["show", "999"])
        .output()
        .expect("running story");
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(
        stderr.contains("SH-999"),
        "a bare id that resolves to nothing must report what it resolved to, which is what \
         teaches the mapping; got:\n{stderr}"
    );
}

/// The too-far guard: expanding an id must not touch the argument beside it.
///
/// Three verbs whose *other* positional is also a short string — a state slug,
/// a relation name, and a phase number that is itself a bare integer. The phase
/// one is the sharp case: `story phase add 1 3` has a bare integer in both
/// positions and only the first is an id.
#[test]
fn the_argument_beside_an_id_is_left_alone() {
    let env = TestEnv::isolated();
    let project = env.project().seed_story("first").build();
    project.new_story("second");

    project.run(&["move", "1", "in-progress"]).success();
    let view = project.json(&["show", "1"]);
    assert_eq!(
        view["story"]["story"]["state"], "in-progress",
        "the state slug must survive the id expansion"
    );

    project.run(&["phase", "add", "1", "3"]).success();
    let listed = project
        .story()
        .args(["list", "--phase", "3"])
        .output()
        .expect("running story list --phase");
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("SH-1"),
        "the phase number must stay a phase number, so the story lands in phase 3"
    );

    project.run(&["relate", "1", "blocks", "2"]).success();
    let view = project.json(&["show", "1"]);
    assert!(
        view["story"]["story"]["relationships"]
            .as_array()
            .expect("relationships")
            .iter()
            .any(|r| r["relation"] == "blocks"),
        "the relation name must survive the id expansion"
    );
}
