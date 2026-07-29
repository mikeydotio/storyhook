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

use storyhook::domain::{StoryEvent, StorySnapshot};
use storyhook::error::AppError;
use storyhook::service::{Clock, GitService, NewStoryInput, StoryService};
use storyhook::store::{ProjectSettings, ReadOps, Store, StoryNo, WriteOps, partition_known};
use storyhook_test_support::ServiceFixture;

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
    commit(&fixture, &format!("feat: land {id}"));
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
    commit(&fixture, &format!("feat: land {id}"));
    let message = sync(&fixture).expect("syncing");

    assert!(
        !message.contains('\u{2192}'),
        "no transition may be reported: {message}"
    );
    let events = events_of(&fixture, StoryNo::new(1));
    assert_eq!(events.len(), 2, "creation and the comment, nothing else");
}

#[test]
fn an_absent_setting_leaves_the_transition_on() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Referenced");
    commit(&fixture, &format!("feat: land {id}"));
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
    commit(&fixture, &format!("feat: land {id}"));
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
        message.starts_with("scanned 3 commits, added 1 comments to 1 stories"),
        "{message}"
    );
}

// ---------------------------------------------------------------------------
// What a commit does to a story — carried across from `differential_git.rs`
// ---------------------------------------------------------------------------

/// The comment's text is the contract every other surface renders: `story
/// show`, the dashboard, an export.
///
/// Pinned here explicitly, character for character, because it is the thing a
/// later change to how a link is *stored* must not disturb.
#[test]
fn the_comment_reads_git_short_hash_colon_subject() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Referenced");
    let subject = format!("feat: land {id}");
    commit(&fixture, &subject);
    let short = head_short_hash(&fixture);
    sync(&fixture).expect("syncing");

    assert_eq!(
        comments_of(&fixture, StoryNo::new(1)),
        vec![format!("[git] {short}: {subject}")]
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
        message.starts_with("scanned 1 commits, added 0 comments to 0 stories"),
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
        second.contains("added 0 comments to 0 stories"),
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
    for part in ["one", "two", "three"] {
        commit(&fixture, &format!("feat: {id} part {part}"));
    }

    let message = sync(&fixture).expect("syncing");
    assert_eq!(comments_of(&fixture, StoryNo::new(1)).len(), 3);
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
        message.starts_with("scanned 1 commits, added 2 comments to 2 stories"),
        "{message}"
    );
    assert_eq!(comments_of(&fixture, StoryNo::new(1)).len(), 1);
    assert_eq!(comments_of(&fixture, StoryNo::new(2)).len(), 1);
    assert!(
        comments_of(&fixture, StoryNo::new(3)).is_empty(),
        "the story the commit did not name must be untouched"
    );
}

#[test]
fn a_commit_naming_a_closed_story_is_skipped() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Already finished");
    StoryService::new(&fixture.ctx())
        .set_state(&id, "done", None, None)
        .expect("closing it");
    commit(&fixture, &format!("chore: mention the closed {id}"));

    let message = sync(&fixture).expect("syncing");
    assert!(
        message.contains("added 0 comments to 0 stories"),
        "{message}"
    );
    assert_eq!(snapshot_of(&fixture, StoryNo::new(1)).state, "done");
}

#[test]
fn a_commit_naming_a_story_that_does_not_exist_is_skipped() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    commit(&fixture, "feat: implements SH-404, which nobody filed");

    let message = sync(&fixture).expect("a phantom reference is not an error");
    assert!(
        message.contains("added 0 comments to 0 stories"),
        "{message}"
    );
}

#[test]
fn a_story_already_out_of_the_default_state_is_commented_but_not_moved() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Under review");
    StoryService::new(&fixture.ctx())
        .set_state(&id, "in-progress", None, None)
        .expect("moving it on");
    commit(&fixture, &format!("fix: more work on {id}"));

    let message = sync(&fixture).expect("syncing");
    assert!(
        !message.contains('\u{2192}'),
        "a story someone has already moved on must not be dragged back: {message}"
    );
    assert_eq!(comments_of(&fixture, StoryNo::new(1)).len(), 1);
    assert_eq!(snapshot_of(&fixture, StoryNo::new(1)).state, "in-progress");
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
        message.starts_with("scanned 1 commits, added 1 comments to 1 stories"),
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
        message.starts_with("scanned 0 commits, added 0 comments to 0 stories"),
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
        message.contains("added 0 comments to 0 stories"),
        "{message}"
    );
    assert_eq!(events_of(&fixture, StoryNo::new(1)).len(), 1);
}

