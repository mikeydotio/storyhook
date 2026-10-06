//! Restore-specific physical rebinding. No claim, progress or conversation reset.
use super::*;
use serde_json::Value;
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::BorrowedFd;

/// Run the shared proof helper with the caller's existing workspace exclusion.
pub(super) fn call(
    dispatcher: &ShellDispatcher,
    lane: &EngineLaneRecord,
    expected: Option<&Value>,
    workspace: Option<BorrowedFd<'_>>,
    rearm: bool,
    deadline: Instant,
) -> Result<Option<Value>, AppError> {
    let Some(lease) = &lane.cleanup_lease else {
        return Ok(None);
    };
    // Resolve unmanaged servers cheaply. Ensure belongs to the background
    // reconciler, never the synchronous pre-publication restart sweep.
    let target = super::super::tmux_target::ensure(
        &dispatcher.env,
        Some(&lease.tmux.socket_path),
        deadline,
        &Default::default(),
    )?;
    if !target.protected {
        return Ok(None);
    }
    let helper = dispatcher
        .story_sh_path
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| AppError::Validation("dispatch helper has no plugin root".into()))?
        .join("lib/restoration.py");
    let mut input = serde_json::json!({"lease":lease,"window":lane.window_name});
    if let Some(expected) = expected {
        input["proposal"] = expected.clone();
        input["lease"] = expected["lease_before"].clone();
    }
    let mut file = tempfile::tempfile()?;
    file.write_all(&serde_json::to_vec(&input)?)?;
    file.seek(SeekFrom::Start(0))?;
    let mut command = Command::new("python3");
    apply_dispatch_allowlist(&mut command);
    command
        .env("HOME", dispatcher.env.home())
        .envs(dispatcher.env.child_vars())
        .env("STORY_BIN", std::env::current_exe()?)
        .arg(helper)
        .arg(if rearm {
            "rearm"
        } else if expected.is_some() {
            "publish"
        } else {
            "propose"
        })
        .current_dir(&lease.worktree_path)
        .env(
            "STORY_RESTORATION_BUDGET",
            (super::super::tmux_target::remaining(deadline)?.as_secs_f64() * 2.0 / 3.0).to_string(),
        );
    if let Some(fd) = workspace {
        use std::os::fd::AsRawFd;
        super::super::workspace_lock::inherit_descriptor(fd, &mut command);
        command.env("STORY_WORKSPACE_LOCK_FD", fd.as_raw_fd().to_string());
    }
    let output = crate::process::run_captured_with_input(
        command,
        file,
        super::super::tmux_target::remaining(deadline)?,
    )
    .map_err(|e| AppError::Validation(format!("restoration proof: {}", e.detail())))?;
    let answer: Value = serde_json::from_slice(&output.stdout).map_err(|e| {
        AppError::Validation(format!(
            "restoration response: {e}; {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    })?;
    if !output.status.success() || answer["ok"] != true {
        return Err(AppError::Validation(format!(
            "restoration refused: {}",
            answer["detail"]
        )));
    }
    Ok(answer.get("proposal").filter(|v| !v.is_null()).cloned())
}

/// Derive only physical fields; all native message and story authority survives.
fn rebound_capture(capture: &Value, proposal: &Value) -> Result<Value, StoreError> {
    let metadata = &proposal["metadata"];
    if capture["lease"] != proposal["lease_before"] && capture["lease"] != proposal["lease"] {
        return Err(StoreError::Validation(
            "restored continuation lease conflicts".into(),
        ));
    }
    for key in ["provider", "session_id", "transcript_path"] {
        if capture[key] != metadata[key] || metadata[key].is_null() {
            return Err(StoreError::Validation(format!(
                "restored continuation {key} conflicts"
            )));
        }
    }
    let mut fresh = capture.clone();
    fresh["lease"] = proposal["lease"].clone();
    for key in ["socket", "pane", "window", "pid", "started", "restored"] {
        if let Some(value) = metadata.get(key) {
            fresh[key] = value.clone();
        }
    }
    Ok(fresh)
}

impl<S: Store, D: Dispatcher> EngineService<'_, S, D> {
    /// Publish locally under exclusion, then compare-and-swap all native owners.
    /// If CAS loses, local publication remains an idempotently recoverable prefix.
    pub(super) fn restore_lane(
        &self,
        lane: &EngineLaneRecord,
        seq: Option<i64>,
    ) -> Result<(EngineLaneRecord, bool), AppError> {
        let deadline = Instant::now() + Duration::from_secs(45);
        let Some(proposal) = self.dispatcher.restore_lane(lane, None, None, deadline)? else {
            return Ok((lane.clone(), false));
        };
        let old = lane
            .cleanup_lease
            .as_ref()
            .ok_or_else(|| AppError::Validation("restoration lacks a lease".into()))?;
        let common = proposal["common"].as_str().ok_or_else(|| {
            AppError::Validation("restoration lacks a proven common directory".into())
        })?;
        let workspace = super::super::workspace_lock::WorkspaceLock::try_acquire_proven(
            Path::new(common),
            &old.story_id,
        )?
        .ok_or_else(|| AppError::Validation("restoration workspace is busy".into()))?;
        let project = self.ctx.project();
        let records = self.ctx.store().read(|tx| {
            if !observation_is_current(tx, project, lane, seq)? {
                return Err(StoreError::Validation(
                    "restoration lane or story changed".into(),
                ));
            }
            Ok(tx
                .continuations(project)?
                .into_iter()
                .filter(|r| {
                    r.story_id == old.story_id
                        && r.capture["session_id"] == proposal["metadata"]["session_id"]
                })
                .collect::<Vec<_>>())
        })?;
        let captures = records
            .iter()
            .map(|r| rebound_capture(&r.capture, &proposal))
            .collect::<Result<Vec<_>, _>>()?;
        let published = self.dispatcher.restore_lane(
            lane,
            Some(&proposal),
            Some(workspace.descriptor()),
            deadline,
        )?;
        if published.as_ref() != Some(&proposal) {
            return Err(AppError::Validation(
                "restoration publication changed its proof".into(),
            ));
        }
        let now = self.ctx.now();
        let rebound = self.ctx.store().write(|tx| {
            if !observation_is_current(tx, project, lane, seq)?
                || tx
                    .continuations(project)?
                    .into_iter()
                    .filter(|r| {
                        r.story_id == old.story_id
                            && r.capture["session_id"] == proposal["metadata"]["session_id"]
                    })
                    .collect::<Vec<_>>()
                    != records
            {
                return Err(StoreError::Validation(
                    "restoration native ownership changed concurrently".into(),
                ));
            }
            let mut fresh = lane.clone();
            fresh.cleanup_lease = Some(
                serde_json::from_value(proposal["lease"].clone())
                    .map_err(|e| StoreError::Validation(format!("restoration lease: {e}")))?,
            );
            fresh.pane_id = Some(
                proposal["pane"]
                    .as_str()
                    .filter(|p| valid_pane_id(p))
                    .ok_or_else(|| StoreError::Validation("restoration has invalid pane".into()))?
                    .into(),
            );
            if let Some(identity) = &mut fresh.adopted_identity {
                identity.pane_pid = proposal["identity"]["process"]["pid"]
                    .as_i64()
                    .and_then(|p| i32::try_from(p).ok())
                    .ok_or_else(|| StoreError::Validation("restoration has invalid PID".into()))?;
                identity.window_id = proposal["window"]
                    .as_str()
                    .ok_or_else(|| StoreError::Validation("restoration has no window".into()))?
                    .into();
            }
            for (mut record, capture) in records.clone().into_iter().zip(&captures) {
                if &record.capture != capture {
                    record.capture = capture.clone();
                    super::super::continuation::save(tx, &mut record, &now)?;
                }
            }
            if &fresh != lane {
                tx.put_engine_lane(&fresh)?;
            }
            Ok(fresh)
        })?;
        let permitted = self.ctx.store().read(|tx| {
            if !observation_is_current(tx, project, &rebound, seq)? {
                return Ok(false);
            }
            let prefix = project_prefix(tx, project)?;
            Ok(
                optional_lane_story(tx, project, &prefix, &old.story_id)?.is_some_and(|row| {
                    row.state == "in-progress"
                        && crate::domain::reserved_label(&row.snapshot).is_none()
                }),
            )
        })?;
        if permitted {
            self.dispatcher.rearm_restored_lane(
                &rebound,
                &proposal,
                workspace.descriptor(),
                deadline,
            )?;
        }
        Ok((rebound, true))
    }
}

/// Reconcile retained manual/Auto dispatches that no engine lane owns.
/// This runs only in the background, after the daemon can answer provider hooks.
pub(crate) fn reconcile_manual<S: Store>(store: &S, env: &Environment) -> Result<(), AppError> {
    reconcile_manual_with(store, env, |checkout| {
        super::super::workspace_lock::git(
            checkout,
            &["worktree", "list", "--porcelain", "-z"],
            None,
        )
    })
}

fn reconcile_manual_with<S: Store>(
    store: &S,
    env: &Environment,
    mut inventory: impl FnMut(&Path) -> Result<String, AppError>,
) -> Result<(), AppError> {
    let projects = store.read(|tx| tx.projects())?;
    for project in projects {
        let Some(_automation) = super::super::automations::enter(store, env, project.id)? else {
            continue;
        };
        let Some(checkout) = store.read(|tx| tx.checkout_path(project.id))? else {
            continue;
        };
        let inventory = match inventory(&checkout) {
            Ok(inventory) => inventory,
            Err(error) => {
                crate::daemon::activity::emit(
                    "WARN",
                    "restoration",
                    "event",
                    &format!("project={}", project.slug),
                    &error.to_string(),
                );
                continue;
            }
        };
        for path in inventory
            .split('\0')
            .filter_map(|field| field.strip_prefix("worktree "))
        {
            let lease = match super::super::cleanup_lease::marker_at_registered(Path::new(path)) {
                Ok(Some(lease)) => lease,
                Ok(None) => continue,
                Err(error) => {
                    crate::daemon::activity::emit(
                        "WARN",
                        "restoration",
                        "event",
                        &format!("project={} worktree={path}", project.slug),
                        &error.to_string(),
                    );
                    continue;
                }
            };
            if lease.project_slug != project.slug {
                continue;
            }
            let result = (|| -> Result<(), AppError> {
                let fact = store.read(|tx| {
                    let prefix = project_prefix(tx, project.id)?;
                    if engine_owned(tx, &project.slug, &lease.story_id)? {
                        return Ok(None);
                    }
                    let row = optional_lane_story(tx, project.id, &prefix, &lease.story_id)?
                        .filter(|row| row.superstate != SuperState::Closed);
                    if let Some(row) = &row
                        && !restoration_permitted(tx, project.id, row.story_no)?
                    {
                        return Ok(None);
                    }
                    Ok(row)
                })?;
                let Some(row) = fact else { return Ok(()) };
                // The helper detects the source provider independently. Plugin roots
                // contain both provider adapters; choosing one grants no dispatch.
                let script =
                    crate::api::dispatch::resolve_engine_dispatch_script(EngineAgent::Codex)
                        .map_err(AppError::Storage)?;
                let dispatcher = ShellDispatcher::new(script, env.clone());
                let mut lane = super::idle_lane("restoration-only", 0, &env.now());
                lane.story_id = Some(lease.story_id.clone());
                lane.cleanup_lease = Some(lease.clone());
                let deadline = Instant::now() + Duration::from_secs(45);
                let Some(proposal) = call(&dispatcher, &lane, None, None, false, deadline)? else {
                    return Ok(());
                };
                let common = proposal["common"].as_str().ok_or_else(|| {
                    AppError::Validation(
                        "manual restoration lacks a proven common directory".into(),
                    )
                })?;
                let Some(workspace) =
                    super::super::workspace_lock::WorkspaceLock::try_acquire_proven(
                        Path::new(common),
                        &lease.story_id,
                    )?
                else {
                    return Ok(());
                };
                let current = |tx: &S::ReadTx<'_>| -> Result<bool, StoreError> {
                    Ok(!engine_owned(tx, &project.slug, &lease.story_id)?
                        && tx
                            .story(project.id, row.story_no)?
                            .is_some_and(|fresh| fresh.head_global_seq == row.head_global_seq))
                };
                let records = store.read(|tx| {
                    if !current(tx)? {
                        return Err(StoreError::Validation(
                            "manual restoration ownership changed".into(),
                        ));
                    }
                    Ok(tx
                        .continuations(project.id)?
                        .into_iter()
                        .filter(|r| {
                            r.story_id == lease.story_id
                                && r.capture["session_id"] == proposal["metadata"]["session_id"]
                        })
                        .collect::<Vec<_>>())
                })?;
                let captures = records
                    .iter()
                    .map(|r| rebound_capture(&r.capture, &proposal))
                    .collect::<Result<Vec<_>, _>>()?;
                if call(
                    &dispatcher,
                    &lane,
                    Some(&proposal),
                    Some(workspace.descriptor()),
                    false,
                    deadline,
                )?
                .as_ref()
                    != Some(&proposal)
                {
                    return Err(AppError::Validation(
                        "manual restoration proof changed".into(),
                    ));
                }
                store.write(|tx| {
                    if engine_owned(tx, &project.slug, &lease.story_id)?
                        || tx
                            .story(project.id, row.story_no)?
                            .is_none_or(|fresh| fresh.head_global_seq != row.head_global_seq)
                        || tx
                            .continuations(project.id)?
                            .into_iter()
                            .filter(|r| {
                                r.story_id == lease.story_id
                                    && r.capture["session_id"] == proposal["metadata"]["session_id"]
                            })
                            .collect::<Vec<_>>()
                            != records
                    {
                        return Err(StoreError::Validation(
                            "manual restoration ownership changed concurrently".into(),
                        ));
                    }
                    for (mut record, capture) in records.clone().into_iter().zip(&captures) {
                        if &record.capture != capture {
                            record.capture = capture.clone();
                            super::super::continuation::save(tx, &mut record, &env.now())?;
                        }
                    }
                    Ok(())
                })?;
                if row.state == "in-progress"
                    && crate::domain::reserved_label(&row.snapshot).is_none()
                    && store.read(current)?
                {
                    call(
                        &dispatcher,
                        &lane,
                        Some(&proposal),
                        Some(workspace.descriptor()),
                        true,
                        deadline,
                    )?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                crate::daemon::activity::emit(
                    "WARN",
                    "restoration",
                    "event",
                    &format!("project={} story={}", project.slug, lease.story_id),
                    &error.to_string(),
                );
            }
        }
    }
    Ok(())
}

/// Comments or field edits cannot reactivate dispatch authority from before a toggle.
fn restoration_permitted(
    tx: &impl ReadOps,
    project: crate::store::ProjectId,
    story: crate::store::StoryNo,
) -> Result<bool, StoreError> {
    let events = tx.events_for(project, story)?;
    let generation = events.iter().rev().find_map(|event| {
        matches!(
            event.known(),
            Some(
                crate::domain::StoryEvent::StoryCreated { .. }
                    | crate::domain::StoryEvent::StoryStateChanged { .. }
            )
        )
        .then_some(event.global_seq)
    });
    super::super::automations::permits_generation(tx, project, generation)
}

fn engine_owned(tx: &impl ReadOps, project: &str, story: &str) -> Result<bool, StoreError> {
    for run in tx.engine_runs(project)? {
        if tx
            .engine_lanes(&run.id)?
            .iter()
            .any(|lane| lane.story_id.as_deref() == Some(story))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_project_retained_dispatches_are_not_even_inventoried() {
        let fixture = storyhook_test_support::ServiceFixture::new();
        fixture.add_project("enabled", "EN");
        let store = crate::store::SqliteStore::open(fixture.store().path()).unwrap();
        let ctx = crate::service::Ctx::new(
            &store,
            crate::store::ProjectId::new(fixture.project().get()),
            fixture.cwd(),
            Environment::at(fixture.cwd()),
        )
        .no_hooks(true);
        let settings = crate::service::SettingsService::new(&ctx);
        settings.set("automations.enabled", "false").unwrap();
        let mut observed = Vec::new();
        reconcile_manual_with(&store, ctx.env(), |path| {
            observed.push(path.to_path_buf());
            Ok(String::new())
        })
        .unwrap();
        assert_eq!(observed, [PathBuf::from("/checkouts/enabled")]);
        settings.set("automations.enabled", "true").unwrap();
        observed.clear();
        reconcile_manual_with(&store, ctx.env(), |path| {
            observed.push(path.to_path_buf());
            Ok(String::new())
        })
        .unwrap();
        observed.sort();
        assert_eq!(
            observed,
            [
                PathBuf::from("/checkouts/enabled"),
                PathBuf::from("/checkouts/fixture")
            ]
        );
    }

    #[test]
    fn retained_dispatch_needs_a_fresh_state_generation_after_reenabling() {
        let fixture = storyhook_test_support::ServiceFixture::new();
        let store = crate::store::SqliteStore::open(fixture.store().path()).unwrap();
        let ctx = crate::service::Ctx::new(
            &store,
            crate::store::ProjectId::new(fixture.project().get()),
            fixture.cwd(),
            Environment::at(fixture.cwd()),
        )
        .no_hooks(true);
        let stories = crate::service::StoryService::new(&ctx);
        let story = stories
            .create(&crate::service::NewStoryInput {
                title: "Retained dispatch".into(),
                ..Default::default()
            })
            .unwrap();
        let permitted = || {
            store
                .read(|tx| restoration_permitted(tx, ctx.project(), crate::store::StoryNo::new(1)))
                .unwrap()
        };
        assert!(permitted());
        let settings = crate::service::SettingsService::new(&ctx);
        settings.set("automations.enabled", "false").unwrap();
        settings.set("automations.enabled", "true").unwrap();
        assert!(!permitted());
        stories
            .comment(&story.id, "Manual note, not dispatch authority")
            .unwrap();
        assert!(!permitted());
        stories
            .set_state(&story.id, "in-progress", None, None, None)
            .unwrap();
        assert!(permitted());
    }

    #[test]
    fn expired_restoration_budget_never_starts_readiness_or_publication() {
        let fixture = super::super::super::tmux_target::tests::Fixture::new();
        let mut lane = super::super::restart_probe_tests::adopted_lane(fixture.root.path());
        lane.cleanup_lease.as_mut().unwrap().tmux.socket_path = fixture.socket.clone();
        let dispatcher = ShellDispatcher::new(
            fixture.root.path().join("bin/story.sh"),
            fixture.env.clone(),
        );
        let result = call(
            &dispatcher,
            &lane,
            None,
            None,
            false,
            Instant::now() - Duration::from_secs(1),
        );
        assert!(result.unwrap_err().to_string().contains("budget exhausted"));
        assert!(!fixture.root.path().join("called.json").exists());
    }

    #[test]
    fn capture_rebind_retains_message_authority_and_refuses_foreign_conversation() {
        let before = serde_json::json!({"provider":"codex","session_id":"same","transcript_path":"/transcript","lease":{"old":true},"pane":"%1","pid":7,"turn_id":"turn","message_id":"message","head":"head","fingerprint":"dirty","mode":"plan","compaction_receipt":{"same":true}});
        let mut proposal = serde_json::json!({"lease_before":{"old":true},"lease":{"new":true},"metadata":{"provider":"codex","session_id":"same","transcript_path":"/transcript","pane":"%8","pid":18,"socket":"/new","window":"@9","started":"new","restored":{"uuid":"uuid"}}});
        let after = rebound_capture(&before, &proposal).unwrap();
        assert_eq!(after["pane"], "%8");
        for key in [
            "provider",
            "session_id",
            "turn_id",
            "message_id",
            "head",
            "fingerprint",
            "mode",
            "compaction_receipt",
        ] {
            assert_eq!(after[key], before[key]);
        }
        proposal["metadata"]["session_id"] = "foreign".into();
        assert!(rebound_capture(&before, &proposal).is_err());
    }
}
