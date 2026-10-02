//! Text-only legacy incidents cannot establish project ownership or cleanup.
use super::persistence;
use crate::store::{ProjectId, StoreError, VerificationFailureDisposition, WriteOps};

pub(crate) fn reconcile_incident(
    tx: &mut impl WriteOps,
    project: ProjectId,
    now: &str,
) -> Result<bool, StoreError> {
    let Some(incident) = tx.verification_incident(project)? else {
        return Ok(false);
    };
    if !incident.halted || incident.disposition != VerificationFailureDisposition::Permanent {
        return Ok(false);
    }
    for record in tx.project_recoveries(project)? {
        // Only a record whose own observation corroborates the incident can
        // take it, so only that record is validated: an invalid record that
        // cannot take it must not fail every verifier tick (SH-848).
        if !tx
            .project_recovery_observations(project, &record.id)?
            .iter()
            .any(|observation| {
                observation.project == incident.project
                    && observation.story == incident.story
                    && observation.generation == incident.generation
            })
        {
            continue;
        }
        let mut view = persistence::read_view(tx, record)?;
        // This receipt was written only after the verifier settled owned work.
        // Retain the full incident before releasing its queue-wide authority.
        if !view.state.legacy_incidents.contains(&incident) {
            view.state.legacy_incidents.push(incident.clone());
            persistence::save(tx, &mut view, now)?;
        }
        return tx.clear_verification_incident(&incident.incident_id);
    }
    Ok(false)
}
