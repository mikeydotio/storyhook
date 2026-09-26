# Test audit: where the gate's time goes, and what was cut

Design of record for **SH-783**, audited 2026-09-25.

## The ask, and what the measurements said instead

SH-783 asked for an audit of every storyhook test, deleting the tests that do
not earn their keep, with a goal of a full release in 15 minutes or less. The
suspicion was bloat. The measurements found some, but they found that the
wall clock was going somewhere else first.

| Leg | 2026-09-08 | 2026-09-25 (pr-860) |
|---|---|---|
| fmt + clippy + build | 20 s | 78 s |
| rust-suite (core, 201 binaries) | 423 s | 815 s |
| rust-contracts (131 binaries) | 330 s | 1,259 s |
| plugin (74 → 107 scripts) | 228 s | 1,014 s |
| **`make test`** | **~17 min** | **~53 min** |
| e2e (release tier only) | — | ~30-40 min |

Sources: the per-binary `finished in` lines of the central verifier's gate logs
(`.git/storyhook/verification-logs/`), 40+ gates from 2026-09-04 to 2026-09-25,
and the plugin scripts' PASS-line timestamps in the daemon's activity journal
(15 runs, 2026-09-19 to -21).

Three findings:

1. **Everything ran one at a time.** Cargo runs the binaries of one
   `cargo test` invocation one after another, so each Rust battery took the
   SUM of its binaries' run times (1,734 s). The plugin runner ran 107 scripts
   one after another. The e2e runner runs its six Playwright projects one after
   another, each with `workers: 1`.
2. **About ten hot spots held most of the Rust time.** Five binaries were 46%
   of it: `github_shell` 270 s, `verifier_lifecycle` 190 s, `merge_gate` 134 s,
   `plugin_install` 112 s, `orphan_check` 99 s. Most of the 3.7× growth since
   2026-09-08 is 92 new binaries (747 s) and a few fixtures that became several
   times slower per test.
3. **The long tail is cheap.** About 6,000 of the Rust tests run in
   milliseconds. Deleting them would save seconds and lose detection. There is
   no `#[ignore]` bloat (2 hits, both legitimate).

So deletion alone could not reach the goal without deleting tests that prove
something, which the story forbids. SH-783's scope became the goal (decision
D1 on the story): delete what is provably redundant, remove waste in the hot
spots, and stop running independent work one item at a time.

## The deletion rule

A test or case was deleted only when (a) it repeated an execution provably
identical to another, or (b) another named test asserts the same observable
(decision D3). Every regression test without a covering test stays.

## Verdicts

### Rust hot spots

| Binary | Before | After | Verdict |
|---|---|---|---|
| `github_shell` | 270 s | 40 s | **7 of 8 runs deleted.** `sanitized_submission_receipts_…` ran one plugin fixture once per subset of three parent selectors, through a wrapper that unconditionally strips all three: eight identical child environments. One run with every selector set keeps the claim; `test_children_cannot_inherit_orchestration_selectors` proves the stripping. |
| `dead_public_surface` | 53 s | 1.4 s | **Rewritten, same rules.** It rescanned 355,000 lines once per `pub` definition. A one-pass identifier index gives the same answer. |
| `daemon_concurrency` | 31 s | 2.5 s | **Wait removed.** After its assertions it waited out the rest of a 30 s hook; the guards now kill it. |
| `orphan_check` | 99 s | 97 s | **Defect fixed, cost kept.** `collecting_an_abandoned_daemon_never_fails_the_phase` was vacuous for `postlude` and `check` (the preflight collected the only daemon). Each phase now collects its own. Its 11 s age waits are real `ps etime` and stay. |
| `verifier_lifecycle` | 190 s | — | **Kept; follow-up.** 55 Python cases run one at a time. SH-767 is reworking this suite, so its parallelization waits for it. `test_verifier_lifecycle.py:986` duplicates `merge_gate.rs:480` and is the one to drop then. |
| `merge_gate` | 134 s | — | **Kept; follow-up.** 66 real-git gate runs with no single waste; needs its own audit (busy-waits at `:248`, `:336`, `:862`). |
| `plugin_install` | 112 s | — | **Kept.** Each packaged harness copies the 80 MB binary and starts a daemon; the copy-vs-daemon split was not measured. Overlaps under the pool. |
| `attachment_upload`, `daemon_timeouts` | 31 s, 30 s | — | **Kept** (decision D4). Each waits out a 30 s production deadline that a unit test proves at 100-250 ms; shortening it would need a production override used only by tests. Overlaps under the pool. |
| `release_gate_order` | 23 s | — | **Kept.** Seven fixtures each hit `release.sh`'s `sleep 1`; about 7 s to gain, overlapped under the pool. |

