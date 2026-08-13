//! `GitService` — everything `story commit-sync` owes, over a real git
//! repository.
//!
//! This file used to cover only the store-side properties the legacy-versus-
//! store differential harness could not see: that comment and transition are
//! one transaction, that `sync.auto_transition` is honoured, and that no event
//! hooks fire. The behavioural rows — what a commit naming a closed story does,
//! what a second run over the same window adds, which stories a multi-story
//! commit touches — lived in `tests/differential_git.rs`, asserted as *agreement
//! between two implementations*.
//!
//! There is one implementation now, so those rows were carried across and
//! restated as claims about behaviour rather than about agreement. They are
//! here rather than gone because the differential harness's subject was never
//! really "do the legs agree" — it was "does `commit-sync` do the right thing",
//! and that question outlives the leg it was asked of.

use std::process::Command;

use storyhook::domain::{CommitReference, StoryEvent, StorySnapshot};
use storyhook::error::AppError;
use storyhook::service::{Clock, GitService, NewStoryInput, StoryService};
use storyhook::store::{ProjectSettings, ReadOps, Store, StoryNo, WriteOps, partition_known};
use storyhook_test_support::{ServiceFixture, default_states};

/// Turns the fixture's working directory into a git repository.
fn git_init(fixture: &ServiceFixture) {
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
    ] {
        run_git(fixture, &args);
    }
}

fn commit(fixture: &ServiceFixture, subject: &str) {
    run_git(fixture, &["commit", "-q", "--allow-empty", "-m", subject]);
}

/// [`commit`], with a body after the subject — `git commit -m … -m …`, which
/// is how Conventional Commits puts a `Closes SH-N` trailer in a message.
fn commit_with_body(fixture: &ServiceFixture, subject: &str, body: &str) {
    run_git(
        fixture,
        &["commit", "-q", "--allow-empty", "-m", subject, "-m", body],
    );
}

/// [`commit`], with the author and committer dates pinned.
///
/// An explicit timestamp, never a relative expression: `GIT_AUTHOR_DATE` does
/// not accept `1 hour ago` — only `--date` does — and it fails with `fatal:
/// invalid date format` rather than falling back to now.
fn commit_at(fixture: &ServiceFixture, subject: &str, date: &str) {
    let output = Command::new("git")
        .current_dir(fixture.cwd())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .args(["commit", "-q", "--allow-empty", "-m", subject])
        .output()
        .expect("running git commit");
    assert!(
        output.status.success(),
        "`git commit -m {subject}` failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The full hash of `HEAD`, as a link record stores it.
fn head_full_hash(fixture: &ServiceFixture) -> String {
    let output = Command::new("git")
        .current_dir(fixture.cwd())
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("running git rev-parse");
    assert!(output.status.success(), "`git rev-parse HEAD` failed");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// The abbreviated hash of `HEAD`, as `commit-sync` renders it into a comment.
fn head_short_hash(fixture: &ServiceFixture) -> String {
    head_full_hash(fixture)[..7].to_string()
}

fn run_git(fixture: &ServiceFixture, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(fixture.cwd())
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .output()
        .expect("running git");
    assert!(
        output.status.success(),
        "`git {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn create(fixture: &ServiceFixture, title: &str) -> String {
    StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: title.to_string(),
            ..NewStoryInput::default()
        })
        .expect("creating a story")
        .id
}

fn sync(fixture: &ServiceFixture) -> Result<String, AppError> {
    GitService::new(&fixture.ctx()).commit_sync(None)
}

fn events_of(fixture: &ServiceFixture, story: StoryNo) -> Vec<StoryEvent> {
    fixture
        .store()
        .read(|tx| {
            let stored = tx.events_for(fixture.project(), story)?;
            Ok(partition_known(story, &stored).0)
        })
        .expect("reading events")
}

/// The folded story, as `story show` would render it.
fn snapshot_of(fixture: &ServiceFixture, story: StoryNo) -> StorySnapshot {
    fixture
        .store()
        .read(|tx| {
            tx.story(fixture.project(), story)?
                .map(|row| row.snapshot)
                .ok_or_else(|| {
                    storyhook::store::StoreError::NotFound(format!(
                        "story {story} is not in the read model"
                    ))
                })
        })
        .expect("reading the story")
}

/// Every comment on a story, in order, as text.
fn comments_of(fixture: &ServiceFixture, story: StoryNo) -> Vec<String> {
    snapshot_of(fixture, story)
        .comments
        .into_iter()
        .map(|comment| comment.text)
        .collect()
}

/// Every commit `commit-sync` has linked to a story, in order (SH-169) —
/// what `comments_of` answered before `fold_story` stopped rendering a link
/// as a comment.
fn referenced_by_commits_of(fixture: &ServiceFixture, story: StoryNo) -> Vec<CommitReference> {
    snapshot_of(fixture, story).referenced_by_commits
}

#[test]
fn outside_a_git_repository_the_answer_is_a_validation_error() {
    let fixture = ServiceFixture::new();
    let error = sync(&fixture).expect_err("not a repository");
    assert!(matches!(error, AppError::Validation(_)), "{error}");
    assert_eq!(error.to_string(), "not a git repository");
}

#[test]
fn an_invalid_window_names_the_duration_it_could_not_parse() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let error = GitService::new(&fixture.ctx())
        .commit_sync(Some("a fortnight"))
        .expect_err("an unparseable duration");
    assert!(
        error.to_string().contains("invalid duration `a fortnight`"),
        "{error}"
    );
}

#[test]
fn the_comment_and_the_transition_are_one_atomic_batch() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Referenced");
    // A claim, not a bare mention: only a claim moves a story (SH-124), and
    // this test is about the move landing in the same batch as the comment.
    commit(&fixture, &format!("feat: closes {id}"));
    sync(&fixture).expect("syncing");

    let events = events_of(&fixture, StoryNo::new(1));
    assert!(
        matches!(events[1], StoryEvent::StoryCommitLinked { .. }),
        "the link record comes first: {events:?}"
    );
    assert!(
        matches!(events[2], StoryEvent::StoryStateChanged { ref state, .. } if state == "in-progress"),
        "the move follows it in the same batch: {events:?}"
    );
    fixture.assert_no_drift();
}

