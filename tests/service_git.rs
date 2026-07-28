//! `GitService` — the store-side properties `commit-sync` owes that the
//! differential harness cannot see: that comment and transition are one
//! transaction, that the project's `sync.auto_transition` setting is honoured,
//! and that no event hooks fire.

use std::process::Command;

use storyhook::domain::StoryEvent;
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
        matches!(events[1], StoryEvent::StoryCommentAdded { .. }),
        "the comment comes first: {events:?}"
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
            StoryEvent::StoryCommentAdded { at, .. } | StoryEvent::StoryStateChanged { at, .. } => {
                at.as_str()
            }
            other => panic!("unexpected event {other:?}"),
        };
        assert_eq!(at, "2030-06-01T12:00:00Z");
    }
}
