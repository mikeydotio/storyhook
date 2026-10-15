# Independent review receipt — story stall matrix

Completed 2026-10-15 UTC. This is a **static documentation review**, not runtime validation or implementation approval.

- Deliverable: [Story stall and recovery decision matrix](../spec/story-stall-decision-matrix.md).
- Source: `mikeydotio/storyhook`, `dev` snapshot `7a06ec099d6af0cae1c329352af54d9972d4e31b`, v3.0.3.
- Local document branch: `docs/stall-decision-matrix-20261015`.
- Final matrix SHA-256: `3a1f346fed651b9695c3df13c714af0db6dc953ae1d6fc37856113191e9d4236`.
- Scope: 45 decision rows; six implementation increments; sixteen historical evidence entries; 122 pinned source links.

## Review method and outcome

Three source auditors supplied independent engine/lifecycle, verifier/integration and resource/process findings. A fourth, fresh reviewer (`stall_matrix_review`) checked the completed document against source, concentrating on E05/E07/E10, V03/V11 and preservation of manual controls, ownership, cancellation and certification. The fresh reviewer reported **no remaining must-fix findings after the correction below**. This does not claim that every hypothetical path has been reproduced or every line received separate independent review.

| Finding | Resolution |
|---|---|
| V11 initially grouped HTTP 401/403 with permanent failures. Source proves definitive refusal, not permanence; some 403 responses are rate limits. | Corrected to diagnostic/capability-based classification. Applicable 403/429 rate limits honor Retry-After and bounded shared backoff. Unknown/GraphQL/transport outcomes remain under V12's uncertainty fence. |
| E07 wording could imply three blocked backlog cards halt a run. | Clarified throughout that the breaker counts local hard stops on already admitted lanes. Blocked backlog alone does not increment it. |
| V03 needed a reachable witness and consideration of enclosing observers/reservations. | Added the unleased Verifying/no-PR stopped-mode witness. Reviewer verified Current refresh repeats; failed reservation withdraws, and authority/cost observers supply no aggregate deadline that would invalidate the path. |
| One source link initially used nonexistent `src/daemon/verification/landing.rs`. | Corrected to `src/service/landing.rs`; all final pinned paths and line bounds validated. |

The reviewer independently confirmed E05's unknown-pane fallback, E10's terminal continuation/monitor/engine mismatch and V11's refusal release followed by later reselection. These remain **code-proven paths**, not newly observed runtime incidents.

## Documentation checks

- All 122 pinned file/line links resolve within the inspected source snapshot.
- All 45 decision IDs are unique; each matrix row has the five declared fields.
- Referenced test-source paths exist. Reading them is not a test run.
- No trailing whitespace found. The new Markdown files were also checked with Git's whitespace checker.
- Changes are limited to the matrix and this receipt; no story was filed or changed. Static review concluded before publication. The subsequent commit and draft PR record publication status; this receipt does not authorize a merge.

No tests, builds, providers, installations, inference, native lane reservations or runtime setting/session changes were performed for this review. Suggested future regression cases and retry timings are proposals. Runtime reproduction, implementation and central acceptance remain future work.

The earlier provider evaluation proposal is shelved. This task has no native job running or reserved; that establishes only this task's non-overlap, not machine-wide lane clearance for BDocs/A21 or another owner.
