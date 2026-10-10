# Recovery status elapsed time ignored the service observation clock

- **Date**: 2026-10-10
- **Severity/Impact**: Recovery status could report elapsed time inconsistent with its supplied clock. A fixed-clock fixture returned durations spanning months, and repeated equivalent snapshots differed by 8 ms. Production impact and duration were not measured.
- **Status**: Fixed in `8853437eabd49d601b7da2d8078ca933a845c3ef`; scoped validation complete, final full release validation pending. Tracked under SH-872.

## Summary

Recovery status projections sampled the process wall clock even though their enclosing service snapshot already captured an observation instant from `Ctx`.
This made elapsed output ignore `Clock::Fixed` and made otherwise equivalent snapshots differ.
The repair passes one captured observation instant into project, host and unfinished integration recovery projections while preserving retained start times.
Completed integration elapsed time remains frozen at its retained completion endpoint.
The invariant is that a status projection must use its enclosing snapshot's clock whenever it calculates live elapsed time.

## Timeline

- **2026-10-08 UTC — source introduction:** commit `9e0f91fe` introduced the project recovery path with the omitted observation argument. This attribution comes from source history; no historical first-bad or last-known-good revision was executed.
- **2026-10-10 — observed during SH-872 validation:** `malformed_version_and_authority_rows_coexist_with_valid_rows_in_either_order` failed when complete valid rows differed only in elapsed time by 8 ms.
- **2026-10-10 — isolated and challenged:** an unchanged fixed-clock reproduction failed on original source, passed with the observation-clock intervention, then failed after exact source restoration: exit codes **101 / 0 / 101**.
- **2026-10-10 — repaired:** commit `8853437e` corrected the three projection boundaries, updated direct callers and added precise regression coverage.
- **2026-10-10 — validated and independently reviewed:** 18 focused tests passed. Independent review found no blocking defect or remaining demonstrated sibling in recovery status elapsed projections.
- **2026-10-10 — campaign boundary:** the separate measurement campaign expired without extension, with five failed attempts and zero accepted measurements. This correctness repair does not restart or complete that campaign.

## Root cause & trigger

`src/service/mod.rs` defines a service clock that can supply a fixed instant, making service output comparable.
`src/daemon/verification/status.rs` already captured `ctx.now()` once, but the recovery projection interfaces omitted that argument.
`src/service/project_recovery/status.rs` consequently subtracted the retained recovery start from a fresh `Utc::now()` sample.
The retained start was correct; the observation operand was not the service's requested instant.

The controlled reproduction retained a start of `2026-01-01T00:00:00Z` and supplied observations 1.250 and 2.750 seconds later.
Original and restored source returned approximately 24.39 billion milliseconds; the intervention returned exactly **1250** and **2750** milliseconds.
The reproduction contained no invalid neighboring rows, excluding them as a necessary cause.
Fresh wall-clock samples also explain the original 8 ms drift between equal-context reads.

**ODC classification:** Interface / Missing; the observation instant was omitted from the projection boundary.
**Trigger:** fixed service-clock observations or successive reads expected to share one observation instant.
Independent challenge verified this immediate mechanism; it did not establish production incidence or performance effects.

## Contributing factors

- The projection interfaces accepted retained data without accepting its observation time, leaving a second clock source available inside read-only status code.
- Existing coverage checked presence or absence of timing and whole-row equality without a literal fixed-clock elapsed contract. Whole-row equality exposed the defect but depended on timing between reads.
- Host and unfinished integration recovery projections contained the same direct wall-clock pattern. Their repair required explicit sibling coverage rather than assuming the project experiment proved them.

## The fix

The **SURGICAL** repair in `8853437e` changes four production files:
`src/daemon/verification/status.rs`, `src/service/project_recovery/status.rs`,
`src/service/host_recovery/owner.rs` and `src/service/integration_recovery/owner.rs`.
The caller passes the same captured `now` to all three projections; each live elapsed calculation subtracts its unchanged retained start from that observation.
Direct test callers supply their fixture clock, and negative elapsed durations still clamp to zero.

Observation parsing remains lazy: malformed `now` returns `StoreError::Corrupt` when a valid visible row needs live timing.
Empty, invalid-only and filtered-out collections retain their empty or diagnostic results without requiring an observation instant.
A `Landed` integration row still uses retained `updated_at`, including when a later supplied observation is malformed.
`Clock::System` retains its existing second precision; the milliseconds output unit does not imply a more precise system observation.
There is no schema change, persistence mutation, ownership change or global clock precision change.

Validation recorded **18 passing focused tests**: 17 new or modified cases and the unchanged original mixed-row detector.
Independent review matched all ten changed file hashes to that successful validation and confirmed the original detector was not weakened.
The review covered the clock repair commit, not a later release composition.
Raw experiment logs, source-integrity evidence and validation receipts remain retained with the investigation, outside this public document.
No full-gate pass, historical execution boundary, performance acceptance or 15-minute verification claim follows from this evidence.

## Preventative action — killing the class

- `tests/recovery_status_clock.rs::recovery_status_elapsed_respects_the_context_clock` asserts literal results **[1250, 1250, 1250, 2750, 0]** for repeated, offset-equivalent, later and pre-start observations, preserving the original start. It uses no sleeps or tolerance.
- `native_host_status_elapsed_uses_observation_clock` in `src/service/host_recovery/native/tests.rs` and `managed_integration_status_elapsed_uses_observation_clock` in `src/service/integration_recovery/tests/owner/publication.rs` apply the same literal matrix to sibling paths and preserve stored recovery state.
- `recovery_status_rejects_malformed_observation_for_a_live_row` and `recovery_status_malformed_observation_preserves_empty_and_invalid_diagnostics`, plus host and integration malformed-observation cases, make the lazy validation boundary explicit.
- `managed_completed_owner_retains_cleanup_failure_without_reactivating_effects` verifies the frozen completion endpoint under later and malformed observations while retaining cleanup and ownership assertions.
- The three projection APIs now require an observation argument and document when it is needed. Independent review found no additional recovery elapsed projection bypassing that boundary.

There is no dedicated new malformed-observation case for a resolved-only project collection; review confirmed its explicit filter-before-parse path and judged this gap nonblocking.
The required final full release battery remains outstanding.

## Lessons

A read-only projection still has a clock dependency: elapsed time is a function of both retained data and the requested observation instant.
Thread that instant across the boundary instead of sampling a second clock, and test literal elapsed values independently of scheduling.
Keep completion endpoints and diagnostic-only paths distinct so fixing live timing does not reactivate completed work or hide corrupt-record diagnostics.
