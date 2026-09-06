# SH-566 engine-status golden fixtures omitted required output fields

- **Date**: 2026-09-05 PDT / 2026-09-06 UTC
- **Severity/Impact**: PR 652 could not pass centralized verification. No release, user, runtime, or data impact occurred.
- **Status**: Fixed in `93769252a`

## Summary

Central verification of PR 652 found two failing complete-output tests after SH-566 added model, effort, and speed to engine status. The runtime output matched the approved contract, but the corresponding human and JSON snapshot fixtures still represented the previous contract. Commit `93769252a` updated only those two fixtures, and the exact tests plus the complete `golden_cli` target passed. The incident shows that an output-field change must include deliberate review of the complete-output golden target.

## Timeline

- **2026-09-05 14:48:25 PDT** — Feature commit `55605278bfc97d393539fbebe35e0c17e72dfdf8` added model, effort, and speed to engine-run persistence and output. The two engine-status snapshots were not part of the commit.
- **2026-09-05 approximately 18:40:41 PDT** — Central verification reported `golden_cli::engine_status_human` and `golden_cli::engine_status_json` red on merge tree `33b84e63b073090c2805648f46a793dc1b2ed5f6`, blocking PR 652.
- **2026-09-05** — Exact reproduction confirmed that the only differences were the three required default lines in human output and three nullable keys in JSON output. The SH-566 contract, source, and commit history ruled out incorrect runtime output and merge damage.
- **2026-09-05 18:59:23 PDT** — Fix commit `93769252ac9c31bd4a2b8ad724b5cd86deab0d57` refreshed the two snapshot bodies. Both exact tests and all 30 `golden_cli` tests passed.
- **2026-09-05 18:59:59 PDT** — Commit `c3f49f038649208549c8b5d326717a9444d199cb` recorded the repair evidence and resubmission boundary in `HANDOFF.md`.

## Root cause & trigger

Commit `55605278` intentionally expanded `EngineRunView` and its human renderer in `src/output.rs` with model, effort, and speed. The required updates to `tests/snapshots/golden_cli__engine_status_human.snap` and `tests/snapshots/golden_cli__engine_status_json.snap` were missing, so the committed whole-output oracle retained the pre-SH-566 contract. When the centralized verifier exercised the complete `golden_cli` target on merge tree `33b84e6`, the correct new output differed from both stale fixtures and stopped verification.

ODC classification: **Build/Package/Merge / Missing / configuration**. The configuration feature changed the output schema; complete-output verification exposed the missing packaged test-contract data.

## Contributing factors

- The focused pre-submission test set covered the engine, CLI grammar, wire, HTTP, migration, and browser behavior, but did not run the `golden_cli` target.
- Field-level tests established the new behavior without checking the complete rendered output.
- Snapshot acceptance is deliberately manual, so omitted fixture updates remain loud; this correctly prevented a stale contract from passing unnoticed.

## The fix

Commit `93769252a` adds the three approved fields to only the human and JSON snapshot bodies. It changes no runtime code, test logic, or snapshot metadata. This is a **SURGICAL** fix because the defect originated in two missing fixture updates; the runtime implementation and architecture already satisfied the approved contract. Commit `c3f49f038` separately records the verifier repair and handoff evidence.

## Preventative action — killing the class

The complete-output tests `engine_status_human` and `engine_status_json` now encode the default/null representation of model, effort, and speed. Any future engine-status output change must run `cargo test --test golden_cli` as a directly impacted target, making a missing fixture update fail before submission. The contract at `tests/golden_cli.rs:1-12` already states that output diffs require deliberate review and acceptance; no additional `CLAUDE.md` or `AGENTS.md` rule was added because that local, executable contract already exists.

## Lessons

- Adding optional output fields is still a public output-contract change when the human renderer supplies defaults and JSON preserves null values.
- Directly impacted test selection must follow the changed output surface, not only the underlying engine and transport layers.
- Complete-output snapshots are most useful when their failure remains explicit and acceptance remains deliberate.