#[test]
fn the_project_setting_can_turn_the_transition_off() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Referenced");
    fixture
        .store()
        .write(|tx| {
            tx.put_settings(
                fixture.project(),
                &ProjectSettings {
                    sync_auto_transition: Some(false),
                    ..ProjectSettings::default()
                },
            )
        })
        .expect("writing settings");
    // A commit that *would* have claimed. Without the claim word this test
    // would pass for the wrong reason — the grammar rather than the setting —
    // and would stop guarding the switch it names.
    commit(&fixture, &format!("feat: closes {id}"));
    let message = sync(&fixture).expect("syncing");

    assert!(
        !message.contains('\u{2192}'),
        "no transition may be reported: {message}"
    );
    let events = events_of(&fixture, StoryNo::new(1));
    assert_eq!(events.len(), 2, "creation and the comment, nothing else");
    // SH-178: the commit DID claim it — the setting is why it did not move,
    // not a missing claim word. A user who turned auto-transition off should
    // not be told their commit grammar is wrong.
    assert!(
        !message.contains("no claim word"),
        "the setting is off, not the grammar: {message}"
    );
    assert!(
        message.contains(&format!(
            "linked without moving: {id} (sync.auto_transition is off for this project)"
        )),
        "the report must name the setting as the cause: {message}"
    );
}

/// SH-178, reason 4: since SH-125 a project with no `active`-role state is
/// the *common* shape, not an edge one — three required OPEN states
/// (`todo`/`in-progress`/`blocked`) mean the old two-open-state guess never
/// kicks in. A claim over such a project must say so, not blame the grammar.
#[test]
fn a_claim_with_no_active_state_configured_reports_why_not_the_grammar() {
    let mut states = default_states();
    for state in &mut states {
        state.role = None;
    }
    let fixture = ServiceFixture::with_states(&states);
    git_init(&fixture);
    let id = create(&fixture, "No active state to move into");
    commit(&fixture, &format!("feat: closes {id}"));

    let message = sync(&fixture).expect("syncing");
    assert!(
        !message.contains('\u{2192}'),
        "there is nowhere to move it: {message}"
    );
    assert!(
        !message.contains("no claim word"),
        "the commit claimed the story; the project has no active state, which is a \
         configuration fact, not a grammar mistake: {message}"
    );
    assert!(
        message.contains(&format!(
            "linked without moving: {id} (this project has no active state configured)"
        )),
        "the report must name the real cause: {message}"
    );
}

#[test]
fn an_absent_setting_leaves_the_transition_on() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Referenced");
    commit(&fixture, &format!("feat: closes {id}"));
    let message = sync(&fixture).expect("syncing");
    assert!(
        message.contains(&format!("{id}: todo \u{2192} in-progress")),
        "{message}"
    );
}

#[test]
fn commit_sync_fires_no_event_hooks() {
    // A week's worth of history would otherwise fire a burst of `comment` and
    // `state_change` hooks for work that happened days ago. The legacy path
    // fired none and neither does this.
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let marker = fixture.cwd().join("hook-fired");
    fixture.write_hooks_toml(&format!(
        "[hooks]\ncomment = \"touch {}\"\nstate_change = \"touch {}\"\n",
        marker.display(),
        marker.display()
    ));
    let id = create(&fixture, "Referenced");
    // Claims, so that both hooks under test have something to fire *about*: a
    // bare mention would leave no state change, and half this assertion would
    // pass vacuously.
    commit(&fixture, &format!("feat: closes {id}"));
    sync(&fixture).expect("syncing");
    assert!(
        !marker.exists(),
        "commit-sync must not fire the project's event hooks"
    );
}

#[test]
fn the_report_counts_commits_scanned_not_commits_matched() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Referenced");
    commit(&fixture, "chore: unrelated");
    commit(&fixture, &format!("feat: land {id}"));
    commit(&fixture, "docs: also unrelated");
    let message = sync(&fixture).expect("syncing");
    assert!(
        message.starts_with("scanned 3 commits, linked 1 commits to 1 stories"),
        "{message}"
    );
}

