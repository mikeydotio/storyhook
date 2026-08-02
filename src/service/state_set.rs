//! The one path a state set reaches the store by.
//!
//! # Why this module exists rather than a rule inside `put_states`
//!
//! [`crate::store::WriteOps::put_states`] is deliberately raw. It is the seam
//! the store's own tests write damage through — `tests/service_project.rs`
//! writes a catalog with no CLOSED state at all, and the conformance suite
//! writes two-state and duplicated sets — because the guarantees underneath
//! (positions renumbered, a duplicate slug refused as an invariant violation)
//! are only testable against a catalog no service would ever assemble. A
//! product rule enforced there would not merely break those tests; it would
//! delete what they exist to prove.
//!
//! The other candidate, [`crate::domain::validate_state_defs_for_write`], is
//! wrong for the opposite reason: it runs on the legacy write path and inside
//! `MigrationPlan::build`, so a floor enforced there would refuse to migrate
//! every legacy tree written before the floor existed — including the baseline
//! corpus `tests/migrate_round_trip.rs` guards, which is the W4 revert policy's
//! evidence.
//!
//! So the floor lives here, in the service layer, between the two. Every
//! `src/` caller routes through [`write_states`] or [`write_states_repairing`],
//! and `tests/state_set_funnel.rs` fails if a new one does not.
//!
//! # Refuse or repair
//!
//! The two entry points differ only in what they do about a set that does not
//! meet the floor, and the split is about **whose set it is**:
//!
//! * [`write_states`] **refuses**. The set came from a user editing this
//!   project's catalog, so the edit can simply not be made.
//! * [`write_states_repairing`] **repairs**. The set came from a document or a
//!   legacy tree written before the floor existed, and refusing it would make
//!   old data unimportable rather than making it conform.

use crate::domain::{StateDef, validate_required_states, with_required_states};
use crate::store::{ProjectId, StoreError, WriteOps};

/// Writes a state set, refusing one that does not meet the required floor.
///
/// The path a *user edit* takes: `story state add|set|remove|reorder`, the TUI's
/// states editor, and the catalog a new project starts with.
pub(crate) fn write_states(
    tx: &mut impl WriteOps,
    project: ProjectId,
    states: &[StateDef],
) -> Result<(), StoreError> {
    validate_required_states(states).map_err(StoreError::from)?;
    tx.put_states(project, states)
}

/// Writes a state set, adding any required states it lacks.
///
/// The path *foreign data* takes: `story import-project` and `story migrate`,
/// both of which carry a catalog written by something other than this binary.
/// Returns what was actually written, which is the input plus any repair.
///
/// A repair is still not a licence to reinterpret: a required slug present
/// under the wrong superstate is refused here exactly as it would be on a user
/// edit — see [`with_required_states`].
pub(crate) fn write_states_repairing(
    tx: &mut impl WriteOps,
    project: ProjectId,
    states: &[StateDef],
) -> Result<Vec<StateDef>, StoreError> {
    let repaired = with_required_states(states).map_err(StoreError::from)?;
    tx.put_states(project, &repaired)?;
    Ok(repaired)
}
