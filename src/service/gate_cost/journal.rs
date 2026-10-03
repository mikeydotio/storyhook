//! Incremental import of attempt-bound progress; records cannot certify a tree.
use crate::store::{
    GateAttempt, GateExecution, GateFailedCase, GateInputs, GateInterval, GateLeg, GateSubmission,
};
use serde_json::Value;

/// Imports complete lines, leaving a concurrently written tail unconsumed.
pub fn ingest(admission: &mut GateAttempt, execution_id: &str, text: &str) {
    let Some(attempt) = admission
        .executions
        .iter_mut()
        .find(|e| e.id == execution_id)
    else {
        admission.diagnostics.push(format!(
            "journal refers to unknown execution {execution_id}"
        ));
        return;
    };
    for line in text.split_inclusive('\n') {
        if !line.ends_with('\n') {
            break;
        }
        attempt.journal_offset += line.len() as u64;
        if let Err(error) = import(&admission.id, &admission.submission, attempt, line) {
            let detail = format!("journal byte {}: {error}", attempt.journal_offset);
            if !attempt.diagnostics.contains(&detail) {
                attempt.diagnostics.push(detail);
            }
        }
    }
}

fn import(
    admission: &str,
    submission: &GateSubmission,
    attempt: &mut GateExecution,
    line: &str,
) -> Result<(), String> {
    let row: Value =
        serde_json::from_str(line).map_err(|e| format!("malformed complete record: {e}"))?;
    let kind = field(&row, "kind")?;
    if kind == "run" {
        attempt.journal_bound = row["attempt_id"].as_str() == Some(admission)
            && row["execution_id"].as_str() == Some(&attempt.id)
            && row["generation"].as_i64() == submission.generation.map(|g| g.get());
        return if attempt.journal_bound {
            Ok(())
        } else {
            Err("foreign attempt or generation".into())
        };
    }
    if !attempt.journal_bound {
        return Err("record has no authenticated run identity".into());
    }
    match kind {
        "case" if row["outcome"] == "fail" => attempt.failed_cases.push(GateFailedCase {
            path: field(&row, "path")?.into(),
            name: optional(&row, "name"),
            target: optional(&row, "target"),
            identity: optional(&row, "identity"),
            title_path: row
                .get("title_path")
                .map(|value| serde_json::from_value(value.clone()))
                .transpose()
                .map_err(|e| format!("invalid case title path: {e}"))?,
        }),
        "item" => {
            let path = field(&row, "path")?;
            let leg = GateLeg {
                path: path.into(),
                status: field(&row, "status")?.into(),
                milliseconds: row["milliseconds"]
                    .as_u64()
                    .or_else(|| row["seconds"].as_u64().and_then(|s| s.checked_mul(1000))),
                receipt: optional(&row, "receipt").filter(|s| !s.is_empty()),
            };
            if let Some(old) = attempt.legs.iter_mut().find(|l| l.path == path) {
                *old = leg;
            } else {
                attempt.legs.push(leg);
            }
        }
        "cost" => interval(attempt, &row)?,
        "context" => {
            let inputs: GateInputs = serde_json::from_value(row["inputs"].clone())
                .map_err(|e| format!("invalid gate input evidence: {e}"))?;
            if !attempt.inputs.preserved_by(&inputs) {
                return Err("gate input identity changed within one execution".into());
            }
            attempt.inputs = inputs;
        }
        "output" => {
            if row["attempt_id"].as_str() != Some(admission) {
                return Err("foreign raw output binding".into());
            }
            let path = field(&row, "path")?.to_string();
            if !attempt.logs.contains(&path) {
                attempt.logs.push(path);
            }
        }
        _ => {}
    }
    Ok(())
}

fn interval(attempt: &mut GateExecution, row: &Value) -> Result<(), String> {
    let id = field(row, "id")?;
    let phase = field(row, "phase")?;
    let path = field(row, "path")?;
    let at = field(row, "at")?;
    if !matches!(
        phase,
        "workspace"
            | "resource-wait"
            | "discovery"
            | "compile-link"
            | "execution"
            | "cleanup"
            | "verdict"
    ) {
        return Err(format!("unknown cost phase {phase}"));
    }
    match field(row, "event")? {
        "start" => {
            if attempt.intervals.iter().any(|i| i.id == id) {
                return Err(format!("repeated cost start {id}"));
            }
            attempt.intervals.push(GateInterval {
                id: id.into(),
                path: path.into(),
                phase: phase.into(),
                started_at: Some(at.into()),
                ended_at: None,
                milliseconds: None,
                estimated: false,
                started_monotonic_ns: row["monotonic_ns"].as_u64(),
            });
        }
        "end" => {
            let Some(interval) = attempt.intervals.iter_mut().find(|i| i.id == id) else {
                return Err(format!("cost end without start {id}"));
            };
            if interval.path != path || interval.phase != phase || interval.ended_at.is_some() {
                return Err(format!("mismatched cost end {id}"));
            }
            interval.ended_at = Some(at.into());
            interval.milliseconds =
                match (interval.started_monotonic_ns, row["monotonic_ns"].as_u64()) {
                    (Some(start), Some(end)) => end.checked_sub(start).map(|n| n / 1_000_000),
                    _ => {
                        interval.estimated = true;
                        interval
                            .started_at
                            .as_deref()
                            .and_then(|start| super::utc_milliseconds(start, at))
                    }
                };
            if interval.milliseconds.is_none() {
                return Err(format!("invalid cost clock {id}"));
            }
        }
        other => return Err(format!("unknown cost event {other}")),
    }
    Ok(())
}