#[test]
fn a_pinned_clock_stamps_every_event_the_run_writes() {
    let mut fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Referenced");
    commit(&fixture, &format!("feat: land {id}"));
    fixture.set_clock(Clock::Fixed("2030-06-01T12:00:00Z".to_string()));
    sync(&fixture).expect("syncing");

    for event in events_of(&fixture, StoryNo::new(1)).iter().skip(1) {
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
    let short = head_short_hash(&fixture);

    let message = sync(&fixture).expect("syncing");
    assert!(
        message.starts_with("scanned 1 commits, added 1 comments to 1 stories"),
        "{message}"
    );
    assert_eq!(
        comments_of(&fixture, StoryNo::new(1)),
        vec![format!("[git] {short}: feat: land the thing")],
        "the comment text stays the SUBJECT — a multi-paragraph body would be \
         unreadable in `story show`"
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
    let short = head_short_hash(&fixture);

    let message = sync(&fixture).expect("syncing");
    assert!(
        message.starts_with("scanned 1 commits,"),
        "one commit, however many lines its message has: {message}"
    );
    let comments = comments_of(&fixture, StoryNo::new(1));
    assert_eq!(
        comments,
        vec![format!("[git] {short}: feat: land the thing")],
        "a story named twice in one commit is linked once, under the real hash"
    );
    assert!(
        !comments[0].starts_with("[git] deadbee"),
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

    assert!(second.contains("added 0 comments to 0 stories"), "{second}");
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
        message.starts_with("scanned 1 commits, added 2 comments to 2 stories"),
        "{message}"
    );
    assert_eq!(comments_of(&fixture, StoryNo::new(1)).len(), 1);
    assert_eq!(comments_of(&fixture, StoryNo::new(2)).len(), 1);
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
        message.contains("added 1 comments to 1 stories"),
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
/// report "added 0 comments" rather than fail. This is what makes the property
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
        )?;
        Ok(())
    });

    assert!(
        second.is_err(),
        "the store must refuse a duplicate link record: {second:?}"
    );
    assert_eq!(
        comments_of(&fixture, StoryNo::new(1)).len(),
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
    assert_eq!(comments_of(&fixture, StoryNo::new(1)).len(), 1);
    assert_eq!(comments_of(&fixture, StoryNo::new(2)).len(), 1);
}

/// A user comment that opens like a link record must not suppress a real one.
///
/// The old idempotency check scanned every event for a comment starting
/// `[git] <short>:`, so typing that prefix by hand silently stopped the next
/// `commit-sync` from linking that commit. The constraint is keyed on a field,
/// not on rendered text, so a comment cannot reach it.
#[test]
fn a_hand_written_comment_that_looks_like_a_link_does_not_suppress_the_real_one() {
    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "About to be impersonated");
    commit(&fixture, &format!("feat: land {id}"));
    let short = head_short_hash(&fixture);

    StoryService::new(&fixture.ctx())
        .comment(&id, &format!("[git] {short}: I typed this myself"))
        .expect("commenting");

    sync(&fixture).expect("syncing");
    let comments = comments_of(&fixture, StoryNo::new(1));
    assert_eq!(
        comments.len(),
        2,
        "the real link must still be recorded beside the impostor: {comments:?}"
    );
}

/// The link record renders into the comment stream where the comment it
/// replaced did, and reports the same last-activity type.
#[test]
fn a_link_record_is_indistinguishable_from_the_comment_it_replaced() {
    use storyhook::domain::last_activity_type;

    let fixture = ServiceFixture::new();
    git_init(&fixture);
    let id = create(&fixture, "Rendered the same");
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
    let short = head_short_hash(&fixture);
    sync(&fixture).expect("syncing");

    let snapshot = snapshot_of(&fixture, StoryNo::new(1));
    assert_eq!(
        snapshot.comments.last().map(|c| c.text.as_str()),
        Some(format!("[git] {short}: {subject}").as_str())
    );
    assert_eq!(
        last_activity_type(&events_of(&fixture, StoryNo::new(1))),
        "comment",
        "a link record is a comment to everything that reads a story; this \
         string is rendered by `story list --stale`"
    );
    fixture.assert_no_drift();
}
