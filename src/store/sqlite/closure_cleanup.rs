//! Derive cleanup intent from the transaction's original and final projections.
use crate::domain::{StoryEvent, SuperState};
use crate::store::{ClosureCleanup, ProjectId, StoreError, StoryNo};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(super) struct Changes {
    stories: BTreeMap<(ProjectId, StoryNo), (bool, Option<ClosureCleanup>)>,
    projects: BTreeSet<ProjectId>,
}

fn supported(conn: &Connection) -> Result<bool, StoreError> {
    crate::store::migrate::has_columns(
        conn,
        "closure_cleanups",
        &["project_id", "story_no", "token", "record_json"],
    )
}

pub(super) fn read(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
) -> Result<Option<ClosureCleanup>, StoreError> {
    if !supported(conn)? {
        return Ok(None);
    }
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT token,record_json FROM closure_cleanups WHERE project_id=?1 AND story_no=?2",
            params![project.get(), story.get()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    row.map(|(token, json)| {
        let record: ClosureCleanup = serde_json::from_str(&json)?;
        if record.project != project || record.story != story || record.token != token {
            return Err(StoreError::Corrupt(
                "closure cleanup identity disagrees with its key".into(),
            ));
        }
        Ok(record)
    })
    .transpose()
}

pub(super) fn list(
    conn: &Connection,
    project: ProjectId,
) -> Result<Vec<ClosureCleanup>, StoreError> {
    if !supported(conn)? {
        return Ok(Vec::new());
    }
    let stories = conn
        .prepare("SELECT story_no FROM closure_cleanups WHERE project_id=?1 ORDER BY story_no")?
        .query_map([project.get()], |row| Ok(StoryNo::new(row.get(0)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    stories
        .into_iter()
        .map(|story| {
            read(conn, project, story)?.ok_or_else(|| {
                StoreError::Corrupt("closure cleanup disappeared within a read".into())
            })
        })
        .collect()
}

/// A stale worker cannot complete, postpone, or replace another closure.
pub(super) fn update(conn: &Connection, record: &ClosureCleanup) -> Result<bool, StoreError> {
    let Some(current) = read(conn, record.project, record.story)? else {
        return Ok(false);
    };
    if current.token != record.token {
        return Ok(false);
    }
    if current.generation != record.generation
        || (current.lease.is_some() && current.lease != record.lease)
        || (current.completed && !record.completed)
    {
        return Err(StoreError::Invariant(
            "closure cleanup authority cannot change or move backwards".into(),
        ));
    }
    Ok(conn.execute("UPDATE closure_cleanups SET record_json=?1 WHERE project_id=?2 AND story_no=?3 AND token=?4",
        params![serde_json::to_string(record)?,record.project.get(),record.story.get(),record.token])? == 1)
}

/// Effective state includes parents changed solely by a descendant write.
pub(super) fn effective(
    conn: &Connection,
    project: ProjectId,
) -> Result<BTreeMap<StoryNo, (String, SuperState)>, StoreError> {
    let has_epics: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM stories WHERE project_id=?1 AND story_type='epic')",
        [project.get()],
        |row| row.get(0),
    )?;
    if !has_epics {
        return Ok(conn
            .prepare("SELECT story_no,state,superstate='CLOSED' FROM stories WHERE project_id=?1")?
            .query_map([project.get()], |row| {
                Ok((
                    StoryNo::new(row.get(0)?),
                    (
                        row.get(1)?,
                        if row.get::<_, bool>(2)? {
                            SuperState::Closed
                        } else {
                            SuperState::Open
                        },
                    ),
                ))
            })?
            .collect::<Result<_, _>>()?);
    }
    let rows = super::read::stories(conn, project, &crate::store::StoryQuery::all())?;
    Ok(crate::store::effective_states(
        &rows,
        &super::read::states(conn, project)?,
    ))
}

impl Changes {
    pub(super) fn capture_project(
        &mut self,
        conn: &Connection,
        project: ProjectId,
    ) -> Result<(), StoreError> {
        if !supported(conn)? || !self.projects.insert(project) {
            return Ok(());
        }
        let epics = conn
            .prepare("SELECT story_no FROM stories WHERE project_id=?1 AND story_type='epic'")?
            .query_map([project.get()], |row| Ok(StoryNo::new(row.get(0)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        for story in epics {
            let prior = read(conn, project, story)?;
            // A retained request is the prior computed closed lifecycle. Missing
            // migration-era intent is safely backfilled from the final projection.
            self.stories
                .insert((project, story), (prior.is_some(), prior));
        }
        Ok(())
    }

    pub(super) fn capture(
        &mut self,
        conn: &Connection,
        project: ProjectId,
        story: StoryNo,
    ) -> Result<(), StoreError> {
        self.capture_project(conn, project)?;
        if !supported(conn)? || self.stories.contains_key(&(project, story)) {
            return Ok(());
        }
        let closed = conn
            .query_row(
                "SELECT superstate='CLOSED' FROM stories WHERE project_id=?1 AND story_no=?2",
                params![project.get(), story.get()],
                |row| row.get::<_, bool>(0),
            )
            .optional()?
            .unwrap_or(false);
        self.stories
            .insert((project, story), (closed, read(conn, project, story)?));
        Ok(())
    }

    pub(super) fn finish(&self, conn: &Connection) -> Result<(), StoreError> {
        let states = self
            .projects
            .iter()
            .map(|project| Ok((*project, effective(conn, *project)?)))
            .collect::<Result<BTreeMap<_, _>, StoreError>>()?;
        for (&(project, story), (was_closed, prior)) in &self.stories {
            if crate::store::migrate::has_columns(
                conn,
                "project_settings",
                &["automations_enabled"],
            )? && super::read::settings(conn, project)?.automations_enabled == Some(false)
            {
                continue;
            }
            let row = super::read::story(conn, project, story)?;
            let Some(row) = row.filter(|_| {
                states
                    .get(&project)
                    .and_then(|rows| rows.get(&story))
                    .is_some_and(|(_, state)| *state == SuperState::Closed)
            }) else {
                conn.execute(
                    "DELETE FROM closure_cleanups WHERE project_id=?1 AND story_no=?2",
                    params![project.get(), story.get()],
                )?;
                continue;
            };
            // Preserve worker progress written in this transaction; restore the
            // prior receipt only if a projection rebuild cascaded its row away.
            if *was_closed {
                if read(conn, project, story)?.is_some() {
                    continue;
                }
                if let Some(record) = prior {
                    insert(conn, record)?;
                    continue;
                }
            }
            let events = super::read::events_for(conn, project, story)?;
            let lease = events.iter().rev().find_map(|event| match event.known() {
                Some(StoryEvent::StoryCleanupLeaseRecorded { lease, .. }) => {
                    Some(lease.as_ref().clone())
                }
                _ => None,
            });
            let record = ClosureCleanup {
                project,
                story,
                token: uuid::Uuid::new_v4().simple().to_string(),
                generation: row.head_global_seq,
                lease,
                completed: false,
                retry_at: None,
                detail: None,
            };
            insert(conn, &record)?;
        }
        Ok(())
    }
}

fn insert(conn: &Connection, record: &ClosureCleanup) -> Result<(), StoreError> {
    conn.execute("INSERT INTO closure_cleanups(project_id,story_no,token,record_json) VALUES(?1,?2,?3,?4) ON CONFLICT(project_id,story_no) DO UPDATE SET token=excluded.token,record_json=excluded.record_json",
        params![record.project.get(),record.story.get(),record.token,serde_json::to_string(record)?])?;
    Ok(())
}
