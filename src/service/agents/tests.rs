//! The census reads the resume helper's own evidence and claims no more than it shows.
use super::*;
use crate::service::resources::{ResourceCandidate, ResourcePane};
use crate::service::{NewStoryInput, StoryService};
use crate::store::{ProjectId, SqliteStore};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// A store-backed context over a fresh fixture project, `SH` prefixed.
fn fixture() -> (storyhook_test_support::ServiceFixture, SqliteStore) {
    let f = storyhook_test_support::ServiceFixture::new();
    let store = SqliteStore::open(f.store().path()).unwrap();
    (f, store)
}

fn ctx<'a>(
    f: &'a storyhook_test_support::ServiceFixture,
    store: &'a SqliteStore,
) -> Ctx<'a, SqliteStore> {
    Ctx::new(
        store,
        ProjectId::new(f.project().get()),
        f.cwd(),
        crate::env::Environment::at(f.cwd()),
    )
    .no_hooks(true)
}

/// A story opened in `state`; a closed state is reached by a move, since a
/// story cannot be created closed.
fn story(ctx: &Ctx<'_, SqliteStore>, title: &str, state: &str, story_type: Option<&str>) -> String {
    let closed = state == "done";
    let id = StoryService::new(ctx)
        .create(&NewStoryInput {
            title: title.into(),
            state: (!closed).then(|| state.to_string()),
            story_type: story_type.map(str::to_string),
            ..Default::default()
        })
        .unwrap()
        .id;
    if closed {
        StoryService::new(ctx)
            .set_state(&id, state, None, None, None)
            .unwrap();
    }
    id
}

/// The fixture project defines only `bug` and `feature`; an epic needs its
/// type declared first.
fn allow_epics(ctx: &Ctx<'_, SqliteStore>) {
    crate::service::ConfigService::new(ctx)
        .add_type(crate::domain::EPIC_TYPE_SLUG, None, None)
        .unwrap();
}

fn candidate(worktree: &Path) -> ResourceCandidate {
    ResourceCandidate {
        repository: PathBuf::from("/repo"),
        worktree: Some(worktree.to_path_buf()),
        branch: "worktree-SH-1".into(),
        registered: true,
        exists: true,
        locked: false,
        lease: None,
        sources: BTreeSet::from(["marker".to_string()]),
    }
}

fn pane(dead: bool) -> ResourcePane {
    ResourcePane {
        window_id: "@3".into(),
        window_name: "SH-1".into(),
        pane_id: "%7".into(),
        pid: "4242".into(),
        dead,
        provider: Some("claude".into()),
        cwd: PathBuf::from("/repo/.claude/worktrees/SH-1"),
    }
}

fn report(
    id: &str,
    status: &str,
    worktree: Option<&Path>,
    pane: Option<ResourcePane>,
) -> ResourceReport {
    ResourceReport {
        location_only: false,
        project: "fixture".into(),
        story_id: id.into(),
        status: status.into(),
        repository: Some(PathBuf::from("/repo")),
        worktree: worktree.map(Path::to_path_buf),
        branch: Some(format!("worktree-{id}")),
        window_name: id.into(),
        socket_path: Some(PathBuf::from("/private/tmp/tmux-501/default")),
        pane,
        provider: Some("claude".into()),
        candidates: worktree.map(candidate).into_iter().collect(),
        observations: Vec::new(),
        diagnostics: vec!["two registrations claim the story".into()],
    }
}

/// A linked worktree whose private Git directory holds `record`, if any.
fn worktree_with(root: &Path, name: &str, record: Option<&str>) -> PathBuf {
    let worktree = root.join(name);
    let admin = root.join(format!("{name}-admin"));
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::create_dir_all(&admin).unwrap();
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", admin.display()),
    )
    .unwrap();
    if let Some(record) = record {
        std::fs::write(admin.join(launch_record::LAUNCH_RECORD_FILE), record).unwrap();
    }
    worktree
}

