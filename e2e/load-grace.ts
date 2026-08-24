import os from "node:os";

/**
 * Load-grace policy for the browser suite (SH-347).
 *
 * A binding user determination on SH-347 (2026-08-17): *"relax the timeouts
 * when machine is under load. If timeout fires, examine load, and if high
 * contention, reset timeout timer (up to a maximum of 15 minutes) rather
 * than ending the test."*
 *
 * Playwright cannot resume a test timeout it has already fired -- there is
 * no hook that runs after the runner has torn a test down. The only lever is
 * `testInfo.setTimeout()`, which must be called *before* the deadline. So
 * "reset the timer" is implemented as: sample contention continuously and
 * extend the deadline before it can fire, never after. Two layers apply this
 * (`e2e/playwright.config.ts` for the config-evaluation layer, the
 * `loadGrace` fixture in `e2e/specs/support.ts` for the per-test watchdog) --
 * this module is only the pure policy both of them call into. This is the
 * ONLY file under `e2e/` that may call `os.loadavg()`
 * (`tests/e2e_load_grace.rs` fences that).
 *
 * At idle this machine's own load average is ~3 on 10 cores (ratio ~0.3), so
 * `graceMultiplier` returns exactly 1 and every budget below is bit-identical
 * to SH-222's own measured numbers. A real defect still surfaces exactly as
 * fast as it does today when nothing is contending for the machine.
 */

/** SH-222's own measured budget for a whole test (`e2e/playwright.config.ts`'s
 * own comment carries the three-point measurement this derives from: idle
 * 2.9m/7.7s slowest, load~44 4.4m/8.6s, load~100 6.4m/10.0s -- 111/111 green
 * in all three). Not to be raised on its own: raising it would hide exactly
 * the two real defects that comment records finding. */
export const BASE_TEST_TIMEOUT_MS = 15_000;

/** SH-222's own measured budget for a single `expect(...)` call. Unchanged
 * for the same reason as {@link BASE_TEST_TIMEOUT_MS} -- an assertion that
 * exists to *prove* a bound states its own `{ timeout: N }`
 * (`DATA_DELAY_MS * 3`, `GONE_TIMEOUT`, `DISPATCH_COMPLETION_TIMEOUT`, …);
 * this default only ever governs how long the harness is willing to *wait*
 * for something it expects to become true, which is patience, not proof. */
export const BASE_EXPECT_TIMEOUT_MS = 5_000;

/** The ceiling. Provenance is a recorded human tolerance, not a
 * measurement, and this comment is where that is admitted: the user
 * determination on SH-347 named above, verbatim, 2026-08-17. Everything
 * else in this module derives from it rather than carrying its own bare
 * literal. */
export const MAX_TEST_TIMEOUT_MS = 15 * 60_000;

/** Derived from the ceiling divided by the base budget, so one number
 * (`MAX_TEST_TIMEOUT_MS`) governs both the test-timeout ceiling and the
 * expect-timeout ceiling in the same proportion, rather than two ceilings
 * that could independently drift. */
export const MAX_GRACE_MULTIPLIER = MAX_TEST_TIMEOUT_MS / BASE_TEST_TIMEOUT_MS;

/** The number of logical cores this machine reports, for expressing
 * contention as "runnable threads per core" rather than a raw load average
 * whose meaning depends on the machine it was measured on.
 * `os.availableParallelism()` (Node 19+) is preferred over `os.cpus().length`
 * where available -- the two agree on an unconstrained machine, and only
 * `availableParallelism()` reflects a cgroup/container CPU limit, which this
 * repo's own CI and contributor machines do not use today but a future one
 * might. */
export function cores(): number {
  return os.availableParallelism?.() ?? os.cpus().length;
}

/**
 * Contention as processor sharing: the one-minute load average divided by
 * the core count. A value of 1.0 means exactly as many runnable threads as
 * cores -- the point past which a thread that owned a whole core at idle now
 * shares it, and wall clock stretches in proportion. Below 1.0, nothing is
 * contending and no grace applies.
 *
 * `os.loadavg()` is valid on every platform this suite runs on (Darwin,
 * Linux); it returns zeros on Windows, where this suite does not run
 * (`e2e/playwright.config.ts` targets only Desktop Chrome/Safari and their
 * mobile-emulation counterparts, none of them Windows-only).
 *
 * Known imprecision, stated rather than hidden: a one-minute average is an
 * exponentially-weighted moving average, so it under-reacts at the start of
 * a load burst and over-reacts for a while after one ends. Both directions
 * are the safe direction here -- under-reacting costs a test its grace a few
 * seconds late, over-reacting only ever grants unneeded patience -- so this
 * is accepted rather than replaced with a heavier, self-measuring probe.
 */
export function contention(): number {
  return os.loadavg()[0] / cores();
}