### Plugin scripts

No script was deleted. The candidates the audit examined:

| Script | Verdict |
|---|---|
| `test-resource-discovery.sh` (3 locations × 5 callers) | **Kept.** SH-709's claim is that discovery does not depend on the caller; the full cross product is what proves independence, and the callers differ in `STORY_AGENT`, so the runs are not identical. |
| `test-dispatch-launch-template.sh` (15 real dispatches) | **Kept.** Dry-run output carries the launch command, but these cases prove what a real dispatch types; no named test covers that. |
| `test-submit-head-reporting.sh` | Runs in the plugin leg, in a lib test, and (now once) in `github_shell`. Each run proves a different parent environment. |

Two scripts race a wall-clock budget that concurrent siblings could stretch
(`test-dispatch-sentinel-readiness.sh`'s late-sentinel negative,
`test-fake-tmux-state.sh`'s pane lifetime). They run in the serial lane. Every
other small poll budget in the suite is event-driven.

## The runners

### Plugin leg: a bounded pool

`plugins/story/tests/run-tests.sh` runs up to `STORYHOOK_PLUGIN_JOBS` scripts
at once (default 4). Every script already had its own root, store, daemon,
port and fake tmux, so they share nothing but the machine. A
`# plugin-runner: serial` line puts a script in a lane that runs alone after
the pool. The report keeps its exact line format in one fixed order, only
the runner writes the gate journal, and a signal takes every running script's
process tree with it. `tests/plugin_runner.rs` is the contract.

Two isolation defects surfaced on the way and were fixed in their own commits:
scripts inherited the caller's `$TMUX` (so a `tmux new-session` without `-S`
reached the real server), and `test-binary-lease.sh` counted every lease in a
root that concurrent runs share.

| Jobs | Result | Wall clock |
|---|---|---|
| 1 (recent gates) | green | 870-1,870 s |
| 4 | 107/107 | 401 s |
| 6 | 107/107 | 372 s |

Measured at load average 18-25 with the verifier's own gate running. Six
bought 7% for half again the process churn, so the default is four.

### Rust batteries: a thread-budgeted pool

`scripts/test-pool.py` runs one `cargo test` per binary and admits binaries
while their test threads fit `STORYHOOK_TEST_THREAD_BUDGET`. A binary gets
one thread per test up to the battery's own `--test-threads`, so a one-test
binary takes one slot, not four. Every selected target is built once first,
through `cargo_diagnostics.py`, so compile errors keep their classification.
Each binary's capture is written out whole and in the battery's order, so the
ledger, the verifier's failure summary and the daemon's bundled parser see
the same log shape as before. Binaries start longest first from run times the
pool records under the git common dir.

Not cargo-nextest (decision D2): its process-per-test model would turn about
330 shared per-binary daemons into about 6,000 daemon starts and change the
log format that three parsers and a daemon-bundled copy read.

Measured 2026-09-26 under one hold of the machine `gate` lock (per-binary cap
`--test-threads=4`):

| Battery | Budget | Leg wall | Pool | Load avg | Result |
|---|---|---|---|---|---|
| contracts | serial (2026-09-25 gate) | 1,259 s | — | — | green |
| contracts | 4 | 1,015 s | 936 s | 8-9 | green¹ |
| contracts | 8 | 667 s | 581 s | 4-8 | green¹ ² |
| contracts | 16 | 349 s | 345 s | 24-33 | green¹ |
| core | serial (2026-09-25 gate) | 815 s | — | — | green |
| core | 8 | 694 s | 443 s | 13-14 | green |
| core | 16 | 351 s | 333 s | 17-33 | **7 failures** |