#[test]
fn every_report_shape_maps_to_what_it_proves() {
    let (f, store) = fixture();
    let ctx = ctx(&f, &store);
    allow_epics(&ctx);
    let scratch = storyhook_test_support::scratch_dir();
    let live = story(&ctx, "Its agent works", "in-progress", None);
    let dead = story(&ctx, "Its agent exited", "in-progress", None);
    let gone = story(&ctx, "Its window is gone", "in-progress", None);
    let manual = story(&ctx, "Claimed by hand", "in-progress", None);
    let ambiguous = story(&ctx, "Two worktrees claim it", "in-progress", None);
    let unreadable = story(&ctx, "tmux did not answer", "in-progress", None);
    let recorded = story(&ctx, "Its launch was recorded", "in-progress", None);
    let spoiled = story(&ctx, "Its launch record is foreign", "in-progress", None);
    // Never asked about: not claimed, an epic, or closed.
    story(&ctx, "Not claimed", "todo", None);
    story(&ctx, "An epic", "in-progress", Some("epic"));
    story(&ctx, "Finished", "done", None);

    let record = format!(
        r#"{{"version":1,"project_slug":"fixture","story_id":"{recorded}","provider":"codex","model":null,"effort":"high","speed":null,"autonomy":"auto","recorded_at":"2026-09-29T00:00:00Z"}}"#
    );
    let foreign = record.replace(&recorded, "SH-999");
    let mut reports: BTreeMap<String, Result<ResourceReport, AppError>> = BTreeMap::new();
    let wt = |name: &str| worktree_with(scratch.path(), name, None);
    reports.insert(
        live.clone(),
        Ok(report(
            &live,
            "resolved",
            Some(&wt("live")),
            Some(pane(false)),
        )),
    );
    reports.insert(
        dead.clone(),
        Ok(report(
            &dead,
            "resolved",
            Some(&wt("dead")),
            Some(pane(true)),
        )),
    );
    reports.insert(
        gone.clone(),
        Ok(report(&gone, "resolved", Some(&wt("gone")), None)),
    );
    reports.insert(manual.clone(), Ok(report(&manual, "absent", None, None)));
    reports.insert(
        ambiguous.clone(),
        Ok(report(
            &ambiguous,
            "ambiguous",
            Some(&wt("ambiguous")),
            None,
        )),
    );
    reports.insert(
        unreadable.clone(),
        Err(AppError::Validation(
            "cannot query recorded tmux server".into(),
        )),
    );
    reports.insert(
        recorded.clone(),
        Ok(report(
            &recorded,
            "resolved",
            Some(&worktree_with(scratch.path(), "recorded", Some(&record))),
            None,
        )),
    );
    reports.insert(
        spoiled.clone(),
        Ok(report(
            &spoiled,
            "resolved",
            Some(&worktree_with(scratch.path(), "spoiled", Some(&foreign))),
            None,
        )),
    );
    let asked = std::cell::RefCell::new(Vec::new());
    let views = AgentService::new(&ctx)
        .census_with(|id| {
            asked.borrow_mut().push(id.to_string());
            match reports
                .get(id)
                .unwrap_or_else(|| panic!("the census asked about {id}, which it must skip"))
            {
                Ok(report) => Ok(report.clone()),
                Err(error) => Err(AppError::Validation(error.to_string())),
            }
        })
        .unwrap();
    let mut expected_asked: Vec<String> = reports.keys().cloned().collect();
    expected_asked.sort();
    let mut asked = asked.into_inner();
    asked.sort();
    assert_eq!(
        asked, expected_asked,
        "only claimed ordinary open stories are asked"
    );

    let by_id: BTreeMap<_, _> = views.iter().map(|v| (v.story.clone(), v)).collect();
    assert!(
        !by_id.contains_key(&manual),
        "a story with no dispatch evidence is omitted"
    );
    assert_eq!(by_id[&live].state, AgentState::Live);
    assert!(
        by_id[&live].detail.contains("%7"),
        "{}",
        by_id[&live].detail
    );
    assert_eq!(by_id[&dead].state, AgentState::Lost);
    assert!(
        by_id[&dead].detail.contains("exited"),
        "{}",
        by_id[&dead].detail
    );
    assert_eq!(by_id[&gone].state, AgentState::Lost);
    assert!(
        by_id[&gone].detail.contains("no window named")
            && by_id[&gone]
                .detail
                .contains("/private/tmp/tmux-501/default"),
        "{}",
        by_id[&gone].detail
    );
    assert_eq!(by_id[&ambiguous].state, AgentState::Unknown);
    assert!(
        by_id[&ambiguous].detail.contains("two registrations"),
        "{}",
        by_id[&ambiguous].detail
    );
    assert_eq!(by_id[&unreadable].state, AgentState::Unknown);
    assert!(
        by_id[&unreadable].detail.contains("cannot query"),
        "{}",
        by_id[&unreadable].detail
    );
    assert_eq!(by_id[&live].provider.as_deref(), Some("claude"));

    let launch = by_id[&recorded]
        .launch
        .as_ref()
        .expect("the recorded launch is offered");
    assert_eq!(launch.provider, "codex");
    assert_eq!(launch.effort.as_deref(), Some("high"));
    assert_eq!(launch.autonomy, launch_record::Autonomy::Auto);
    assert_eq!(by_id[&gone].launch, None, "no record, no launch");
    assert_eq!(
        by_id[&spoiled].state,
        AgentState::Lost,
        "a bad record does not hide a lost agent"
    );
    assert_eq!(by_id[&spoiled].launch, None);
    assert!(
        by_id[&spoiled].detail.contains("cannot be offered"),
        "{}",
        by_id[&spoiled].detail
    );
}

#[test]
fn a_project_with_no_active_role_has_nothing_to_resume() {
    let (f, store) = fixture();
    let ctx = ctx(&f, &store);
    story(&ctx, "Claimed", "in-progress", None);
    store
        .write(|tx| {
            let mut states = tx.states(ctx.project())?;
            for state in &mut states {
                state.role = None;
            }
            crate::service::state_set::write_states(tx, ctx.project(), &states)?;
            Ok(())
        })
        .unwrap();
    let views = AgentService::new(&ctx)
        .census_with(|id| panic!("nothing is claimed without an active role, yet {id} was asked"))
        .unwrap();
    assert!(views.is_empty());
}