// ---------------------------------------------------------------------------
// What a commit does to a story — carried across from `differential_git.rs`
// ---------------------------------------------------------------------------

/// The referenced-by entry's fields are the contract every other surface
/// renders: `story show`, the dashboard, an export (SH-169). And the comment
/// stream — what `[git]` noise this feature exists to remove — stays empty.
///
/// Pinned here explicitly, character for character, because it is the thing a
/// later change to how a link is *stored* must not disturb.
#[test]
fn the_referenced_by_commit_reads_short_hash_colon_subject() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Referenced");
    let subject = format!("feat: land {id}");
    commit(&fixture, &subject);
    let short = head_short_hash(&fixture);
    let full = head_full_hash(&fixture);
    sync(&fixture).expect("syncing");

    let snapshot = snapshot_of(&fixture, StoryNo::new(1));
    assert_eq!(snapshot.comments, Vec::new(), "no `[git]` comment noise");
    assert_eq!(snapshot.referenced_by_commits.len(), 1);
    let commit_ref = &snapshot.referenced_by_commits[0];
    assert_eq!(commit_ref.sha, full);
    assert_eq!(commit_ref.subject, subject);
    assert_eq!(
        storyhook::domain::git_link_comment(&commit_ref.sha, &commit_ref.subject),
        format!("[git] {short}: {subject}"),
        "the rendered form a human reads still matches the pre-SH-169 text"
    );
}

#[test]
fn a_repository_whose_commits_name_no_stories_changes_nothing() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    create(&fixture, "Untouched");
    commit(&fixture, "chore: nothing to do with stories");

    let message = sync(&fixture).expect("syncing");
    assert!(
        message.starts_with("scanned 1 commits, linked 0 commits to 0 stories"),
        "{message}"
    );
    assert_eq!(events_of(&fixture, StoryNo::new(1)).len(), 1);
}

#[test]
fn a_second_run_over_the_same_window_adds_nothing() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Referenced twice");
    commit(&fixture, &format!("fix: {id} first pass"));

    sync(&fixture).expect("the first run");
    let before = events_of(&fixture, StoryNo::new(1));
    let second = sync(&fixture).expect("the second run");

    assert!(
        second.contains("linked 0 commits to 0 stories"),
        "a re-run over an overlapping window must add nothing: {second}"
    );
    assert_eq!(
        events_of(&fixture, StoryNo::new(1)),
        before,
        "and must leave the event log byte for byte as it was"
    );
}

#[test]
fn several_commits_naming_one_story_comment_it_each_time_and_move_it_once() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Busy story");
    // Every one of them claims. The point is that the *second* and *third*
    // claims change nothing, not that they failed to claim.
    for part in ["one", "two", "three"] {
        commit(&fixture, &format!("feat: fixes {id} part {part}"));
    }

    let message = sync(&fixture).expect("syncing");
    assert_eq!(referenced_by_commits_of(&fixture, StoryNo::new(1)).len(), 3);
    assert_eq!(
        message.matches('\u{2192}').count(),
        1,
        "a story moves on the first commit that names it and no other: {message}"
    );
    assert_eq!(
        events_of(&fixture, StoryNo::new(1))
            .iter()
            .filter(|event| matches!(event, StoryEvent::StoryStateChanged { .. }))
            .count(),
        1
    );
}

#[test]
fn one_commit_naming_several_stories_touches_each_of_them_once() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let first = create(&fixture, "First");
    let second = create(&fixture, "Second");
    create(&fixture, "Third");
    commit(&fixture, &format!("chore: touch {first} and {second}"));

    let message = sync(&fixture).expect("syncing");
    assert!(
        message.starts_with("scanned 1 commits, linked 2 commits to 2 stories"),
        "{message}"
    );
    assert_eq!(referenced_by_commits_of(&fixture, StoryNo::new(1)).len(), 1);
    assert_eq!(referenced_by_commits_of(&fixture, StoryNo::new(2)).len(), 1);
    assert!(
        referenced_by_commits_of(&fixture, StoryNo::new(3)).is_empty(),
        "the story the commit did not name must be untouched"
    );
}

// ---------------------------------------------------------------------------
// SH-279: a closed story still takes a link
// ---------------------------------------------------------------------------

/// The defect this fixes: before SH-279, `record_commit` resolved every
/// story through `resolve_open_story`, so a merge commit naming a story its
/// own PR had just closed recorded nothing — no event, no `referenced_by`
/// entry, no diagnostic. `Intent::Append` now permits exactly the same
/// append SH-261 already granted `story comment`.
#[test]
fn a_commit_naming_a_closed_story_is_linked_but_not_moved() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Already finished");
    StoryService::new(&fixture.ctx())
        .set_state(&id, "done", None, None, None)
        .expect("closing it");
    // A claim, not a bare mention: this proves the story does not move even
    // when the commit asks it to, not merely that a mention never would have.
    commit(&fixture, &format!("chore: closes {id}"));

    let message = sync(&fixture).expect("syncing");
    assert!(
        message.starts_with("scanned 1 commits, linked 1 commits to 1 stories"),
        "{message}"
    );
    assert!(
        !message.contains('\u{2192}'),
        "a closed story must never move: {message}"
    );
    assert!(
        message.contains(&format!(
            "linked without moving: {id} (the story is closed)"
        )),
        "the report must name the real cause: {message}"
    );
    assert_eq!(snapshot_of(&fixture, StoryNo::new(1)).state, "done");
    assert_eq!(referenced_by_commits_of(&fixture, StoryNo::new(1)).len(), 1);
}

