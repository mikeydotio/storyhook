//! A single commit boundary fences every durable resource owner, including raw imports.
use crate::store::{ProjectId, StoreError, StoryNo};
use rusqlite::{Connection, OptionalExtension};
use std::collections::BTreeSet;

/// An immutable operation identity; mutable diagnostics are deliberately excluded.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Owner {
    project: i64,
    story: i64,
    kind: &'static str,
    token: String,
    project_identity: String,
}

/// Active owners at entry and explicitly completed identities within this transaction.
#[derive(Default)]
pub(super) struct Ownership {
    before: BTreeSet<Owner>,
    released: BTreeSet<(&'static str, i64, i64, String)>,
}

impl Ownership {
    pub(super) fn begin(conn: &Connection) -> Result<Self, StoreError> {
        Ok(Self {
            before: owners(conn)?,
            released: BTreeSet::new(),
        })
    }

    pub(super) fn release(
        &mut self,
        kind: &'static str,
        project: ProjectId,
        story: StoryNo,
        token: &str,
    ) {
        self.released
            .insert((kind, project.get(), story.get(), token.to_string()));
    }

    pub(super) fn validate(&self, conn: &Connection) -> Result<(), StoreError> {
        let after = owners(conn)?;
        let mut occupied = BTreeSet::new();
        // Helpers can be settling while reset reserves the lane, but still pin its identity.
        for owner in &after {
            if owner.kind != "block delivery" && !occupied.insert((owner.project, owner.story)) {
                return Err(StoreError::Invariant(format!(
                    "story {}/{} has conflicting reset or landing ownership; finish its current operation first",
                    owner.project, owner.story
                )));
            }
        }
        for owner in &self.before {
            if !after.contains(owner)
                && !self.released.contains(&(
                    owner.kind,
                    owner.project,
                    owner.story,
                    owner.token.clone(),
                ))
            {
                return Err(StoreError::Invariant(format!(
                    "{} still owns story {}/{}: operation {} cannot be removed, replaced, or transferred before explicit completion",
                    owner.kind, owner.project, owner.story, owner.token
                )));
            }
        }
        Ok(())
    }
}

fn owners(conn: &Connection) -> Result<BTreeSet<Owner>, StoreError> {
    let mut result = BTreeSet::new();
    for (table, columns, kind, token, predicate) in [
        (
            "dropped_cleanups",
            &["project_id", "story_no", "token", "record_json"][..],
            "dropped cleanup",
            "token",
            "COALESCE(json_extract(record_json,'$.released'),0) != 1",
        ),
        (
            "block_deliveries",
            &["project_id", "story_no", "id", "status"][..],
            "block delivery",
            "CAST(id AS TEXT)",
            "status='attempting'",
        ),
        (
            "story_reset_reservations",
            &["project_id", "story_no", "reservation"][..],
            "native reset",
            "json_extract(reservation,'$.operation')",
            "1",
        ),
        (
            "story_resets",
            &["project_id", "story_no", "record_json"][..],
            "card reset",
            "token",
            "COALESCE(json_extract(record_json,'$.completed'),0) != 1",
        ),
        (
            "engine_resets",
            &["project_id", "story_no", "record_json"][..],
            "engine reset",
            "token",
            "1",
        ),
        (
            "landing_intents",
            &["project_id", "story_no", "payload"][..],
            "landing",
            "id",
            "1",
        ),
    ] {
        // A released stable schema uses this name for its native journal.
        if table == "story_resets" && conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('story_resets') WHERE name='reservation')",[],|row|row.get::<_,bool>(0))? {
            read_owners(conn, "story_resets", "native reset", "json_extract(reservation,'$.operation')", "1", &mut result)?;
            continue;
        }
        if crate::store::migrate::has_columns(conn, table, columns)? {
            read_owners(conn, table, kind, token, predicate, &mut result)?;
        }
    }
    Ok(result)
}

fn read_owners(
    conn: &Connection,
    table: &str,
    kind: &'static str,
    token: &str,
    predicate: &str,
    out: &mut BTreeSet<Owner>,
) -> Result<(), StoreError> {
    let sql = format!("SELECT project_id,story_no,{token} FROM {table} WHERE {predicate}");
    let rows = conn
        .prepare(&sql)?
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (project, story, token) in rows {
        let identity: Option<String> = conn.query_row(
            "SELECT json_array(p.uuid,p.slug,p.prefix,p.checkout_path,s.created_at,CASE WHEN ?3='dropped cleanup' THEN s.state ELSE NULL END) FROM projects p JOIN stories s ON s.project_id=p.id WHERE p.id=?1 AND s.story_no=?2",
            rusqlite::params![project,story,kind], |row|row.get(0)).optional()?;
        let project_identity = identity.ok_or_else(|| {
            StoreError::Invariant(format!(
                "{kind} operation {token} has no owning project/story {project}/{story}"
            ))
        })?;
        out.insert(Owner {
            project,
            story,
            kind,
            token,
            project_identity,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
