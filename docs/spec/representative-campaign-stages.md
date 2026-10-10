# Staged representative-load campaign

Decision, 2026-10-09: retain the existing 18-slot SH-872 protocol and ceilings.
The idle-only criterion is amended; cohort size is unchanged. The exploratory
15-attempt alternative is deferred and is not a required decision. This document
is preparation, not an execution start receipt or merge authorization.

## Source and native readiness before the campaign clock starts

1. Compose one isolated integration tree containing current dev and exact reviewed
   PR990 (Cargo fingerprints), PR991, PR992 and PR993 heads. Record source parents,
   resulting commit/tree and clean status. Preserve original author worktrees.
   If dev moves, inspect the delta and repeat only affected checks; do not silently
   change the frozen baseline after its measurement window starts.
2. After the coordinator releases BDocs' native lane, run the two affected
   `gate_measurement` Rust wrappers, serially, through retained child custody.
   They cover the new pure exposure/deadline cases plus disposable native
   owner/class/probe/cancellation/C-W-R/target-turnover fixtures. Use the normal
   managed Cargo wrapper, offline locked dependencies, `test-seam`, one Cargo
   job and one test thread. No complete `make test` in this preparation phase.
3. Verify the newly built binary's embedded module, version/build tree and hash;
   observe real CPU ticks, memory pressure, swap, process census and storage.
   Freeze current tools/dependency/configuration/environment/resource limits and
   complete byte identities using the existing input preflight. Python changed
   since the old freeze: historical hashes cannot authorize this run.
4. Review the source/binary/input proof and assign a real campaign start. Use a
   new private output root and a start receipt naming that exact root/revision
   and the real coordinator authority. The expired old controller/root/receipt
   is not reused. No idle observation loop or autonomous scheduler is installed.

## Baseline window: slots 1–9

At the actual baseline reservation, set a continuous same-boot campaign end of
start + 20 hours. Baseline ends no later than start + 10 hours. Initial preparation
is at most 40 minutes within that window; project lock wait remains 60 seconds.

Run three blocks, each exactly **C, W, R**:

| Baseline slots | Block | Target and execution |
|---|---:|---|
| 1, 2, 3 | 1 | Fresh target; C executes all detectors, W executes again with warm artifacts and no verdict reuse, R reuses only the immediately preceding successful W evidence. |
| 4, 5, 6 | 2 | New target, same pinned source and controls; repeat C/W/R. |
| 7, 8, 9 | 3 | New target, same pinned source and controls; repeat C/W/R. |

C and W each have a 75-minute ceiling; R has a 10-minute ceiling. The nine slot
ceilings sum to eight hours, leaving at most two hours of the 10-hour window for
preparation, input observations, locks and cleanup. This is containment arithmetic,
not a runtime prediction. No extra warmup, sample substitution or retry-to-green.
Unknown/unsafe resources, changed inputs, detector failure or unsettled custody
stop admission with the failed/pending slot intact. Every duration >=900 seconds
is a production-target breach even when accepted as a measurement observation.

Keep serial legs and one-worker controls. Retain natural external workload and
raw exposure; coordinate our own broad jobs. Keep at most two fresh campaign
targets. Disposal requires exact identity, owner settlement and ProductLease;
never sweep an existing/shared cache. Admission requires 130 GiB initially free,
at most 40 GiB per target, 10 GiB evidence and 40 GiB system headroom, including
remaining permitted growth. Space is observed, not exclusively reserved.

## Decision and candidate preparation inside the same campaign clock

After all nine baseline slots succeed and settle, summarize each cache class
separately. Rank measured queue/discovery/compile/link/execution/cleanup costs
and resource peaks. Choose at most one concrete optimization supported by those
observations; do not assume the Git-shim result identifies a full-gate bottleneck.
Implement it in an isolated author checkout and run only its new/changed focused
regressions plus a meaningful negative/mutation control for retained detection.
Record the candidate source delta and alternative considered.

The 20-hour clock continues during analysis, edits and tests. No fresh clock is
created for candidate preparation. If no justified candidate or insufficient time
remains, stop with baseline-only/inconclusive evidence. Do not use unchanged source
as a fake optimization or extend/restart the reservation to complete the plan.

## Candidate window: slots 10–18

Use the internal revision name `optimization`, with a separately coordinated
start naming the same campaign root. Its window ends at the earlier of candidate
start + 10 hours and the original campaign end. Repeat exactly three C/W/R blocks
on fresh targets. Hold every comparison control fixed except source commit/tree
and fresh target identities. A changed toolchain, dependency, configuration,
worker policy or environment invalidates the match; no silent relaxation.

Pair baseline block 1 with candidate block 1 within each cache class, likewise
blocks 2 and 3. Preserve all raw times and compare paired differences/ratios,
range and exposure. Sequential windows can be confounded by changing workload;
three pairs per cache class are descriptive evidence, not automatic causal proof.
An inconsistent effect or unmatched exposure remains inconclusive. Report every
900-second breach and any residual capacity gap, never a median-only success.
Do not narrow detector coverage or activate host/batching/automation policies.

SH-801's independent scheduling study still requires ten alternating pairs on
one tree/day, warm artifacts without verdict reuse, actual scheduling classes and
overlapping list/hook probes. It gets its own coordinated reservation after this
stage; do not interleave it into the 18 SH-872 slots. SH-873 and SH-874 remain
conditional on measurements and their separately reviewed policy/rollout decisions.
