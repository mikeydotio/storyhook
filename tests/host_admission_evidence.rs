//! Admission observations extend gate evidence without changing certification.

use serde_json::{Value, json};
use storyhook::service::gate_cost::journal::ingest;
use storyhook::store::{GateAttempt, GateExecution, GateSubmission, GlobalSeq, ProjectId};

fn attempt() -> GateAttempt {
    let mut value = GateAttempt::new(
        "attempt".into(),
        GateSubmission {
            project: ProjectId::new(1),
            story_id: "SH-1".into(),
            generation: Some(GlobalSeq::new(7)),
            submitted_at: None,
        },
        "2026-10-03T00:00:00Z",
    );
    value.executions.push(GateExecution::new(
        "gate".into(),
        "2026-10-03T00:00:00Z",
        "/tmp/unused.ndjson".into(),
    ));
    ingest(
        &mut value,
        "gate",
        "{\"kind\":\"run\",\"attempt_id\":\"attempt\",\"execution_id\":\"gate\",\"generation\":7}\n",
    );
    value
}

fn event(sequence: u64) -> Value {
    json!({"version":1,"authority":"broker","sequence":sequence,"host":"host",
        "boot":"boot","policy":"a".repeat(64),"at":1000 + sequence,
        "event":"grant","lease":"lease","parent":null,"project":"p","work":"test",
        "resources":{"cpu":1000,"memory":4096},"wait_ms":sequence,
        "binding":{"attempt_id":"attempt","execution_id":"gate","generation":7}})
}

fn import(value: &mut GateAttempt, event: Value) {
    let record = json!({"kind":"resource","attempt_id":"attempt","execution_id":"gate",
        "generation":7,"observation":event});
    ingest(value, "gate", &format!("{record}\n"));
}

fn observations(value: &GateAttempt) -> Vec<Value> {
    serde_json::to_value(&value.executions[0]).unwrap()["resource_events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[test]
fn resource_replay_keeps_ordered_peaks_without_changing_inputs_or_verdict() {
    let mut a = attempt();
    a.executions[0].inputs.resources = Some(json!({"policy":"immutable launch context"}));
    let inputs = a.executions[0].inputs.clone();
    import(&mut a, event(1));
    let mut peak = event(2);
    peak["event"] = json!("usage");
    peak["sample"] = json!({"cpu":600,"memory":3000});
    peak["peaks"] = json!({"cpu":650,"memory":3500});
    import(&mut a, peak.clone());
    import(&mut a, event(1));
    import(&mut a, peak.clone());
    assert_eq!(observations(&a), vec![event(1), peak]);
    assert_eq!(a.executions[0].inputs, inputs);
    assert_eq!(a.verdict, None);
    assert!(a.executions[0].diagnostics.is_empty());
    a.validate().unwrap();
}

#[test]
fn foreign_malformed_or_changed_evidence_is_diagnostic_not_authority() {
    let mut a = attempt();
    import(&mut a, event(2));
    let mut foreign = event(3);
    foreign["binding"]["generation"] = json!(8);
    import(&mut a, foreign);
    let mut malformed = event(3);
    malformed["resources"]["cpu"] = json!(-1);
    import(&mut a, malformed);
    let mut changed = event(2);
    changed["wait_ms"] = json!(999);
    import(&mut a, changed);
    import(&mut a, event(1));
    assert_eq!(observations(&a), vec![event(2)]);
    assert_eq!(a.executions[0].diagnostics.len(), 4);
    assert_eq!(a.verdict, None);
}

#[test]
fn outer_binding_and_host_wide_pressure_are_checked_independently() {
    let mut a = attempt();
    let record = json!({"kind":"resource","attempt_id":"foreign","execution_id":"gate",
        "generation":7,"observation":event(1)});
    ingest(&mut a, "gate", &format!("{record}\n"));
    let pressure = json!({"version":1,"authority":"broker","sequence":2,"host":"host",
        "boot":"boot","policy":"a".repeat(64),"at":1002,"event":"pressure",
        "lease":null,"reason":"sensor unavailable","sample":null});
    import(&mut a, pressure.clone());
    assert_eq!(observations(&a), vec![pressure]);
    assert_eq!(a.executions[0].diagnostics.len(), 1);
}

#[test]
fn old_gate_records_remain_readable_and_bad_retained_events_are_refused() {
    let mut serialized = serde_json::to_value(attempt()).unwrap();
    serialized["executions"][0]
        .as_object_mut()
        .unwrap()
        .remove("resource_events");
    let old: GateAttempt = serde_json::from_value(serialized.clone()).unwrap();
    assert!(observations(&old).is_empty());
    serialized["executions"][0]["resource_events"] = json!([event(1), event(1)]);
    let bad: GateAttempt = serde_json::from_value(serialized).unwrap();
    assert!(
        bad.validate().is_err(),
        "duplicate retained evidence must fail validation"
    );
}

#[test]
fn native_broker_publication_is_consumed_by_the_gate_importer() {
    let script = r#"
import sys
sys.path.insert(0, sys.argv[1])
from test_host_admission_system import BrokerTests
fixture = BrokerTests()
try:
    fixture.setUp()
    print(fixture.publication(), end='')
finally:
    fixture.doCleanups()
"#;
    let output = std::process::Command::new("python3")
        .args(["-B", "-c", script])
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/tests"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut a = attempt();
    ingest(&mut a, "gate", &String::from_utf8(output.stdout).unwrap());
    assert!(
        a.executions[0].diagnostics.is_empty(),
        "{:?}",
        a.executions[0].diagnostics
    );
    let events = observations(&a);
    assert!(events.iter().any(|v| v["event"] == "grant"));
    assert!(events.iter().any(|v| v["event"] == "release"));
    a.validate().unwrap();
    assert_eq!(a.verdict, None);
}
