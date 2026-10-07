//! Attribution history remains distinct from cost and certification results.
use super::{AttributionRecord, FailureCause};

pub(crate) fn render(records: &[AttributionRecord]) -> String {
    if records.is_empty() {
        return "\nNo retained attribution evidence. Missing evidence means unknown cause.\n"
            .into();
    }
    let mut output = String::from(
        "\n| Attribution | Generation | State | Preparation | Comparison cleanup | Probe starts | Active diagnosis ms |\n|---|---|---|---|---|---:|---:|\n",
    );
    for record in records {
        output.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            cell(&record.id),
            record
                .submission
                .generation
                .map_or_else(|| "unknown".into(), |g| g.get().to_string()),
            if record.held { "held" } else { "retired" },
            record
                .preparation
                .as_ref()
                .map_or("not recorded", |p| if p.unsettled() {
                    "unsettled"
                } else {
                    "settled"
                }),
            record
                .settlement
                .as_ref()
                .map_or("unproved", |s| if s.cleanup_complete {
                    "settled"
                } else {
                    "failed"
                }),
            record.probes.len(),
            record.diagnosis_ms
        ));
    }
    output.push_str("\n| Attribution | Component | Cause observation | Assessment | Original log |\n|---|---|---|---|---|\n");
    for record in records {
        for component in &record.components {
            let assessment = record
                .assessments
                .iter()
                .rev()
                .find(|a| a.component == component.id);
            let cause = assessment.map_or(component.observed_cause, |a| a.cause);
            let source = assessment.map_or_else(
                || "not assessed".into(),
                |a| format!("assessment revision {}", a.evidence_revision),
            );
            output.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                cell(&record.id),
                cell(&component.check),
                label(cause),
                source,
                cell(&component.log)
            ));
        }
    }
    output
}

fn label(cause: FailureCause) -> &'static str {
    match cause {
        FailureCause::CandidateCaused => "candidate-caused",
        FailureCause::SharedProject => "shared-project",
        FailureCause::HostExternal => "host-external",
        FailureCause::Integration => "integration",
        FailureCause::Unknown => "unknown",
    }
}

fn cell(value: &str) -> String {
    value.replace(['\n', '\r'], " ").replace('|', "\\|")
}