/// Field-by-field, modelled on `service_story.rs`'s
/// `a_comment_on_a_closed_story_moves_only_updated_at_and_the_comment_list` —
/// the same SH-261 argument, made for the sibling write SH-279 grants it to.
#[test]
fn a_commit_link_on_a_closed_story_moves_only_updated_at_and_the_commit_list() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "untouched by a link");
    StoryService::new(&fixture.ctx())
        .set_state(&id, "done", None, None, None)
        .expect("closing it");
    let before = snapshot_of(&fixture, StoryNo::new(1));
    commit(&fixture, &format!("chore: closes {id}"));

    sync(&fixture).expect("syncing");
    let after = snapshot_of(&fixture, StoryNo::new(1));

    assert_eq!(after.state, before.state);
    assert_eq!(after.superstate, before.superstate);
    assert_eq!(after.closed_at, before.closed_at);
    assert_eq!(after.hidden_at, before.hidden_at);
    assert_eq!(after.deleted, before.deleted);
    assert_eq!(after.draft, before.draft);
    assert_eq!(after.labels, before.labels);
    assert_eq!(after.assignee, before.assignee);
    assert_eq!(after.priority, before.priority);
    assert_eq!(after.awaiting, before.awaiting);
    assert_eq!(after.relationships, before.relationships);
    assert_eq!(after.title, before.title);
    assert_eq!(after.comments, before.comments);
    // The two that do move, and the reason the story stays findable.
    assert_eq!(
        after.referenced_by_commits.len(),
        before.referenced_by_commits.len() + 1
    );
    assert!(after.updated_at >= before.updated_at);
    // `archived` is a store-level derivation of `closed_at`; a link must not
    // disturb it either, or the story would leave the archive.
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
        .expect("reading the row")
        .expect("the story exists");
    assert!(row.archived, "a linked closed story stays archived");
}

/// `hidden` is a display fact, not a permission one — the same rule SH-261
/// pinned for `comment`.
#[test]
fn a_hidden_closed_story_accepts_a_link_and_stays_hidden() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let id = create(&fixture, "out of sight");
    service
        .set_state(&id, "done", None, None, None)
        .expect("closing it");
    service.hide(&id).expect("hiding");
    commit(&fixture, &format!("chore: mentions {id}"));

    sync(&fixture).expect("syncing");

    let after = snapshot_of(&fixture, StoryNo::new(1));
    assert!(after.hidden_at.is_some(), "the story stays hidden");
    assert_eq!(after.referenced_by_commits.len(), 1);
}

/// A commit already linked to a closed story adds nothing on a second run —
/// the idempotency check does not care whether the story is open.
#[test]
fn a_second_run_over_a_closed_story_adds_nothing() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "linked once");
    StoryService::new(&fixture.ctx())
        .set_state(&id, "done", None, None, None)
        .expect("closing it");
    commit(&fixture, &format!("chore: mentions {id}"));
    sync(&fixture).expect("first sync");

    let message = sync(&fixture).expect("second sync");
    assert!(
        message.contains("linked 0 commits to 0 stories"),
        "a re-run over an already-linked closed story must add nothing: {message}"
    );
    assert_eq!(referenced_by_commits_of(&fixture, StoryNo::new(1)).len(), 1);
}

#[test]
fn a_commit_naming_a_story_that_does_not_exist_is_skipped() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    commit(&fixture, "feat: implements SH-404, which nobody filed");

    let message = sync(&fixture).expect("a phantom reference is not an error");
    assert!(
        message.contains("linked 0 commits to 0 stories"),
        "{message}"
    );
}

#[test]
fn a_story_already_out_of_the_default_state_is_commented_but_not_moved() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Under review");
    StoryService::new(&fixture.ctx())
        .set_state(&id, "in-progress", None, None, None)
        .expect("moving it on");
    // It must *claim*. With a bare mention the story would stay put because of
    // the grammar, and this test would prove nothing about the state it is in.
    commit(&fixture, &format!("fix: fixes {id}, more work"));

    let message = sync(&fixture).expect("syncing");
    assert!(
        !message.contains('\u{2192}'),
        "a story someone has already moved on must not be dragged back: {message}"
    );
    assert_eq!(referenced_by_commits_of(&fixture, StoryNo::new(1)).len(), 1);
    assert_eq!(snapshot_of(&fixture, StoryNo::new(1)).state, "in-progress");
    // SH-178: the commit DID claim it — the reason it did not move is that the
    // story had already moved on, not that the commit's grammar was wrong.
    // Asserting the real cause here is the regression test for the report
    // asserting "no claim word" for every reason a story stayed put.
    assert!(
        !message.contains("no claim word"),
        "the commit claimed the story; blaming its grammar is the SH-178 defect: {message}"
    );
    assert!(
        message.contains(&format!(
            "linked without moving: {id} (already out of the project's default open state)"
        )),
        "the report must name the real cause: {message}"
    );
}

