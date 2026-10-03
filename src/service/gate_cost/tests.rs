use super::*;

const START: &str = "2026-10-03T00:00:00Z";

#[test]
fn reversed_submillisecond_time_is_unknown_not_zero() {
    assert_eq!(utc_milliseconds("2026-10-03T00:00:00.000001Z", START), None);
    assert_eq!(utc_milliseconds(START, START), Some(0));
    assert_eq!(utc_milliseconds("unknown", START), None);
}

#[test]
fn budget_threshold_is_inclusive_and_sticky() {
    let mut elapsed = Elapsed::new(START);
    elapsed.observe(BUDGET_MS - 1, "2026-10-03T00:14:59.999Z");
    assert_eq!(elapsed.milliseconds, BUDGET_MS - 1);
    assert!(elapsed.breached_at.is_none());
    elapsed.observe(BUDGET_MS, "2026-10-03T00:15:00Z");
    let breach = elapsed.breached_at.clone();
    assert_eq!(breach.as_deref(), Some("2026-10-03T00:15:00Z"));
    elapsed.observe(BUDGET_MS + 1, "2026-10-03T00:15:00.001Z");
    assert_eq!(elapsed.breached_at, breach);
}

#[test]
fn restart_crosses_the_deadline_without_another_gate_event() {
    let mut elapsed = Elapsed::new(START);
    elapsed.observe(899_000, "2026-10-03T00:14:59Z");
    let mut restored: Elapsed =
        serde_json::from_str(&serde_json::to_string(&elapsed).unwrap()).unwrap();
    restored.restart("2026-10-03T00:15:01Z");
    assert_eq!(restored.milliseconds, 901_000);
    assert!(restored.estimated);
    assert!(restored.breached_at.is_some());
    restored.restart("2026-10-03T00:15:01Z");
    assert_eq!(
        restored.milliseconds, 901_000,
        "replay must not double count"
    );
}

#[test]
fn invalid_clock_does_not_erase_known_elapsed_or_breach() {
    let mut elapsed = Elapsed::new(START);
    elapsed.observe(950_000, "2026-10-03T00:15:50Z");
    for at in ["2026-10-02T00:00:00Z", "not-a-time"] {
        elapsed.restart(at);
        assert_eq!(elapsed.milliseconds, 950_000);
        assert!(elapsed.breached_at.is_some());
        assert!(elapsed.diagnostic.is_some());
    }
    elapsed.observe(0, START);
    assert_eq!(elapsed.milliseconds, 950_000);
}
