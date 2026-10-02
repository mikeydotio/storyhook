# Workspace consent blocked autonomous startup

- **Date**: 2026-10-02 UTC
- **Severity/Impact**: SH-859; first autonomous dispatch into an untrusted workspace could stop before delivering the story charter. Duration and first affected revision are unknown.
- **Status**: Fixed locally in `821558c0`; central verification pending.

## Summary

Claude and Codex can require workspace consent before normal readiness. StoryHook waited for readiness but had no transition to answer that startup dialog. The fix supplies a narrowly authorized consent step during managed autonomous startup. Consent completion still requires every original readiness and delivery check.

## Timeline

| Date (UTC) | Evidence |
| --- | --- |
| Unknown | Introduction and last known-good revision are not established. |
| 2026-09-30 | SH-859 reported the first-launch workspace prompt blocking autonomous work. |
| 2026-10-02 | Baseline replay reproduced Claude `no-sentinel` and Codex `timeout`, with no charter delivered. The trusted readiness control passed. |
| 2026-10-02 | `c2ef5710` corrected provider session IDs in test fixtures. |
| 2026-10-02 | `821558c0` added consent handling and regression coverage. Submission to central verification remained pending. |

## Root cause & trigger

The startup protocol assumed consent had already occurred. In `plugins/story/lib/session.sh`, Claude readiness required its hook sentinel; Codex readiness required its composer before `plugins/story/lib/codex-bootstrap.sh` could initialize hooks. Pending workspace consent withheld those witnesses. Observation alone could not advance the provider, so readiness exhausted its budget.

The trigger was an autonomous launch whose provider had not trusted the workspace. Corrected external-terminal fixtures reproduced this chain through production dispatch. The trusted control distinguished the omission from generic readiness detection failure. Missing hooks remain an independent failure and must still refuse dispatch.

ODC classification: **function/interface; missing startup transition; first-launch workspace consent**. No current live-provider end-to-end compatibility result is claimed.

## Contributing factors

- Both providers expose consent before the evidence that dispatch waits for.
- Claude can select the negative choice initially and protect early input.
- Existing trusted-workspace fixtures did not exercise this startup boundary.
- Provider dialogs are version-sensitive; evidence covers Codex 0.159.3 source and Claude 2.1.287 installed renderer strings.

## The fix

`821558c0` is **SURGICAL**: `startup_trust.py` classifies complete dialogs, and `startup-trust.sh` supplies the missing transition inside existing readiness budgets. Only managed autonomous launches enable it. Before each key, it checks the original process incarnation, provider, pane directory and exact displayed path. Explicit Codex repository-wide trust must match Git's registered primary worktree.

The dialog must remain stable for one second. The handler observes affirmative selection and sends Enter once. It stops before Codex initialization or after Claude's startup gate. Unknown, changed or ambiguous dialogs refuse; sentinel, bootstrap, Plan-mode, charter and rollback checks remain authoritative. See the [startup contract](../spec/workspace-trust-startup.md).

## Preventative action — killing the class

- `test-startup-trust.sh`: 9 parser tests, including exact paths and shared-clock behavior.
- `test-startup-trust-readiness.sh`: 44 production-readiness cases plus 2 unavailable-classifier checks cover protected input, ownership changes, rejection and no replay.
- `test-dispatch-workspace-trust.sh`: 10 provider/mode cases cover Auto, Full Auto, fresh resume, attended refusal and missing-hook refusal.
- `tests/notify_reasons.rs` explicitly inventories `startup_trust_poll` as a key sender with its guard rationale.

Eight directly impacted shell files and 18 Rust contract tests passed. The selector returned `ALL` because its baseline map was absent; the full suite remains the central verifier's responsibility.

## Lessons

Startup consent is a state transition, not readiness evidence. Cross-process deadlines need a shared clock: macOS Python 3.9 monotonic origins differ, so this handler uses `CLOCK_MONOTONIC`. Initial fake socket-prefix handling and fixed session IDs required fixture corrections; those failures were not production regressions.