fn field<'a>(row: &'a Value, key: &str) -> Result<&'a str, String> {
    row[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing or invalid {key}"))
}

fn optional(row: &Value, key: &str) -> Option<String> {
    row[key].as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{GateSubmission, GlobalSeq, ProjectId};

    fn attempt() -> GateAttempt {
        GateAttempt::new(
            "a".into(),
            GateSubmission {
                project: ProjectId::new(1),
                story_id: "SH-1".into(),
                generation: Some(GlobalSeq::new(7)),
                submitted_at: None,
            },
            "2026-10-03T00:00:00Z",
        )
    }

    fn ingest(a: &mut GateAttempt, text: &str) {
        if a.executions.is_empty() {
            a.executions.push(GateExecution::new(
                "e".into(),
                "2026-10-03T00:00:00Z",
                "/tmp/e.ndjson".into(),
            ));
        }
        super::ingest(a, "e", text);
    }

    #[test]
    fn binding_is_required_and_only_complete_lines_are_consumed() {
        let mut a = attempt();
        ingest(
            &mut a,
            "{\"kind\":\"run\",\"attempt_id\":\"foreign\",\"generation\":7}\n{\"kind\":\"case\",\"outcome\":\"fail\",\"path\":\"unit\",\"name\":\"wrong\"}\n",
        );
        assert!(!a.executions[0].journal_bound);
        assert!(a.executions[0].failed_cases.is_empty());
        assert!(!a.executions[0].diagnostics.is_empty());
        ingest(
            &mut a,
            "{\"kind\":\"run\",\"attempt_id\":\"a\",\"execution_id\":\"e\",\"generation\":7}\n",
        );
        let before = a.executions[0].journal_offset;
        ingest(&mut a, "{\"kind\":\"case\"");
        assert_eq!(a.executions[0].journal_offset, before);
        assert!(a.executions[0].journal_bound);
    }

    #[test]
    fn exact_failures_reuse_and_phase_measurements_survive_import() {
        let mut a = attempt();
        let lines = [
            serde_json::json!({"kind":"run", "attempt_id":"a", "execution_id":"e", "generation":7}),
            serde_json::json!({"kind":"case", "outcome":"fail", "path":"unit", "name":"parse::bad", "target":"parser"}),
            serde_json::json!({"kind":"item", "status":"reused", "path":"lint", "receipt":"fingerprint"}),
            serde_json::json!({"kind":"cost", "id":"compile", "phase":"compile-link", "path":"unit", "event":"start", "at":"2026-10-03T00:00:01Z", "monotonic_ns":1000000}),
            serde_json::json!({"kind":"cost", "id":"compile", "phase":"compile-link", "path":"unit", "event":"end", "at":"2026-10-03T00:00:02Z", "monotonic_ns":4000000}),
        ];
        ingest(
            &mut a,
            &lines.iter().map(|v| format!("{v}\n")).collect::<String>(),
        );
        assert_eq!(
            a.executions[0].failed_cases[0].name.as_deref(),
            Some("parse::bad")
        );
        assert_eq!(
            a.executions[0].legs[0].receipt.as_deref(),
            Some("fingerprint")
        );
        assert_eq!(
            a.executions[0].legs[0].milliseconds, None,
            "reuse is not zero execution"
        );
        assert_eq!(a.executions[0].intervals[0].milliseconds, Some(3));
        assert_eq!(a.verdict, None, "producer data cannot certify");
    }

    #[test]
    fn malformed_complete_lines_and_missing_phase_end_remain_unknown() {
        let mut a = attempt();
        ingest(
            &mut a,
            "{\"kind\":\"run\",\"attempt_id\":\"a\",\"execution_id\":\"e\",\"generation\":7}\nnot json\n",
        );
        let start = serde_json::json!({"kind":"cost", "event":"start", "id":"run", "phase":"execution", "path":"unit", "at":"2026-10-03T00:00:01Z"});
        ingest(&mut a, &format!("{start}\n"));
        assert_eq!(a.executions[0].intervals[0].milliseconds, None);
        assert!(
            a.executions[0]
                .diagnostics
                .iter()
                .any(|s| s.contains("malformed complete record"))
        );
        let end = serde_json::json!({"kind":"cost", "event":"end", "id":"run", "phase":"execution", "path":"unit", "at":"2026-10-03T00:00:03Z"});
        ingest(&mut a, &format!("{end}\n"));
        assert_eq!(a.executions[0].intervals[0].milliseconds, Some(2000));
        assert!(a.executions[0].intervals[0].estimated);
        ingest(&mut a, &format!("{end}\n"));
        assert_eq!(a.executions[0].intervals.len(), 1);
        assert!(
            a.executions[0]
                .diagnostics
                .iter()
                .any(|s| s.contains("mismatched cost end"))
        );
    }
}
