# Obsolete repair ownership text caused a false retry-shell failure

- **Date**: 2026-10-10
- **Severity/Impact**: One retry-shell integration scenario falsely failed during SH-872 baseline validation. No live board corruption was demonstrated. The broader baseline remains failed.
- **Status**: Fixed in `4501f8a33593c6d6472a6cb14d8e09803c91203e`.

## Summary

The red head-convergence retry scenario required an obsolete ownership sentence in a persisted attribution-hold comment. Shared project recovery had qualified the production sentence to distinguish implementer repair from independently managed repair, but the test still expected the earlier wording. A controlled red/green/red experiment established the mismatch as the immediate cause. The correction preserves the independent ownership assertion and adds actual-comment diagnostics; its focused scenario passes, without establishing a full baseline pass.

## Timeline

- **2026-10-08, 05:54 UTC** — Commit `9e0f91fef8d7f9ceea7fdbf5fff5e25f03580f93` changed both daemon Held comment producers to “No implementer repair is assigned by this held result.” The retry-shell predicate retained “No repair is assigned”. This attribution comes from source history; no historical checkout was executed.
- **2026-10-10, approximately 03:02 UTC** — SH-872 baseline source `5e0398a0d047fc987860719d881f9e58426299d3` failed `real_head_convergence_retry_reports_running_then_preserves_red`. The outer test and nested worker reported the same failure, not two independent defects.
- **2026-10-10** — Focused reproduction on source `0eb86eee7729f35717f8e6f2dadf737015c849d3` failed at the same predicate. Runtime comment capture and a wording-only experiment produced exits `101`, `0`, `101`; independent review verified the ownership contract.
- **2026-10-10, 03:58 UTC** — The final changed scenario passed once, with zero failures. Commit `4501f8a33593c6d6472a6cb14d8e09803c91203e` records the correction and diagnostics.

## Root cause & trigger

The production ownership statement changed while an exact integration expectation remained stale. On a red retry outcome, diagnosis retained an attribution hold and persisted the current comment; the test then rejected that valid comment solely because its expected sentence no longer matched.

Both diagnostic red executions captured the new sentence. Changing only the expected sentence passed the same scenario, including subsequent gate-detail, unchanged-main and admission-callback checks; restoring the old predicate failed again. Earlier settlement, state, hold and queue checks passed in the failing executions. This rules out an absent comment as the immediate cause and gives **high confidence** in the wording mismatch, with one run per experiment leg.

The production distinction is supported by [verification workflow](../spec/verification-workflow.md) and [shared project recovery](../spec/shared-project-recovery.md): an attribution hold does not assign ordinary implementer repair, while independently managed recovery may own a repair. `src/daemon/verification/diagnosis/execute.rs` returns Held in both supported shared-recovery and unsupported-evidence paths. The reproduced scenario exercises unsupported evidence; it does not establish successful shared-recovery enrollment.

**ODC classification:** Checking / Incorrect. **Trigger:** recovery/error path. The fifth baseline attempt reached this red scenario after earlier attempts had stopped elsewhere.

## Contributing factors

- The red outcome had to reach the Held result and then the obsolete comment predicate.
- The old assertion printed its predicate without the persisted comments, delaying direct comparison of expected and actual ownership text.
- Similar sentences exist in administrative and batch hold contexts. Their different ownership contracts make a global wording replacement inappropriate.

## The fix

Commit `4501f8a33593c6d6472a6cb14d8e09803c91203e` changes only the assertion in `tests/verification_retry_shell.rs`. A single persisted comment must still contain both `CENTRAL VERIFICATION ATTRIBUTION HELD` and the independently written ownership sentence. The assertion now prints actual comments on failure. All surrounding behavior checks remain intact; production behavior, persistence and interfaces are unchanged.

The **SURGICAL** verdict follows the demonstrated test-boundary defect. The exact changed scenario passed once in 28.25 test seconds; Rust 2024 formatting also passed. No broad suite was run for this correction. The separate recovery-clock defect and full baseline failure remain open, and no rate or performance conclusion follows from this focused pass.

## Preventative action — killing the class

The existing `real_head_convergence_retry_reports_running_then_preserves_red` integration scenario retains an independent expectation of the ownership explanation. Missing comments, missing held markers or missing qualified ownership wording still fail, and actual-comment diagnostics make future mismatches visible through the existing nested-worker log. The expectation is not imported from a production constant, so unintended wording changes remain detectable.

A scoped static sweep of `tests`, `src` and `docs` found no remaining demonstrated stale expectation of this kind. Distinct administrative and batch wording was preserved. This check improves detection; it does not prove the defect class impossible or exhaustively cover generated or dynamically assembled text.

## Lessons

Ownership wording can express a behavioral contract even when the change looks editorial. Verify that contract before changing either the producer or its independent integration assertion. When a nested test compares persisted output, include the actual output in failure diagnostics so a contract mismatch can be distinguished from missing state without weakening the check.