/**
 * The grace multiplier for a given contention ratio: 1 below the point of
 * contention (`ratio <= 1`), the ratio itself above it (this machine has
 * `ratio` times as many runnable threads as cores, so a thread's own wall
 * clock stretches by roughly that factor), clamped at
 * {@link MAX_GRACE_MULTIPLIER} so a single freak reading can never grant
 * more than the user's own named ceiling.
 */
export function graceMultiplier(ratio: number = contention()): number {
  return Math.min(MAX_GRACE_MULTIPLIER, Math.max(1, ratio));
}

/** `baseMs`, graced by the current (or supplied) contention ratio. Only
 * correct for a `baseMs` at or near {@link BASE_TEST_TIMEOUT_MS} /
 * {@link BASE_EXPECT_TIMEOUT_MS} -- the two config-evaluation-time budgets
 * this is written for -- because {@link graceMultiplier} clamps the
 * *multiplier* itself, which only coincides with an absolute ceiling on
 * `MAX_TEST_TIMEOUT_MS` when `baseMs === BASE_TEST_TIMEOUT_MS`. A per-test
 * budget can start from a spec's OWN `test.setTimeout()` (e.g.
 * `dispatch.spec.ts`'s multiple of `DISPATCH_COMPLETION_TIMEOUT`), for which
 * this function would happily multiply straight past the user's literal
 * 15-minute ceiling -- {@link gracedTestBudget} is what the per-test watchdog
 * actually calls, for exactly that reason. */
export function gracedBudget(baseMs: number, ratio: number = contention()): number {
  return Math.round(baseMs * graceMultiplier(ratio));
}

/**
 * `baseMs`, graced by the current (or supplied) contention ratio, clamped to
 * the user's own ceiling ({@link MAX_TEST_TIMEOUT_MS}) as an ABSOLUTE bound
 * on the result -- unlike {@link gracedBudget}, which clamps the multiplier
 * and so only bounds correctly for the two known config-time bases. This is
 * what the per-test watchdog calls, because a running test's own base
 * timeout is whatever `testInfo.timeout` already is when the watchdog first
 * samples it -- the suite default, or a spec's own `test.setTimeout()` -- and
 * "up to a maximum of 15 minutes" (the user's own words) reads as one ceiling
 * on any single test's total run time, not a further multiple stacked on top
 * of an already-large custom budget.
 */
export function gracedTestBudget(baseMs: number, ratio: number = contention()): number {
  return Math.min(MAX_TEST_TIMEOUT_MS, Math.round(baseMs * Math.max(1, ratio)));
}

/** The timeout window to grant from *now*, shortened as necessary to preserve
 * the user's absolute 15-minute wall-clock ceiling.
 *
 * The watchdog lives in an auto fixture. In that slot Playwright interprets
 * `testInfo.setTimeout()` as a fresh duration for the currently running
 * fixture, not as a deadline measured from the test's beginning. Passing an
 * elapsed-plus-window total therefore resets the fixture to an ever larger
 * duration and can run past the claimed ceiling. The reset duration is the
 * smaller of the contention-graced window and the wall time still remaining;
 * near the ceiling it deliberately shrinks so repeated resets all converge on
 * the same absolute deadline. One millisecond, never zero, at or past the
 * ceiling: Playwright uses zero to mean "disable timeouts". */
export function resetTestBudget(
  baseMs: number,
  elapsedMs: number,
  ratio: number = contention(),
): number {
  const elapsed = Math.max(0, Math.round(elapsedMs));
  const remaining = Math.max(1, MAX_TEST_TIMEOUT_MS - elapsed);
  return Math.min(gracedTestBudget(baseMs, ratio), remaining);
}

/**
 * A one-line, human-readable summary of the grace decision at a given
 * moment -- used both at config-evaluation time (`playwright.config.ts`) and
 * by the per-test watchdog (`support.ts`), so every reader of either sees
 * the same shape. A grace nobody can see is the SH-306 shape one layer up: a
 * gate whose verdict depends on state it never reported.
 */
export function describeGrace(ratio: number = contention()): string {
  const multiplier = graceMultiplier(ratio);
  const testBudget = gracedBudget(BASE_TEST_TIMEOUT_MS, ratio);
  const expectBudget = gracedBudget(BASE_EXPECT_TIMEOUT_MS, ratio);
  return (
    `load-grace: contention=${ratio.toFixed(2)} (loadavg=${os.loadavg()[0].toFixed(2)}, ` +
    `cores=${cores()}) multiplier=${multiplier.toFixed(2)} ` +
    `test=${testBudget}ms expect=${expectBudget}ms`
  );
}

/** `E2E_LOAD_GRACE=0` is the kill switch -- every function above still works
 * (a caller who ignores this and calls `gracedBudget` directly still gets a
 * graced number), but `playwright.config.ts` and the `loadGrace` fixture
 * both check this first and skip grace entirely when it's set, for a run
 * that specifically wants today's bare SH-222 numbers regardless of load. */
export function loadGraceEnabled(): boolean {
  return process.env.E2E_LOAD_GRACE !== "0";
}