// ---------------------------------------------------------------------------
// SH-124: a mention links, a claim moves
// ---------------------------------------------------------------------------

/// The defect, at the service layer: the trailer shape that moved five stories
/// in a sibling project links and changes nothing.
#[test]
fn a_bare_mention_links_the_commit_and_leaves_the_state_alone() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Merely referenced");
    commit_with_body(&fixture, "feat: something else", &format!("Refs {id}"));

    let message = sync(&fixture).expect("syncing");
    assert!(
        !message.contains('\u{2192}'),
        "a bare mention must not move a story: {message}"
    );
    assert_eq!(
        referenced_by_commits_of(&fixture, StoryNo::new(1)).len(),
        1,
        "but it must still be linked — the referenced-by trail is the useful half"
    );
    assert_eq!(snapshot_of(&fixture, StoryNo::new(1)).state, "todo");
}

/// A `Refs` trailer over a list: every id links, none of them claims.
#[test]
fn a_refs_trailer_over_a_list_moves_none_of_them() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let first = create(&fixture, "One");
    let second = create(&fixture, "Two");
    let third = create(&fixture, "Three");
    commit_with_body(
        &fixture,
        "chore: unrelated work",
        &format!("Refs {first}, {second}, {third}"),
    );

    let message = sync(&fixture).expect("syncing");
    assert!(!message.contains('\u{2192}'), "{message}");
    for number in 1..=3 {
        assert_eq!(
            snapshot_of(&fixture, StoryNo::new(number)).state,
            "todo",
            "story {number} must not have moved"
        );
        assert_eq!(
            referenced_by_commits_of(&fixture, StoryNo::new(number)).len(),
            1
        );
    }
}

/// A git trailer claims: `Key: value` where the value is the whole line.
#[test]
fn a_colon_trailer_claims_and_moves_the_story() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Claimed by a trailer");
    commit_with_body(&fixture, "feat: land the thing", &format!("Closes: {id}"));

    let message = sync(&fixture).expect("syncing");
    assert!(
        message.contains(&format!("{id}: todo \u{2192} in-progress")),
        "{message}"
    );
}

/// The decisive case the council split on: here the colon is a Conventional
/// Commits *type*, not a trailer key, and the id is the first word of a
/// description rather than the whole value.
#[test]
fn a_conventional_commits_subject_links_without_claiming() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Named in a subject");
    commit(&fixture, &format!("fix: {id} broken parser"));

    let message = sync(&fixture).expect("syncing");
    assert!(
        !message.contains('\u{2192}'),
        "a CC type prefix is not a trailer key: {message}"
    );
    assert_eq!(referenced_by_commits_of(&fixture, StoryNo::new(1)).len(), 1);
    assert_eq!(snapshot_of(&fixture, StoryNo::new(1)).state, "todo");
}

/// One commit, two stories, two different intents.
#[test]
fn one_commit_may_claim_one_story_and_merely_mention_another() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let claimed = create(&fixture, "Worked on");
    let mentioned = create(&fixture, "Merely related");
    commit_with_body(
        &fixture,
        "feat: the work",
        &format!("Closes {claimed}\nRefs {mentioned}"),
    );

    let message = sync(&fixture).expect("syncing");
    assert!(
        message.contains(&format!("{claimed}: todo \u{2192} in-progress")),
        "{message}"
    );
    assert_eq!(snapshot_of(&fixture, StoryNo::new(1)).state, "in-progress");
    assert_eq!(
        snapshot_of(&fixture, StoryNo::new(2)).state,
        "todo",
        "the mentioned story must not have been dragged along"
    );
    assert_eq!(referenced_by_commits_of(&fixture, StoryNo::new(2)).len(), 1);
}

/// Without this line a project whose commits use no claim word cannot tell
/// "auto-transition is off" from "auto-transition is broken".
#[test]
fn the_report_names_the_stories_it_linked_without_claiming() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Merely referenced");
    commit_with_body(&fixture, "feat: something", &format!("Refs {id}"));

    let message = sync(&fixture).expect("syncing");
    assert!(
        message.contains("linked without claiming"),
        "the run must say what it declined to move: {message}"
    );
    assert!(message.contains(&id), "{message}");
}

/// A story that some commit claimed is not reported as merely linked, however
/// many other commits only mentioned it.
#[test]
fn a_claimed_story_is_not_also_reported_as_linked_only() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Mentioned then claimed");
    commit_with_body(&fixture, "chore: groundwork", &format!("Refs {id}"));
    commit_with_body(&fixture, "feat: the work", &format!("Closes {id}"));

    let message = sync(&fixture).expect("syncing");
    assert!(
        message.contains(&format!("{id}: todo \u{2192} in-progress")),
        "{message}"
    );
    assert!(
        !message.contains("linked without claiming"),
        "it was claimed, so it must not be listed as linked-only: {message}"
    );
}

