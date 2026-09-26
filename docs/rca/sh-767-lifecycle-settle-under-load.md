# A slow census made the verifier refuse a kill that worked, and fixed budgets turned gate load into false reds

- **Date**: 2026-09-24 to 2026-09-26 UTC
- **Severity/Impact**: `tests/verifier_lifecycle.rs` failed on trees that did not touch the verifier. Each false red returned unrelated work to its author and cost about one hour of central verification. Three sightings, each in a different case. The production half could stop the verifier queue until an operator repaired the owner record.
- **Status**: fixed on story SH-767 (`scripts/verifier-owner.py`, `scripts/tests/load_grace.py`, the lifecycle harness).

## Summary

Two mechanisms, one production and one in the harness, both exposed by census latency under gate load (`ps -axo pid=,stat=` takes 20 ms at idle and more than 5 s at load averages of 300 or more on 10 cores, SH-766).

1. **Reap window judged from stale evidence (production).** `execute()` sent SIGKILL and then, in the same loop pass, refused "could not reap execution session ... after SIGKILL". It measured the reaping eighth from the scheduled deadline, not the kill, and named members from the census taken before the kill. `signal_session()` runs its own census, so one slow census used up the whole eighth before the kill could be observed. For the gate supervisor with the leader exit not yet recorded, the next admission refused "interrupted arbitrary gate has ambiguous ownership", and merge-watch refused to restore the checkout.
2. **Fixed budgets and fixed patience (harness).** Each case handed production a fixed 8, 16 or 30 s budget, which sets every grace in the SH-686 ladder, and waited a fixed `budget + 5 s` for work that production does not budget (restoration, verdict emission, outer census). Under contention the work stretches and the numbers do not, so production correctly escalates and ends cleanup that the case asserts is complete.

## Timeline

- **2026-09-24** — PR 857 (SH-758) gate: `test_outer_lock_allows_the_gate_its_nested_cleanup_budget` finds `gate_started` true, `gate_leader_exit` null and the outer session never completed. The failure prints only the owner record.
- **2026-09-24** — SH-758 worktree, 364 s run: `test_exited_leader_stays_pinned_until_its_session_is_quiet` exceeds `budget + 5 s`. The reported 0.03 s "overshoot" is the poll interval, not the size of the miss.
- **2026-09-24** — SH-760 verification at load about 250: `test_cancellation_settles_resistant_subgroup_after_gate_leader_exits` finds the checkout not restored (recorded on SH-766).
- **2026-09-26** — SH-767 reproduces the PR 857 record exactly by delaying only the census (1.5 s at a 16 s budget): the gate supervisor refused with `live writers=[]`. After the reap fix the same case completes; a 3.5 s census still ends restoration, which isolates the harness mechanism.

## Root cause & trigger

The reaping eighth assumed a census takes no time. Every deadline in `execute()` is checked only between censuses, and the refusal re-used the census from before the kill, so latency did not make supervision slow — it made it wrong. ODC: **Timing/Serialization / Incorrect / teardown**.

The harness applied SH-698 Decision 1(a), "the production ladder finishes inside the budget by contract". That premise fails twice: the ladder's checks are late by one census each, and the observed interval includes work that no budget covers.

The trigger is ordinary: several worktree suites and the central verifier on one machine.

## Contributing factors

- The PR 857 failure message had no wrapper log and no attempt log, so which layer ended the cancellation could not be read from it.
- `cancel_gate` read the owner record as soon as the wrapper returned, even when orphaned sessions below it were still writing the record.
- The cancellation descendant check used `os.kill(pid, 0)`, which succeeds on a killed process that is still a zombie.

## What now guards the class

- `verifier-owner.py`: `killed_at` is the delivered kill; only members found by a census begun after `killed_at + budget/8` cause a refusal. A member the census always reports is still refused one pass later (`test_verifier_verdict.py::test_member_outliving_the_reaping_eighth_is_still_refused`).
- Slow-census regressions (a PATH `ps` wrapper that delays only the census shape): gate and lifecycle supervisors each reap a TERM-resistant orphan with no refusal. Mutation-checked: the old predicate turns both red.
- `scripts/tests/load_grace.py` ports SH-347: the budget each case hands production and every harness allowance scale by one-minute load per core; waits resample at expiry and extend, never shrink, never silently; the cap keeps any single wait under 15 minutes. `ContentionGrace` pins it; a normal gate proves a graced, zero-led spelling reaches supervision in decimal.
- Settlement failures carry the wrapper log, every attempt log, timing and load; `cancel_gate` waits for recorded sessions to be quiet before it reads the record.
- `tests/timing_assertions.rs` still refuses a bare Python ceiling in `scripts/tests/`.

## Lessons

- A deadline checked between slow observations is late by one observation. Judge survival only by evidence gathered after the window closes.
- A budget handed to the code under test is a claim about the machine; under shared load, grace it, or the test measures the machine instead of the code.
- A "small overshoot" from a polling assertion measures the poll, not the miss.
