# Live storage scans aborted compilation when temporary files disappeared

- **Date**: 2026-10-09
- **Severity/Impact**: One SH-872 baseline attempt was interrupted during compilation; it produced no accepted timing sample.
- **Status**: Fixed in `d272cc118874b08943f8363b7152021427460eb7`; full baseline validation remains pending.

## Summary

The SH-872 storage observer raised `FileNotFoundError` while scanning a compiler object file, causing the supervised gate to stop. The observer assumed that an enumerated descendant would still exist when inspected, although compilation changes the same directory concurrently. The repair makes live descendant disappearance explicit while binding directory traversal to admitted identities. A live resource observation must tolerate ordinary churn without relaxing root identity or sensor error checks.

## Timeline

- **2026-10-09 02:13 UTC** — `a23a8657b334c5038b6b871319bed1a7e8a905f1` introduced the storage guard with the descendant lifetime assumption.
- **2026-10-09 02:51 UTC** — `156b47a981afd1e57a21077ca24b78eb59813f39` established the enumeration-then-stat implementation implicated by the reproduction.
- **2026-10-09 23:05 UTC** — The SH-872 baseline at `0eddc9339e07af704c6d8331e5e8f70d4c4a5231` stopped during Rust compilation after formatting and clippy passed. Its storage observation reported a missing `rcgu.o` descendant.
- **2026-10-09** — A deterministic reproduction deleted a real temporary file between enumeration and stat. It reproduced the exception without replacing the root or exhausting storage; independent diagnosis confirmed the lifetime defect.
- **2026-10-09 23:25 UTC** — `d272cc118874b08943f8363b7152021427460eb7` committed the repair and regression coverage. The 18 new Python tests passed; the dedicated Rust wrapper and full baseline remained subsequent validation.

## Root cause & trigger

The verified chain is:

1. `usage` in [scripts/gate_measurement_storage.py](../../scripts/gate_measurement_storage.py) enumerated names, then inspected them after closing the enumeration context. It treated those names as durable entries.
2. A descendant removed in that interval made its stat fail with `ENOENT`. The real-file reproduction directly demonstrates this interleaving.
3. The exception escaped `check_storage` and the campaign's `health` callback in [scripts/gate_measurement_campaign.py](../../scripts/gate_measurement_campaign.py).
4. `OwnedGate` in [scripts/gate_measurement_execution.py](../../scripts/gate_measurement_execution.py) cancelled its supervised gate on the observer failure. The interrupted attempt remained a failure, not a completed sample.

**ODC classification:** checking defect; missing concurrency lifetime validation; triggered by descendant removal during a live observation. The historical compiler failure matches the reproduced signature, but its exact unlinking actor was not traced. This was the first repaired baseline attempt to reach Rust compilation; no known-good complete baseline was available.

## Contributing factors

- The periodic health observer scanned the same mutable tree used by compilation.
- Directory descent also used deferred paths, leaving an adjacent disappearance or substitution race.
- Existing storage tests covered caps and root substitution, but lacked deletion between enumeration and inspection.
- The observer's strict failure behavior correctly stopped uncertain measurements, amplifying the missing live-tree contract into a cancelled gate.

## The fix

**SURGICAL:** commit `d272cc118874b08943f8363b7152021427460eb7` localizes the correction to the storage boundary and its tests.

`usage` now distinguishes live observation from strict settled scans. It anchors traversal to directory descriptors, opens descendants relative to their parents without following links, checks directory identity around traversal, and closes descriptors on success and failure. Live scans tolerate vanished descendants; an unexplained directory enumeration error still fails. Missing or substituted admitted roots, permission failures, and other sensor errors remain fatal.

`check_storage` requests live scans and rechecks campaign and target identities after scanning. Disposal callers in [scripts/gate_measurement_targets.py](../../scripts/gate_measurement_targets.py) retain strict behavior. Pinned dependency-input discovery remains unchanged because mutation there invalidates the inputs.

Allocated-block accounting, exclusions, link/socket policy, caps, and remaining-growth headroom arithmetic are unchanged. This remains a non-atomic observation that can miss newly created entries; it does not establish an exact quota or a successful performance result.

## Preventative action — killing the class

- [StorageChurn.test_removed_enumerated_object_does_not_abort_stable_file_accounting](../../scripts/tests/test_gate_measurement_storage_churn.py) deletes a real file after enumeration and verifies the surviving file's allocated bytes.
- The same 18-test module covers nested disappearance, directory and root substitution, strict settled scans, error propagation, descriptor closure, exclusions, evidence policy, and storage bounds.
- [measurement_live_storage_churn_regressions](../../tests/gate_measurement.rs) adds the module to the Rust integration test harness. At this postmortem's validation boundary, the Python module passed; the new wrapper had not yet run.
- The storage API explicitly documents that descendants may vanish during live scans while admitted roots may not. Identity checks make violations fail visibly instead of returning successful partial usage.

## Lessons

Enumeration observes names at an instant; it does not grant those entries a lifetime. Live observers and settled disposal need separate mutation contracts even when they share accounting code. Preserve failed measurement evidence and charge interrupted attempts while repairing the observer; regression success alone does not establish a baseline result.