/// `git revert` copies the original subject into line 1, keywords and all.
#[test]
fn a_revert_does_not_claim_the_story_its_quoted_subject_names() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Reverted work");
    commit(&fixture, &format!("Revert \"feat: closes {id}\""));

    let message = sync(&fixture).expect("syncing");
    assert!(
        !message.contains('\u{2192}'),
        "a revert re-states the original subject; it does not claim it: {message}"
    );
    assert_eq!(
        referenced_by_commits_of(&fixture, StoryNo::new(1)).len(),
        1,
        "the revert is still linked, which is what you want to read later"
    );
}

#[test]
fn an_explicit_window_is_honoured() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "In the window");
    commit(&fixture, &format!("feat: {id} lands"));

    let message = GitService::new(&fixture.ctx())
        .commit_sync(Some("1h"))
        .expect("syncing");
    assert!(
        message.starts_with("scanned 1 commits, linked 1 commits to 1 stories"),
        "{message}"
    );
}

#[test]
fn a_window_that_excludes_every_commit_scans_nothing() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Out of the window");
    // Dated in the past rather than "now": `--since=0d` resolved against a
    // commit made in the current second lands on whichever side of the cutoff
    // the run happens to fall, which is a flake rather than a finding.
    commit_at(
        &fixture,
        &format!("feat: {id} lands"),
        "2020-01-01T00:00:00Z",
    );

    let message = GitService::new(&fixture.ctx())
        .commit_sync(Some("0d"))
        .expect("syncing");
    assert!(
        message.starts_with("scanned 0 commits, linked 0 commits to 0 stories"),
        "{message}"
    );
    assert_eq!(events_of(&fixture, StoryNo::new(1)).len(), 1);
}

#[test]
fn a_reference_carrying_another_projects_prefix_is_ignored() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    create(&fixture, "Ours");
    commit(&fixture, "feat: closes AB-1 in the other tracker");

    let message = sync(&fixture).expect("syncing");
    assert!(
        message.contains("linked 0 commits to 0 stories"),
        "{message}"
    );
    assert_eq!(events_of(&fixture, StoryNo::new(1)).len(), 1);
}

#[test]
fn a_pinned_clock_stamps_every_event_the_run_writes() {
    let mut fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Referenced");
    // Claims, so the run writes a `StoryStateChanged` as well as a link. A bare
    // mention would leave only the link, and this loop would keep passing while
    // covering half of what its name promises.
    commit(&fixture, &format!("feat: closes {id}"));
    fixture.set_clock(Clock::Fixed("2030-06-01T12:00:00Z".to_string()));
    sync(&fixture).expect("syncing");

    let events = events_of(&fixture, StoryNo::new(1));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StoryEvent::StoryStateChanged { .. })),
        "the run must have written a state change for this loop to be worth running: {events:?}"
    );
    for event in events.iter().skip(1) {
        let at = match event {
            StoryEvent::StoryCommitLinked { at, .. } | StoryEvent::StoryStateChanged { at, .. } => {
                at.as_str()
            }
            other => panic!("unexpected event {other:?}"),
        };
        assert_eq!(at, "2030-06-01T12:00:00Z");
    }
}

// ---------------------------------------------------------------------------
// SH-58: the body is part of the commit message
// ---------------------------------------------------------------------------

#[test]
fn a_story_named_only_in_the_commit_body_is_linked() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Referenced from a trailer");
    commit_with_body(&fixture, "feat: land the thing", &format!("Closes {id}"));
    let full = head_full_hash(&fixture);

    let message = sync(&fixture).expect("syncing");
    assert!(
        message.starts_with("scanned 1 commits, linked 1 commits to 1 stories"),
        "{message}"
    );
    let referenced = referenced_by_commits_of(&fixture, StoryNo::new(1));
    assert_eq!(
        referenced
            .iter()
            .map(|c| (c.sha.as_str(), c.subject.as_str()))
            .collect::<Vec<_>>(),
        vec![(full.as_str(), "feat: land the thing")],
        "the referenced-by subject stays the SUBJECT — a multi-paragraph body \
         would be unreadable in `story show`"
    );
}

/// The parse trap SH-58 warns about, made into a test.
///
/// `%B` emits multi-line records. A line-oriented parser splitting each line on
/// its first space reads `Closes SH-1` as hash=`Closes`, subject=`SH-1` — and
/// the short hash is the idempotency key, so a garbage hash also breaks "safe
/// to run repeatedly". The hash must come from `%H` and from nowhere else.
#[test]
fn a_body_line_shaped_like_a_log_line_is_not_read_as_a_commit() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Referenced");
    commit_with_body(
        &fixture,
        "feat: land the thing",
        &format!("deadbeefcafe {id} looks exactly like a log line\nCloses {id}"),
    );
    let full = head_full_hash(&fixture);

    let message = sync(&fixture).expect("syncing");
    assert!(
        message.starts_with("scanned 1 commits,"),
        "one commit, however many lines its message has: {message}"
    );
    let referenced = referenced_by_commits_of(&fixture, StoryNo::new(1));
    assert_eq!(
        referenced
            .iter()
            .map(|c| (c.sha.as_str(), c.subject.as_str()))
            .collect::<Vec<_>>(),
        vec![(full.as_str(), "feat: land the thing")],
        "a story named twice in one commit is linked once, under the real hash"
    );
    assert!(
        !referenced[0].sha.starts_with("deadbee"),
        "the hash must come from %H, never from body text"
    );
}

