//! Legacy versus store for `story commit-sync`, against a real git repository.
//!
//! Both legs read the *same* repository — the store leg's context runs from the
//! legacy project's directory — so a disagreement here is about what the two
//! did with a commit log, never about which log they saw.

mod differential_support;

use differential_support::Differential;
use storyhook::cli::Invocation;

/// `story commit-sync` with the default window.
fn commit_sync() -> Invocation {
    Invocation::CommitSync { since: None }
}

#[test]
fn a_repository_with_no_matching_commits_agrees() {
    let differential = Differential::with_git();
    differential.commit("chore: nothing to do with stories");
    differential.step("commit-sync with no references", commit_sync());
}

#[test]
fn a_commit_naming_a_story_agrees_on_the_comment_and_the_transition() {
    let differential = Differential::with_git();
    let id = differential.step_id(
        "new",
        Invocation::New {
            title: "Referenced".into(),
            state: None,
            story_type: None,
            description: None,
            priority: None,
            labels: None,
            assignee: None,
        },
    );
    differential.commit(&format!("feat: land the thing ({id})"));
    differential.step("commit-sync", commit_sync());
    differential.show("after commit-sync", &id);
}

#[test]
fn a_second_run_over_the_same_window_adds_nothing() {
    let differential = Differential::with_git();
    let id = differential.step_id(
        "new",
        Invocation::New {
            title: "Referenced twice".into(),
            state: None,
            story_type: None,
            description: None,
            priority: None,
            labels: None,
            assignee: None,
        },
    );
    differential.commit(&format!("fix: {id} first pass"));
    differential.step("commit-sync", commit_sync());
    differential.step("commit-sync again", commit_sync());
    differential.show("after two runs", &id);
}

#[test]
fn several_commits_naming_one_story_transition_it_exactly_once() {
    let differential = Differential::with_git();
    let id = differential.step_id(
        "new",
        Invocation::New {
            title: "Busy story".into(),
            state: None,
            story_type: None,
            description: None,
            priority: None,
            labels: None,
            assignee: None,
        },
    );
    differential.commit(&format!("feat: {id} part one"));
    differential.commit(&format!("feat: {id} part two"));
    differential.commit(&format!("feat: {id} part three"));
    differential.step("commit-sync", commit_sync());
    differential.show("after three commits", &id);
}

#[test]
fn one_commit_naming_several_stories_agrees() {
    let differential = Differential::with_git();
    let mut ids = Vec::new();
    for title in ["First", "Second", "Third"] {
        ids.push(differential.step_id(
            "new",
            Invocation::New {
                title: title.into(),
                state: None,
                story_type: None,
                description: None,
                priority: None,
                labels: None,
                assignee: None,
            },
        ));
    }
    differential.commit(&format!(
        "chore: touch {} and {} but not {}",
        ids[0], ids[1], "nothing"
    ));
    differential.step("commit-sync", commit_sync());
    for id in &ids {
        differential.show("after a multi-story commit", id);
    }
}

#[test]
fn a_commit_naming_a_closed_story_is_skipped_by_both_legs() {
    let differential = Differential::with_git();
    let id = differential.step_id(
        "new",
        Invocation::New {
            title: "Already finished".into(),
            state: None,
            story_type: None,
            description: None,
            priority: None,
            labels: None,
            assignee: None,
        },
    );
    differential.step(
        "close it",
        Invocation::SetState {
            id: id.clone(),
            state: "done".into(),
            comment: None,
            if_state: None,
        },
    );
    differential.commit(&format!("chore: mention the closed {id}"));
    differential.step("commit-sync over a closed story", commit_sync());
    differential.show("still closed", &id);
}

#[test]
fn a_commit_naming_a_story_that_does_not_exist_is_skipped_by_both_legs() {
    let differential = Differential::with_git();
    differential.commit("feat: implements SH-404, which nobody filed");
    differential.step("commit-sync over a phantom", commit_sync());
}

#[test]
fn a_story_already_out_of_the_default_state_is_commented_but_not_moved() {
    let differential = Differential::with_git();
    let id = differential.step_id(
        "new",
        Invocation::New {
            title: "Under review".into(),
            state: None,
            story_type: None,
            description: None,
            priority: None,
            labels: None,
            assignee: None,
        },
    );
    differential.step(
        "move it on",
        Invocation::SetState {
            id: id.clone(),
            state: "in-progress".into(),
            comment: None,
            if_state: None,
        },
    );
    differential.commit(&format!("fix: more work on {id}"));
    differential.step("commit-sync", commit_sync());
    differential.show("not dragged back", &id);
}

#[test]
fn an_explicit_window_agrees() {
    let differential = Differential::with_git();
    let id = differential.step_id(
        "new",
        Invocation::New {
            title: "In the window".into(),
            state: None,
            story_type: None,
            description: None,
            priority: None,
            labels: None,
            assignee: None,
        },
    );
    differential.commit(&format!("feat: {id} lands"));
    differential.step(
        "commit-sync --since 1h",
        Invocation::CommitSync {
            since: Some("1h".into()),
        },
    );
    differential.show("after a windowed run", &id);
}

#[test]
fn a_window_that_excludes_every_commit_agrees_on_the_empty_report() {
    let differential = Differential::with_git();
    let id = differential.step_id(
        "new",
        Invocation::New {
            title: "Out of the window".into(),
            state: None,
            story_type: None,
            description: None,
            priority: None,
            labels: None,
            assignee: None,
        },
    );
    // Dated in the past, not "now": `--since=0d` is resolved against each leg's
    // own clock, and a commit made in the current second lands on whichever
    // side of the cutoff the leg happens to run on — the first leg sees a
    // cutoff equal to the commit's timestamp and includes it, the second runs
    // after the second ticks over and does not. A fixed old timestamp makes
    // "outside the window" a property of the fixture rather than of when the
    // test happened to be scheduled.
    differential.commit_at(&format!("feat: {id} lands"), Some("2020-01-01T00:00:00Z"));
    differential.step(
        "commit-sync over an empty window",
        Invocation::CommitSync {
            since: Some("0d".into()),
        },
    );
    differential.show("untouched", &id);
}

#[test]
fn an_invalid_window_agrees_on_the_error() {
    let differential = Differential::with_git();
    differential.step(
        "commit-sync --since nonsense",
        Invocation::CommitSync {
            since: Some("nonsense".into()),
        },
    );
}

#[test]
fn outside_a_git_repository_both_legs_refuse() {
    // No `.git()`, so the project directory is not a repository at all.
    let differential = Differential::new();
    differential.step("commit-sync outside a repo", commit_sync());
}

#[test]
fn a_prefix_that_is_not_this_projects_is_ignored_by_both_legs() {
    let differential = Differential::with_git();
    let id = differential.step_id(
        "new",
        Invocation::New {
            title: "Ours".into(),
            state: None,
            story_type: None,
            description: None,
            priority: None,
            labels: None,
            assignee: None,
        },
    );
    differential.commit("feat: closes AB-1 in the other tracker");
    differential.step("commit-sync with a foreign prefix", commit_sync());
    differential.show("untouched", &id);
}
