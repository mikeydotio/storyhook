# One slow python3 start in the browser cleanup became three reds

- **Date**: 2026-09-24 UTC (sighting); fixed 2026-09-26
- **Severity/Impact**: `bash scripts/run-e2e.sh --project=fractional-firefox`, at load 727-900 on 10 cores. One cleanup timeout failed its own test and two later tests in unrelated code ("2 Drafts", expected "1 Drafts"). Any project could show the same thing. Nothing false was stored.
- **Status**: fixed on story SH-765. SH-804 (other fixed per-call bounds) and SH-805 (the subprocess audit's blind spots) are follow-ups.

## Summary

Every `cleanUpCreatedStories` afterEach waits on the block-delivery barrier before it deletes a stray. The barrier reads the isolated store with one python3 subprocess (`e2e/block-delivery-barrier.cjs`). That read had a bare 5 s bound. Under load python3 took longer, the synchronous call threw `spawnSync python3 ETIMEDOUT` out of `expect.poll`, and the afterEach failed with the story still present. Playwright then started a new worker, and the new worker captured its cleanup baseline with the stray in it. The stray was "fixture" for the rest of the run.

## Root cause & trigger

1. **A fixed bound inside a graced wait.** The poll's patience was the config's load-graced expect timeout (SH-347). The read inside it was not graced. Playwright 1.63's `pollMatcher` awaits the generator outside the `try` that catches matcher failures, so one timed-out read ended the whole wait. The call was also synchronous, so the SH-347 watchdog could not run while python3 started. ODC: **Timing / Missing / bound**.
2. **A per-worker baseline.** The baseline was module state in `support.ts`, captured the first time a spec asked. A failed test always stops its worker (`workerProcessEntry.js`: `if (testInfo._isFailure()) this._isStopped = true`). So a stray from a failed cleanup always reached a fresh worker, and that worker's first capture took it in. The comment on the Map said it relied on `workers: 1`. It did not allow for worker restarts. ODC: **Assignment / Incorrect / state lifetime**.

The trigger is ordinary: iOS simulators and other gates on the same machine.

## What now guards the class

- The barrier reads asynchronously (`execFile`). Each read's bound is what remains of the wait's patience, which is sampled when the wait begins (`gracedPatience()`). The wait owns its deadline (`expect.poll` `timeout: 0`), so a spent patience reports the barrier's own message. Regressions in `cleanup-delivery-barrier.spec.ts`, each red before the fix: a read 1 s past the old bound completes the cleanup; a read that outlasts its patience fails naming the barrier and deletes nothing; a bound that Node would read as "none" is refused before python3 starts.
- The audit (`tests/e2e_browser_coverage.rs`) pins the reader's derived bound and its guard. `no_audited_command_carries_a_bare_numeric_bound` refuses a literal in any audited command.
- `e2e/fixture-baseline.ts` captures the baseline once per run, in global setup, before any worker (`the_fixture_baseline_is_captured_once_per_run_before_any_worker`).
- The `fixtureHeal` auto fixture removes earlier strays before the first hook of each new worker. It covers only the projects that specs clean. A heal that fails costs one red and leaves a run-scoped marker, so it cannot turn the rest of the run red.

## Lessons

- A subprocess bound inside a wait is part of the wait's budget, not a second budget.
- State kept "once per run" in a worker process lasts only as long as the worker. Playwright replaces the worker after every failure, which is exactly when that state matters.
- A recovery step that can fail must not be retried by every later test.