#[test]
fn a_body_reference_is_idempotent_across_runs() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Referenced from a trailer");
    commit_with_body(&fixture, "feat: land the thing", &format!("Closes {id}"));

    sync(&fixture).expect("the first run");
    let before = events_of(&fixture, StoryNo::new(1));
    let second = sync(&fixture).expect("the second run");

    assert!(second.contains("linked 0 commits to 0 stories"), "{second}");
    assert_eq!(events_of(&fixture, StoryNo::new(1)), before);
}

#[test]
fn one_commit_naming_one_story_in_the_subject_and_another_in_the_body_links_both() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let first = create(&fixture, "In the subject");
    let second = create(&fixture, "In the body");
    commit_with_body(
        &fixture,
        &format!("feat: work on {first}"),
        &format!("Also touches {second}.\n\nCo-authored-by: nobody <n@n>"),
    );

    let message = sync(&fixture).expect("syncing");
    assert!(
        message.starts_with("scanned 1 commits, linked 2 commits to 2 stories"),
        "{message}"
    );
    assert_eq!(referenced_by_commits_of(&fixture, StoryNo::new(1)).len(), 1);
    assert_eq!(referenced_by_commits_of(&fixture, StoryNo::new(2)).len(), 1);
}

#[test]
fn a_multi_paragraph_body_is_scanned_to_its_last_line() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Named at the very end");
    commit_with_body(
        &fixture,
        "refactor: something large",
        &format!(
            "A paragraph explaining the change.\n\nA second paragraph, with\n\
             several lines, none of which\nname anything.\n\nCloses {id}"
        ),
    );

    let message = sync(&fixture).expect("syncing");
    assert!(
        message.contains("linked 1 commits to 1 stories"),
        "{message}"
    );
}

// ---------------------------------------------------------------------------
// Idempotency as a database constraint (event kind #18)
// ---------------------------------------------------------------------------

/// A second link record for the same commit on the same story is not a state
/// the store can hold.
///
/// The check `commit-sync` makes first is a courtesy — it lets the command
/// report "linked 0 commits" rather than fail. This is what makes the property
/// true regardless of any caller, which is the difference between an invariant
/// and a convention.
#[test]
fn a_second_link_record_for_one_commit_is_rejected_by_the_store() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Linked once");
    commit(&fixture, &format!("feat: land {id}"));
    sync(&fixture).expect("syncing");

    let sha = head_full_hash(&fixture);
    let second = fixture.store().write(|tx| {
        tx.append_events(
            fixture.project(),
            StoryNo::new(1),
            storyhook::store::ExpectedSeq::Any,
            &[StoryEvent::StoryCommitLinked {
                at: "2030-01-01T00:00:00Z".to_string(),
                sha: sha.clone(),
                subject: "feat: land it again".to_string(),
            }],
            &storyhook::domain::provenance::Provenance::unrecorded(),
        )?;
        Ok(())
    });

    assert!(
        second.is_err(),
        "the store must refuse a duplicate link record: {second:?}"
    );
    assert_eq!(
        referenced_by_commits_of(&fixture, StoryNo::new(1)).len(),
        1,
        "and the refusal must have rolled the whole append back"
    );
}

/// The same commit legitimately links to *different* stories.
#[test]
fn one_commit_may_link_to_several_stories() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let first = create(&fixture, "First");
    let second = create(&fixture, "Second");
    commit(&fixture, &format!("chore: {first} and {second}"));

    sync(&fixture).expect("syncing");
    assert_eq!(referenced_by_commits_of(&fixture, StoryNo::new(1)).len(), 1);
    assert_eq!(referenced_by_commits_of(&fixture, StoryNo::new(2)).len(), 1);
}

/// A user comment that opens like a link record must not suppress a real one.
///
/// The old idempotency check scanned every event for a comment starting
/// `[git] <short>:`, so typing that prefix by hand silently stopped the next
/// `commit-sync` from linking that commit. The constraint is keyed on a field,
/// not on rendered text, so a comment cannot reach it — `commit_linked` (the
/// check `record_commit` actually asks) reads the structured
/// `story_commit_links` table, which a `StoryCommentAdded` never writes to.
///
/// Since SH-169, a hand-typed lookalike is textually indistinguishable from a
/// real *pre-#18* link at fold time too (see `git_link_sha`), so it now folds
/// into `referenced_by_commits` beside the real one rather than `comments` —
/// the same ambiguity the format always had, just visible in the section this
/// feature moved links into. What this test still pins is the property in its
/// name: the impostor cannot suppress `commit-sync` from creating the real,
/// structured link.
#[test]
fn a_hand_written_comment_that_looks_like_a_link_does_not_suppress_the_real_one() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "About to be impersonated");
    commit(&fixture, &format!("feat: land {id}"));
    let short = head_short_hash(&fixture);
    let full = head_full_hash(&fixture);

    StoryService::new(&fixture.ctx())
        .comment(&id, &format!("[git] {short}: I typed this myself"))
        .expect("commenting");

    sync(&fixture).expect("syncing");
    assert_eq!(
        comments_of(&fixture, StoryNo::new(1)),
        Vec::<String>::new(),
        "neither the real link nor its lookalike is a comment"
    );
    let referenced = snapshot_of(&fixture, StoryNo::new(1)).referenced_by_commits;
    assert_eq!(
        referenced.len(),
        2,
        "the real link must still be recorded beside the impostor: {referenced:?}"
    );
    assert!(
        referenced.iter().any(|c| c.sha == full),
        "the real, structured link (full hash) is present: {referenced:?}"
    );
    assert!(
        referenced
            .iter()
            .any(|c| c.sha == short && c.subject == "I typed this myself"),
        "the hand-typed lookalike is present too, not silently dropped: {referenced:?}"
    );
}

