# SH-589 — Verifier handoff

## Diagnosis

- PR #686 completed `rust-suite` in 11m40s, then waited 976s for another
  process's machine-wide gate before the 1746s verifier timeout halted it.
- The dashboard joined the current `rust-contracts` label to aggregate
  3706/3706 counts from the completed suite. Lock acquisition, discovery, and
  result-ledger work were invisible.
- Per-battery lock acquisition also allowed another run to interleave and
  create a second wait inside a timeout designed for one wait plus one run.

## Implementation

- Journal `activity` records expose lock acquisition, Rust test discovery, and
  result-ledger recording without adding fake checklist units.
- Current-step selection uses the newest live item or activity. `/data.tests`
  contains only that test item's exact counts and is absent for activities.
- The dashboard and its accessible card name render the corrected current step
  and omit unrelated percentages.
- Central verification holds `gate` around the complete speculative command.
  Inner Rust battery locks use the existing reentrant path, so a second suite
  cannot interleave between them.
- `machine-lock.sh` owns optional running/passed/failed acquisition telemetry;
  both centralized verification and direct Rust batteries supply a parent path.

## Focused evidence

- Red: the gate-progress unit target lacked current-step counts; the new
  centralized-gate regression returned `tests-failed` because its command did
  not inherit `gate`.
- Green: gate-progress unit tests 18/18.
- Green: `gate_lock` 13/13, `machine_lock` 26/26, `merge_gate` 25/25, and
  `verification_queue` 46/46.
- Green: focused verification-status E2E, Chromium 2/2 and WebKit 2/2.
- Green: targeted Clippy with warnings denied, Rust formatting, changed-shell
  syntax, and diff whitespace checks.

The centralized verifier owns the full suite, merge, completion, and lane
cleanup. Decision history and the approved verbatim plan are durable comments
on SH-589.
