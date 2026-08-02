# Handoff — the hardening run, next up: SH-115

*(Supersedes the SH-131 handoff. SH-131 is closed: the three store-isolation
invariants each have one home, two of them a test, and a real harness gap was
found and fixed on the way.)*

The run itself is described by
[`HARDENING_PROGRESS.md`](HARDENING_PROGRESS.md) — read its **START HERE**
section first. That is the process; this file is only what the next story needs
on top of it.

## What SH-131 left behind

Two new tests in `tests/store_isolation.rs`, one one-line script fix, and a
`CLAUDE.md` that no longer states the invariants itself.

1. **`a_child_process_of_a_store_path_run_lands_in_the_same_store`** pins that a
   `--store-path` run's children reach the same store, by running the binary
   again from an event hook. **It pins the promise, not the mechanism** — this
   matters directly to you if you take SH-114 or SH-116, which rewrite flag
   resolution in `main`. Publish the flag differently and the test stays green;
   leave children resolving somebody else's store and it goes red.
2. **`every_harness_that_isolates_the_data_dir_neutralizes_the_store_path`** is
   derived: it takes every tracked `*.sh` that exports `STORYHOOK_DATA_DIR` and
   requires `unset STORYHOOK_STORE_PATH` beside it. **If you add a shell harness,
   it must neutralize the store path or the suite fails.** That is deliberate —
   it caught `scripts/capture-baseline.sh`, which had been missing the line since
   store isolation landed.
3. **`CLAUDE.md` now points at the spec** rather than restating three invariants.
   `docs/spec/store-isolation.md`'s "As built" items 1, 5 and 6 each name the test
   that guards them. Put new invariants there, not in `CLAUDE.md`.

## One operational warning, and it cost an hour

**The council could not be convened in the SH-131 session.** Two seatings, six
subagents, four rounds of pings, zero responses; both were stopped with
`TaskStop`. If you invoke `council:council-vote` and the seats are still silent
after ~10 minutes with no answer to a direct pulse request, **do not spend
another 30 minutes on a second seating** — that was tried and failed identically.
Stop them, write the abort, and decide as chair with your reasoning and blind
spots recorded, as `.council/sh131-invariant-homes/{ABORT,DECISION}.md` does.
Measure first either way: the measurements are what made a chairless decision
defensible.

**And verify your working directory before any command that writes.** An
exploratory `story project init` whose `cd` had landed in `/private/tmp`
initialized `/private/tmp` itself as a project, and since every fixture in the
suite is built under `/private/tmp`, the resolution walk found that pointer from
inside fixtures that should have had none. One test went red for a reason that
had nothing to do with the change under test.

## The next story: SH-115 — C3 Identity

`story show SH-115` is the brief. Critical, ready, and the first of the
server-owned epic's remaining children. It adds the remotes schema and **one**
URL normalizer; it does not touch flag resolution, so the invariants above are
not in its way.

Read its comments as well as its description — several stories in this backlog
carry re-spec notes that contradict their own titles.

## After SH-115

The queue in `HARDENING_PROGRESS.md` is the forecast: SH-94 and SH-110 (both
gating SH-114), then SH-114 and the epic proper. Four stories filed during this
run are not in the queue yet — **SH-133** (rollback drops project settings),
**SH-134** (`add_type` accepts an unaddressable slug), **SH-135** (a hand-taken
backup inherits the 7-deep retention) and **SH-136** (the daemon-address harness
list is hand-maintained prose and was already stale). SH-136 is the direct
sibling of what SH-131 fixed, and is small.
