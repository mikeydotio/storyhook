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
        let mut view = persistence::read_view(tx, record)?;
        if !view.observations.iter().any(|observation| {
            observation.project == incident.project
                && observation.story == incident.story
                && observation.generation == incident.generation
        }) {
            continue;
        }
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