/// The link record folds into `referenced_by_commits`, not the comment
/// stream (SH-169), and `story list --stale` reports it as `commit-linked`
/// activity rather than a comment — since there no longer is one.
#[test]
fn a_link_record_folds_into_referenced_by_not_comments() {
    use storyhook::domain::last_activity_type;

    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Rendered separately");
    fixture
        .store()
        .write(|tx| {
            tx.put_settings(
                fixture.project(),
                &ProjectSettings {
                    sync_auto_transition: Some(false),
                    ..ProjectSettings::default()
                },
            )
        })
        .expect("writing settings");
    let subject = format!("feat: land {id}");
    commit(&fixture, &subject);
    let full = head_full_hash(&fixture);
    sync(&fixture).expect("syncing");

    let snapshot = snapshot_of(&fixture, StoryNo::new(1));
    assert_eq!(
        snapshot.comments,
        Vec::new(),
        "the link must not appear as a comment"
    );
    assert_eq!(
        snapshot
            .referenced_by_commits
            .last()
            .map(|c| (c.sha.as_str(), c.subject.as_str())),
        Some((full.as_str(), subject.as_str()))
    );
    assert_eq!(
        last_activity_type(&events_of(&fixture, StoryNo::new(1))),
        "commit-linked",
        "this string is rendered by `story list --stale`"
    );
    fixture.assert_no_drift();
}

// ---------------------------------------------------------------------------
// SH-124: the two grammars must not drift apart
// ---------------------------------------------------------------------------

/// Every `.rs` file under `src/`, with its path relative to the crate root.
fn sources() -> Vec<(String, String)> {
    fn walk(dir: &std::path::Path, into: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).expect("reading src/") {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                walk(&path, into);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = std::fs::read_to_string(&path).expect("reading a source file");
                into.push((path.to_string_lossy().into_owned(), text));
            }
        }
    }
    let mut files = Vec::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    assert!(
        files.len() > 20,
        "expected the whole tree, got {}",
        files.len()
    );
    files
}

/// Closing implies working, so the post-merge hook must not be able to close a
/// story `commit-sync` would never have claimed — otherwise a merge could close
/// a story that was never seen active.
///
/// This reads the hook's own alternation out of `src/hooks.rs` rather than
/// restating it, so editing one grammar without the other fails here.
#[test]
fn every_keyword_the_merge_hook_closes_on_also_claims() {
    let hooks = sources()
        .into_iter()
        .find(|(path, _)| path.ends_with("hooks.rs"))
        .expect("src/hooks.rs")
        .1;

    let alternation = hooks
        .split_once("(closes?|fixes?|resolves?)")
        .map(|_| ["close", "closes", "fix", "fixes", "resolve", "resolves"])
        .expect(
            "src/hooks.rs no longer contains the alternation `(closes?|fixes?|resolves?)`. \
             The post-merge hook's closing grammar changed; update this test AND \
             `REF_WORDS` together, because a word that closes a story must also claim it.",
        );

    for word in alternation {
        let scanned = storyhook::domain::scan_story_refs("SH", &format!("{word} SH-1"));
        assert!(
            scanned
                .first()
                .is_some_and(storyhook::domain::StoryReference::claims),
            "`{word}` closes a story in the post-merge hook but does not claim it in \
             commit-sync, so a merge could close a story that was never active"
        );
    }
}

/// `ReferenceIntent::Claim` is constructed in exactly one module.
///
/// The defect SH-124 fixed was a caller deciding for itself what a reference
/// meant. Keeping construction in `domain.rs` is what makes the grammar one
/// thing rather than a rule each caller reimplements; a service that builds a
/// `Claim` of its own has reintroduced the defect's shape.
#[test]
fn a_claim_is_constructed_in_exactly_one_module() {
    let offenders: Vec<String> = sources()
        .into_iter()
        .filter(|(path, _)| !path.ends_with("domain.rs"))
        .filter(|(_, text)| {
            text.lines()
                .map(str::trim_start)
                .filter(|line| !line.starts_with("//"))
                .any(|line| line.contains("ReferenceIntent::Claim"))
        })
        .map(|(path, _)| {
            path.rsplit_once("/src/")
                .map_or(path.clone(), |(_, rest)| rest.to_string())
        })
        .collect();

    assert!(
        offenders.is_empty(),
        "only `domain.rs` may construct `ReferenceIntent::Claim`; these name it too: {offenders:?}. \
         Ask `StoryReference::claims()` instead — the grammar is one thing, not a rule each \
         caller reimplements."
    );
}