¹ Except `selective_gate`'s impact-manifest check, which caught the new
`tests/plugin_runner.rs` missing its declaration (fixed).
² 17 binaries were failed by the pool's own guard for compiling after the
pre-build: Python imports were writing `__pycache__/` into the checkout, which
reruns `build.rs` on the next cargo invocation. Fixed at the origin
(`PYTHONDONTWRITEBYTECODE` in both runners, `__pycache__/` ignored, and the
`sys.dont_write_bytecode` guard added to `plugins/story/lib/continuation_runtime.py`,
the one plugin entry point story.sh starts outside the runners' environment
that lacked it; `tests/plugin_contract.rs` now requires the guard in all of them).

**The default is 8** (decision D7 on the story). At 16 the core battery failed
on production timing bounds under load: `lane_budget`'s 3 s tmux census (six
tests) and `corruption_recovery`'s 5 s spawn deadline. That is the overshoot
symptom the Makefile's note on `--test-threads` warns about, and the same
class as SH-766. Raising the budget is reasonable once those seconds-scale
bounds are load-graced.

Two things the measurements showed beyond the budget:

- Concurrency inflates each binary. The core battery's binaries summed 523 s
  serially and 903 s in the pool at budget 8: core is CPU-bound, where
  contracts mostly waits on subprocesses. The budget is a bound on threads
  that are often idle, so slot time (duration × threads) runs about 3.3× the
  sum of durations.
- About 250 s of the core leg is outside the pool: run-tests.sh's discovery
  lists every binary twice, serially, and the pool lists them again. Folding
  discovery into the pool's parallel listing is recorded on SH-795.

Projected `make test` at budget 8: fmt and clippy about 45 s, core about 694 s,
contracts about 550 s (warm schedule), build about 33 s, plugin about 400 s:
**about 29 minutes, from about 53**.

### The first pooled gate

The central verifier's first run with both pools (PR 865, 2026-09-26):
rust-suite 727 s, rust-contracts 615 s, plugin 412 s. About 31 minutes of
`make test`, against about 53 minutes before.

It also found the class the pool was always going to expose: a test that
asserts on machine-wide process state. `orphan_check`'s postlude collects every
storeless daemon the user owns (SH-493, on purpose), and one of its tests
required the postlude to be completely silent. A daemon from a binary running
beside it made the postlude speak. The assertion now requires silence only
about the fixture's own processes. The reverse direction is accepted rather
than fenced: a sibling binary's daemon that outlives its store for 10 s can be
collected by `orphan_check`, which is what every gate's postlude already does
to other worktrees' leftovers. If that ever shows up as a failure in the
sibling, run `orphan_check` alone after the pool, the Rust counterpart of the
plugin runner's serial lane.

## Roadmap to 15 minutes

What this story leaves, in order of leverage. Each is a story related to
SH-783.

1. **SH-792 — e2e: shard the browser projects and decide cross-engine
   coverage.** 1,212 of about 1,400 test runs are the 111 desktop specs run
   twice (chromium and webkit), one project at a time, one worker each.
   Per-shard daemons and seeds (`run_one_project` already isolates per project)
   can run concurrently; which specs need both engines is a scope decision for
   a council. This is the release gate's largest leg by far.
2. **SH-795 — the Rust legs' non-test time.** About 250 s of serial listing
   before the core pool, plus the link time of 332 integration binaries of
   about 24 MB each.
3. **Raise the thread budget** once the seconds-scale production bounds that
   failed at 16 (SH-766's class) are load-graced: 16 measured about 5.5 minutes
   faster per gate.
4. **SH-793 — `verifier_lifecycle`: run its Python cases concurrently**, after
   SH-767. At budget 16 it is the contracts battery's long pole (283 s).
5. **SH-794 — audit `merge_gate.rs`.**
6. **SH-796 — run independent legs concurrently** (the plugin leg beside the
   Rust batteries), revisiting SH-701 once both pools have a clean history.
7. **SH-797 — dispatch-path latency.** Dispatch-heavy plugin scripts became
   1.6-1.9× slower from 2026-09-13 to -21; whether that is production latency
   is unconfirmed.
