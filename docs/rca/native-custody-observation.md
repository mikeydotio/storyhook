# Native custody observation could abandon a running participant

- **Date:** 2026-10-10
- **Status:** Existing runtime repair isolated from measurement tooling; full release validation remains required.
- **Tracking:** SH-872, with the host custody invariants used by SH-835 and SH-869.

## Summary

A focused retry test eventually printed a passing Rust verdict, but its managed command had already failed with exit 125 after a Darwin process observation returned EPERM. The enclosing supervisor then refused settlement because the lifetime guard was still held. A passing assertion does not certify a failed supervised command.

The isolated release composition omitted production custody repairs that had previously been validated in the measurement draft stack. The repair ports only `scripts/host_admission/native.py` and `scripts/host_admission/supervisor.py`, with focused regressions integrated into core coverage. No measurement collector, campaign policy or deadline changes are included.

## Timeline

- **2026-10-09 — existing repairs:** `5586b32a51fe92723b88cf936a6015e49c8154cf` retained unreadable participants for drainage; `59d4b03e8d65ecdb9269374f466e54d1e279d793` required kernel corroboration of disappeared processes; `5a35a540f3fa6eea51cc632cc2b4c97a96c30fd1` used basic Darwin status for pinned-session liveness.
- **2026-10-10 — isolated composition:** formatting, clippy and 27 supervised focused cases passed before the retry command failed native supervision. Its test later printed one pass. The original failed result and incomplete journals remain retained.
- **2026-10-10 — review and port:** independent review identified the omitted runtime fixes. Six deterministic regression cases failed against the unrepaired runtime and passed after the exact two-file port. A seventh case exercises real managed `ps` completion.

## Root cause and evidence limits

The original supervisor requested full Darwin process identity for every session participant. `_members` caught disappearing processes but allowed other observation errors to escape immediately, leaving descendant settlement unresolved.

Full BSD identity may be unavailable during a process exit transition or during privileged execution. The incident records did not capture the participant's executable, credentials, or corroborating kernel absence result. They do not prove which condition caused this particular EPERM. The regression tests reproduce the supported observation classes independently; they do not retrospectively identify that participant.

## The repair

Authority admission, leader attachment and peer authentication still require full native identity. Basic status is used only to test liveness within an already pinned session. It validates the record size, PID and recognized status, then rechecks session membership. It never grants ownership.

A failed full observation becomes disappearance only when an independent kernel probe confirms ESRCH. Live or denied observations remain failures. Clearing errno before the native call prevents stale errors from becoming current evidence.

An unreadable participant remains in the owned-session set for drainage. The supervisor records observation failure, rechecks session membership before signaling, and refuses success even if the child exits successfully. Malformed-record and census failures still fail closed; this port adds participant-OSError drainage rather than claiming recovery from every observation fault.

## Prevention and validation

`scripts/tests/test_host_native_observation.py` retains five native observation cases and one unreadable-member drainage case from the original repair. A real macOS `ps` case requires successful managed completion and a finished custody record with no attached execution. The mock cases cover denied, truncated, mismatched, stale-errno, vanished, zombie and changed-session observations. The real case does not guarantee sampling during a privileged interval.

`tests/host_admission.rs::host_admission_native_observation_preserves_custody` runs those seven cases through the existing core test entry. It does not import measurement modules. The affected retry scenario must also complete with successful supervision. The seven-case contract and affected retry scenario passed with successful command supervision. Formatting, all-target clippy and the test-seam build also passed on the final source. Independent review found no blocker. The complete release battery remains outstanding.

## Lessons

Use the minimum observation needed for an already-established custody relationship without weakening the full identity needed to establish that relationship. Preserve uncertainty as failure and retain descendants through cleanup. Keep assertion outcomes, command completion and custody settlement as separate facts.
