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

## Plugin and Rust legs stay serial (SH-796)

Decision of **2026-10-02**, v3.0.3, reviewed at `3b6cc7c6`: keep the plugin
leg after both Rust batteries and the production build. SH-796 asked for a
decision after the pools had a clean central-verifier history. The recorded
load failures do not support increasing concurrency yet. This resolves that
decision under current conditions; it is not a permanent prohibition.

### Evidence and limits

Dates below are UTC. Story descriptions and comments retain the evidence even
when a machine-local attempt log is no longer available; read them with
`story show <id> --json`.

| Evidence | Measurement and outcome | Source |
|---|---|---|
| SH-783 budget experiment, 2026-09-26 | Core: 694 s at eight threads, green; 351 s at sixteen, seven failures on production timing bounds. The contracts results have the manifest/rebuild qualifications in the table above. | SH-783 decision D7, 05:08:16Z; local experiment, not a central merge tree |
| First pooled central attempt, 2026-09-26 | Core 727 s, contracts 615 s, plugin 412 s. Plugin passed 110 scripts; contracts failed the machine-wide orphan-report assertion described above. | SH-783 repair comment, 07:46:15Z; PR 865, merge tree `47eac3b4225262984bf068d52a836a3107915f30` |
| Repaired pooled gate, 2026-09-26 | GREEN on a different tree; the preceding row's durations do not describe this attempt. | SH-783 GREEN comment, 09:11:23Z; merge tree `a01976529eb1b7344217e31e760576b278933219` |
| Central load failures, 2026-10-01 | Sampled contention median 6.28, maximum 8.47 on ten cores. Rust retry/early-result tests and plugin `test-unclaim.sh` failed; plugin leg 1,331 s. Targeted reproductions passed at lower load. | SH-863 description and SH-776 RED comment, 19:38:20Z; PR 895, merge tree `f521851a360d3019bc40d3fc1cf639ab46c647af` |
| Recent returned gate, 2026-10-02 | Plugin passed 121 scripts in 563 s; Rust failed `story_list_visibility::all_parses_identically_to_both_include_flags`. That summary alone does not establish the failure's cause. | SH-864 RED comment, 14:34:08Z; PR 904, merge tree `bc93e0bac0235977640ebb94cdca1f5142f258d8` |
| Subsequent central success, 2026-10-02 | GREEN on another merge tree; one success does not establish a stable load baseline. | SH-864 GREEN comment, 15:29:00Z; merge tree `6793ea1d69b7352816e928a47ac4959b63244289` |

Hiding all 412 s of the historical plugin leg would save at most 6 min 52 s
**if every leg kept its duration and build ordering added no cost**. No paired
serial-versus-overlap experiment was performed for SH-796. These different
trees and workloads establish neither causation nor a reliable speedup, and
their durations must not be averaged into one benchmark. A reused leg is not
a fresh execution. An ordinary code failure or an infrastructure halt is not
evidence of a load flake; classify it separately. In particular, SH-858's Python
runtime mismatch is distinct from SH-863's load evidence, whose originating
gate had no Xcode Python 3.9 trace.

### Capacity and orchestration constraints

SH-655's old lane-admission argument no longer bounds machine load.
[SH-672](full-auto-engine.md#sh-672--concurrency-belongs-to-each-run-or-to-the-operator)
removed those admission gates: each engine run has its own lane limit, manual
concurrency belongs to the operator, and the lane census is informational.
The machine-wide compiler bound limits compiler processes, not the Rust and
plugin pools' combined test processes. Eight Rust test threads and four plugin
scripts are different units; each can start descendants. Utility QoS controls
scheduling priority, not total admission.

The plugin needs a successful production build, so overlap would require
moving that dependency earlier. Any future design must also preserve
[SH-701's gate contract](test-tiers.md#as-built-independent-gate-legs-finish-after-red-sh-701):
independent legs finish after ordinary REDs; confirmed shared compilation
failures skip later dependent Rust/build work, and a failed build skips plugin
and browser execution; each leg retains its log, progress and reuse evidence;
cancellation or corrupt control evidence stops the gate; cleanup finishes
before the final receipt, which requires every required leg to pass.
The current helper shares mutable outcome/status state in one shell, so merely
backgrounding its calls would not preserve that contract.

GNU make's [parallel-execution controls](https://www.gnu.org/software/make/manual/html_node/Parallel.html)
limit recipe admission. They do not provide a shared test budget to these two
custom pools inside one recipe. Neither unconditional overlap nor an opt-in
switch is justified without measured benefit and the orchestration proofs.

### Reconsideration

SH-793, SH-794 and SH-795 own lifecycle, merge-gate and discovery optimizations;
SH-858 and SH-863 own runtime and load-failure repairs. At this review they
remain separately owned work; SH-863 records a dependency on SH-793. SH-796
does not take them over or create a blocker for its completed decision.

Reconsider after the relevant repairs land and central history supports a
stable baseline. Compare repeated runs of the same tree at comparable load,
record cache/reuse state and both pools' settings, and measure aggregate wall
time alongside per-leg time and failures. Prove dependency reporting,
cancellation, cleanup and progress integrity before enabling overlap. Until
then, retain serial legs and the existing budgets, with no new concurrency
switch or benchmark infrastructure.

## The browser leg as concurrent slices (SH-792)

Design of record for **SH-792**, measured 2026-09-26.

### What changed

`scripts/run-e2e.sh` ran its Playwright projects one after another, one worker
each. It now lists the whole selection once (the *plan listing*, before any
fixture exists, in `e2e/plan-listing.ts`'s placeholder mode), cuts it with
`scripts/e2e-pool.sh` into slices of whole spec files -- one project and a
Playwright `--test-list` each, packed longest first -- and runs up to
`STORYHOOK_E2E_JOBS` slices at once. Each slice is still one `run_one_project`
with its own seed, daemon and fake tmux, `workers: 1` (SH-627). The run is
refused if the slices ran a different number of tests than the plan listed.
A caller's own `--shard`/`--test-list` keeps its per-project meaning. The pool
replays each slice's log whole and in order, returns 0 or 1 (never a slice's
own 137, which `gate-legs.sh` would read as a cancelled gate), and on a signal
stops writers before the shells whose cleanup removes what they write.
`tests/e2e_pool.rs` drives it under `/bin/bash` 3.2 with stubs.

The cross-engine question went to a council (verdict on SH-792): every
browser-driving desktop spec keeps running on both engines; only the five specs
that never reach a browser run once, as `*.node.spec.ts`, in an engine-free
`node` project that refuses to launch any browser. `tests/e2e_browser_coverage.rs`
fails a desktop-pair spec that requests no browser fixture. No per-spec engine
tag: a guard can check a tag exists, never that it is true.

### Measurements

All on this machine (M1 Max, 10 cores) with other agent sessions running;
load is the 1-minute average, mean (max).

| Run | Slices | Wall | Load | Result |
|---|---|---|---|---|
| HEAD, sequential | 6 projects | 2,842 s | 33 (130) | 4 failed: settings-version (a real bug, fixed on SH-792), 3 load flakes |
| 8 jobs x 1 slice/job | 8 | 774 s | 32 (57) | green, 1,411 passed |
| 8 jobs x 2 | 16 | 625 s | 52 (66) | SH-811 on both engines + load flakes |
| 10 jobs, 1 file/slice (isolation proof) | 252 | 807 s | 122 (185) | SH-811 + 3 webkit load flakes (green alone) |
| 12 jobs x 2 | 24 | 599 s | 87 (132) | SH-811 + load flakes, two at the ungraced 5 s expect budget (SH-813) |

**The default is 8 jobs, 2 slices per job.** Twelve bought 4% at twice the
load flakes; eight keeps two cores free. One slice per job left the four small
projects each holding a whole slot. Per-test time rose about 1.3x at 8 jobs
and about 3x at 12 under ambient load. WindowServer's IOSurface count peaked at
566-1,365 in every run, far from the 65,535 crash threshold (every slice keeps
the display awake, SH-628), so no WebKit cap.

**Under 5 minutes was not reached**, and the council recorded why: the leg is
bound by machine contention, not by coverage -- dropping WebKit desktop
entirely would still land at about 5.3-6.4 minutes on this machine. The levers
left are SH-812 (duration-weighted packing; per-file times run 0.1-94 s while
slices are packed by count) and less fixed setup per slice (6-15 s each).

### Duration history and one discovery per planned slice (SH-812)

The planner accepts an optional fourth input column of seconds, keeping its
four-column output and test counts unchanged. Recorded time controls allocation
between projects, whole-file longest-first packing and slice admission. Ties
retain configuration/listing order. Missing history or a changed test count uses
the count as its weight; the default remains eight jobs and two slices per job.

Playwright's `--list`, `--test-list` and reporter receipts all use file names
relative to `config.rootDir` (currently `e2e/specs`). Canonicalizing the root and
file first handles macOS `/tmp`/`/private/tmp` aliases. The old dispatch post-check
expected a `specs/` prefix absent from real listings; exact root-relative matches
now select `dispatch.spec.ts` and `engine.spec.ts`, excluding stubbed namesakes.

Each slice writes a completed JSON receipt beside its artifacts. It contains
the discovered project/file counts, completed/pass/skip/retry counts and summed
Playwright test durations in seconds. The shell independently requires a
successful receipt matching its manifest: Playwright swallows exceptions thrown
by reporters, so reporter presence alone is insufficient evidence. This also
refuses a caller's reporter override if it suppresses the required receipt.
The receipt's `passed` count includes non-skipped outcomes that match
Playwright's declared `expectedStatus`, including intentional `test.fail()`
proofs. Unexpected passes and failures are not successful observations.

Planner-owned slices reuse the outer selection and omit the second Playwright
listing. Caller-supplied shards/test-lists retain their per-project listing.
Five-project seeding, the launch probe, per-run baseline and isolation are
unchanged. Timing artifacts separate preparation, seeding, daemon readiness,
listing, Playwright execution and cleanup using Bash's one-second clock;
individual test durations retain Playwright's millisecond precision.

`<git-common-dir>/storyhook/e2e-durations.tsv` is disposable scheduling history;
`STORYHOOK_E2E_DURATIONS` can select a separate file for experiments. Rows are
`version<TAB>project<TAB>file<TAB>count<TAB>seconds<TAB>observed-unix-seconds`,
currently version 1. Only complete successful files from a successful unfiltered,
planner-owned harness run train it. Failed, retried, interrupted and skipped files retain
previous history. A newer observation wins; an invalid row is diagnosed and
ignored. The parent reloads under a nonblocking advisory lock and atomically
replaces the file. A busy/unwritable cache is reported without changing the test
verdict. No shared store or seeded template is copied.

Measurement protocol: identical source/spec/dependency snapshots and the same
prebuilt binary; compilation excluded explicitly in both benchmark copies.
One optimized warm-up trains history, followed by baseline/optimized twice, at
eight jobs and sixteen slices. Sample machine load once per second; pairs whose
mean load differs by more than 20% are inconclusive. Retain outcomes and coverage
counts alongside wall time, setup time and longest-slice time. Compare the paired
result separately from SH-792's historical 625 seconds at mean load 52.

**2026-10-02 measurement: acceptance not established.** The first optimized
warm-up was stopped through normal pool cleanup after it exposed stale dispatch
assertions that active SH-804 owns. No baseline/optimized comparison pair ran,
and no timing history was promoted. The interrupted wall time below is a
diagnostic duration, not a completed-leg performance result.

| Measurement | Result |
|---|---|
| Selection | 1,477 tests, 129 files, 16 slices, 8 jobs |
| Interrupted warm-up | 1,575.7 s, including cleanup; exit by TERM |
| Sampled one-minute load | Mean 173.36; maximum 335.01 |
| Complete receipts | 8 slices; 933 results: 913 passed, 14 skipped, 6 failed |
| Longest completed slice | 1,537 s; incomplete slices excluded |
| Completed-slice seeding | Median 7 s; range 7–8 s |
| Completed-slice daemon readiness | Median 1 s; range 0–1 s |
| Completed-slice redundant listing | 0 s; planned manifests reused |

Two failures require `Dispatch` after a successful launch although the product
now shows `Resume` (`dispatch.spec.ts:247,323`). SH-804 owns that contract repair.
The other four failures were retained for diagnosis: an `entering` class disappeared
(`card-transient-classes.spec.ts:71`); Save Draft became enabled and restoring the
project left submit disabled (`create-story-project.spec.ts:343,370`); and a
frozen-clock footer did not change (`settings-version.spec.ts:58`). SH-813 owns
dynamic assertion patience, but this run does not prove the cause of those
four failures. Residual diagnosis stays in SH-812 after those independent fixes.

Raw local evidence is retained in
`.storyhook/logs/sh812-performance/`: `warmup.json` contains load samples;
`warmup.log`, `interrupted-pool/` and `warmup-artifacts/` contain logs, selection
manifests, receipts, phase timings and browser traces. The same initial evidence
is at `/tmp/sh812-bench/`. The benchmark driver is preserved beside the evidence;
refresh its optimized snapshot from the final implementation before reuse.
Its earlier snapshot predates the final whole-harness-success cache guard and
tiny-weight serialization correction; neither was exercised as a successful
history update in this interrupted run.

The shared machine's load is not comparable to SH-792's mean 52. Do not infer a
speedup or regression from these wall times. Resume with the independently owned
test repairs, account for remaining failures, then run a successful warm-up and
the two interleaved pairs before accepting this story's performance claim.

**Resumed repairs, 2026-10-03 UTC.** SH-804's completed changes are integrated.
SH-813's completed assertion-grace, exit-animation and route-lifetime changes
are reused with source attribution. Three separate SH-812 regression commits
repair the retained timing assumptions without changing dashboard behavior:

| Proof | Controlled precondition and retained assertion | RED → GREEN |
|---|---|---|
| Immediate create actions | Install the clock before navigation; hold the 150 ms vocabulary debounce across a native animation witness lasting twice that interval. Retain no-POST, disabled-control and restored-project checks. | Two WebKit failures → four Chromium/WebKit passes |
| Unrelated render preserves classes | Pause the target card's CSS animations and JavaScript cleanup timers. Drive the real modal's opening frame explicitly; retain every transient class after a wall-time animation witness and real create/render. | Two lifecycle-control failures → two desktop passes |
| Footer timer preserves version | Observe native DOM mutation records while advancing the clock. A one-second tick can correctly retain `Updated just now`; retain navigation, layout, version and three-second age-format checks. | Two repeated-label failures → six desktop passes |

The combined card/footer run passed eight cases in 26 seconds. Strict
TypeScript checking of the three changed specs and their imports passed.
The combined SH-804/SH-813 timeout audit exposed a separate integration gap.
Its repaired exception table classifies the assertion adapter's sampled and
delegated budgets and the explicit-deadline precedence proofs. The new
classification regression was RED before that repair; all 16 load-grace and
10 text-assertion source checks now pass. Classifications remain path-,
expression- and count-specific and reject unreviewed paths and changed values.
Raw regression evidence is in `.storyhook/logs/sh812-resumed/`.

The refreshed warm-up selected 1,507 tests in 16 slices, but was interrupted
after a new Drafts readiness failure and a failure in the card animation-control
witness. It ran 308.88 seconds including cleanup, at mean/max load
136.20/190.95; no history was promoted and no comparison arm ran.
`board-readiness.spec.ts:280` observed a closed Drafts modal for 65,347 ms
after the real click, despite the preceding exact global count check passing.
Its trace placed the delayed data response inside the click operation. A
deterministic witness lasting longer than the old two-second delay reproduced
the lost loading precondition on both engines: New became enabled before the
test finished its pre-data assertions. Four related cases now hold data with
an explicit latch, release it in `finally` and drain their routes. The existing
timed `openProject` lower-bound proof remains unchanged. All ten affected
desktop cases pass, including real global-count and Drafts-modal assertions.
Post-release readiness uses the graced default; its two obsolete fixed-delay
audit exceptions are removed. The card witness now
distinguishes CSS class animations from the other timelines `getAnimations()`
returns. A deterministic competing native Web Animation reproduced its overly
broad check on both engines: CSSAnimation was paused while Animation was
running. Filtering the controlled owner passed both cases in a 15-second pool;
strict TypeScript checking passed. Neither failure is attributed to load
without further evidence.

This preparation also copied Cargo's mutable artifact twice while a targeted
test build was replacing it. The optimized run used SHA-256 `dc4e2297…330a053`;
the baseline copy had `2d3ae503…56a84a`. They were not a matched binary pair.
Future preparation must pin one copy first, hash it, copy both arms from that
file and require both hashes to match the manifest before every arm. This
attempt supplies diagnostic evidence only, retained in `/tmp/sh812-v2-bench/`
and `.storyhook/logs/sh812-performance-v2/` with the driver and manifest.

**Third warm-up, 2026-10-03 UTC: diagnostic only.** Both snapshots used one
pinned binary with SHA-256
`408f7486a30d37b325772b7e2681c58eb0ac185eedcb7096aefc6dc5f49f63ab`.
The driver checked binary and spec hashes before execution. The complete
optimized leg exited 1; it produced no history and started no comparison arm.

| Measurement | Result |
|---|---|
| Selection and completion | 1,507 results, 16 slices, 8 jobs |
| Playwright outcomes | 1,491 accepted, including one declared failure; 15 skipped; one unexpected failure |
| Wall time, including planning and cleanup | 1,487.20 s |
| Sampled one-minute load | Mean 117.47; maximum 167.74 |
| Longest slice | 1,123 s |
| Slice seeding | Median 5 s; range 3–7 s |
| Slice daemon readiness | Median 0 s; range 0–1 s |
| Slice selection bookkeeping | Median 0 s; range 0–1 s; no redundant Playwright listing |

The Node slice accepted all 51 tests, but the original receipt incorrectly
rejected SH-813's intentional `test.fail()` proof. The reporter now counts
outcomes against `expectedStatus` and returns its final status override through
the documented asynchronous API. Both new expected-failure/unexpected-pass
regressions were RED; all nine real-runner receipt scenarios, two focused Node
harness cases and strict TypeScript checking then passed.

The remaining failure is `verification-layout.spec.ts:116` on mobile WebKit,
at the 375px short-status sample. After `page.clock.runFor(1000)`, the chip
remained at `2m 24s total` / `18s`, rather than `2m 25s total` / `19s`, for
49,445 ms. Initial text and containment assertions passed. Diagnosis and a
separate regression repair are adopted into SH-812; this trace alone does not
establish a product defect. Preserve elapsed-label and geometry coverage.

That trace also records a fresh `/data` response during the clock advance.
The fixture returned the same 144/18-second snapshot on every response,
resetting the product's elapsed baseline. A deterministic real-navigation
refresh reproduced the lost second on all four projects. Each sample's fixture
now advances its elapsed data with the browser clock. The exact one-second
proof holds its wall-time target while real timer callbacks run, separating
that target from the footer interval's phase. Real refreshes must retain the
advanced label, accessible name and containment. Both layout cases passed on
all four projects (eight cases, 71-second pool); strict TypeScript passed.
Response handlers drain before teardown, and fixed wall time is restored in
`finally`. No product code changed. Regression logs are retained under
`.storyhook/logs/sh812-resumed/sh812-layout-*`.

The full logs, sixteen receipts, phase timings, load samples, manifest, scripts
and failing trace are retained in `/tmp/sh812-v3-bench/` and
`.storyhook/logs/sh812-performance-v3/`. A fresh successful warm-up and two
matched pairs remain required. No performance improvement is established.

**Fourth measurement attempt, 2026-10-03 UTC.** The final reporter and layout
repairs reached a successful unfiltered warm-up at source `2c95d90f`:

| Measurement | Result |
|---|---|
| Completed selection | 1,507 tests, 261 project/file groups, 16 slices, 8 jobs |
| Outcomes | 1,492 accepted, including one declared failure; 15 configured skips; no failures or retries |
| Warm-up wall time | 1,593.42 s |
| Sampled one-minute load | Mean 104.19; maximum 146.70 |
| Eligible timing history | 252 complete-file records |
| Pinned binary SHA-256 | `09320d1486160b253ab03d5334e964d42cc212a74898e25ec0e3d500c00cbf59` |

The first count-based baseline then exposed a different prerequisite on
Chromium: `stale-repo-list.spec.ts:324` called `settledBoundingBox`, and
`support.ts:1933` failed in `scrollIntoViewIfNeeded` because the target
detached while waiting for stability. This happened during preparation of
`a Settings project press survives catalog failure`, before the mouse press.
The same WebKit case passed. The failure is adopted into SH-812 for
deterministic diagnosis and a separate regression repair; no product cause
is inferred from the trace alone.

The failed baseline was stopped through normal TERM cleanup. Its interrupted
727.74 seconds at mean/max load 141.87/212.79 is diagnostic only. The driver
started no optimized comparison arm and preserved the successful warm-up's
history unchanged. No completed comparison pair or speedup is established.
The accepted load-comparison rule is symmetric and conservative:
`abs(baseline_mean - optimized_mean) / min(baseline_mean, optimized_mean)`
must be at most 20%. Compare individual test identities and outcomes too.

Evidence is retained in `/tmp/sh812-v4-bench/` and
`.storyhook/logs/sh812-performance-v4/`: complete warm-up logs/receipts,
phase timings, load samples, history snapshots, interrupted baseline artifacts,
and `catalog-failure/trace.zip`. The baseline pool's temporary per-slice logs
were removed by TERM cleanup; its failure trace and partial parent log remain.
Refresh matched snapshots after the adopted repair, then complete the approved
warm-up and interleaved comparisons before submission.

**Catalog preparation repair and fifth measurement, 2026-10-03 UTC.**
The Settings trace proved that its navigation started a catalog read before
the test installed interception. That response rebuilt the target table during
coordinate preparation. Commit `40aa68d4` installs the holds before selecting
the surface and keeps the no-refresh control pending through the press.
A regression requires no catalog reply to complete before `mouse.down`.
It failed on both desktop engines before the repair; all eighteen affected
gesture cases passed afterward (13-second pool), with strict TypeScript clean.
The shared geometry helper, real coordinate input and press-gate assertions
are unchanged.

Fresh matched snapshots from `40aa68d4` produced another successful warm-up:

| Measurement | Result |
|---|---|
| Selection and outcomes | 1,507 results; 1,491 passes, one declared failure, 15 configured skips; no retries |
| Warm-up wall time | 1,585.37 s |
| Sampled one-minute load | Mean 84.18; maximum 178.98 |
| Eligible timing history | 252 complete-file records |
| Pinned binary SHA-256 | `fa7373332817b56e0784699d44fef020afbd4b7ad31c5b31df91a5a24cbfec7f` |

Baseline 1 exposed a new WebKit prerequisite in `list-wrapping.spec.ts:70`.
The label input was filled and Enter pressed, but the `layout-gamma` drawer
chip stayed absent for the full 63,535 ms assertion budget. The test failed
before its wrapping assertions. The cause is not established. Diagnosis,
deterministic regression and a separate repair are adopted into SH-812;
preserve the label and geometry checks rather than extending patience.

The failed baseline was stopped through normal TERM cleanup at 430.57 seconds,
mean/max load 123.03/168.88. Those interrupted numbers are diagnostic only.
No optimized comparison arm ran, and the successful history stayed byte-for-byte
unchanged. No completed pair or performance improvement is established.
Unlike the previous attempt, the live per-slice logs were copied before cleanup.
Evidence is in `/tmp/sh812-v5-bench/` and
`.storyhook/logs/sh812-performance-v5/`, including
`baseline1-live-diagnostics/list-wrapping-only-titles--dc857-bel-chips-wrap-in-list-rows-webkit/trace.zip`.
Complete the adopted repair and refresh matched snapshots before resuming the
approved warm-up and two comparisons.

**Label write repair and sixth measurement, 2026-10-03 UTC.** The retained
trace established a product race: the beta write was still pending when the
editable input accepted gamma, but `addLabel` silently refused its Enter.
Commit `d26c6f1c` makes the existing serialized-write contract explicit.
The input becomes read-only while preserving focus, removal buttons become
disabled, suggestions close, and a visible status plus `aria-busy` identify
the pending write. Success and refusal restore editing; failure retains the
existing rollback and explicit retry. The local create editor stays editable.

The held-write regression failed on both browsers before the repair. All eight
new add/remove success/failure cases passed afterward, including focus, native
read-only behavior, rollback text and persistence of a subsequent label.
The existing label-editor and wrapping cases passed too: sixteen cases in a
26-second pool. Ten impacted keyboard repeat/composition cases passed in a
12-second pool. Strict TypeScript, the production build and diff checks passed.

Fresh snapshots from `d26c6f1c` selected 1,515 tests in sixteen slices. The
warm-up exposed a new WebKit prerequisite: `toolbar-containment.spec.ts:45`
failed exact header-geometry equality after board scrolling at 1280px and
200% text. Initial containment passed, but the later header bottom was 632
instead of 631.84375 pixels; other control positions changed slightly too.
The cause is not established. Diagnosis and a regression repair are adopted;
preserve exact scroll invariance and containment rather than widen tolerances.

The warm-up was stopped through normal TERM cleanup at 921.78 seconds,
mean/max load 86.39/209.63. Its pinned binary SHA-256 was
`3e382763e6bc43e09df4b8cad8649af13cf9f4598bfdbbeaf8cf41f6c2456ada`.
This interrupted result is diagnostic only: no history was promoted and no
comparison arm ran. Logs, live per-slice diagnostics, trace, load samples,
manifest and scripts are in `/tmp/sh812-v6-bench/` and
`.storyhook/logs/sh812-performance-v6/`. The trace is under
`warmup-live-diagnostics/toolbar-containment-deskto-c65da-its-at-1280px-with-200-text-webkit/`.
No completed comparison pair or performance improvement is established.

**Toolbar settling repair, 2026-10-03 UTC.** A production-CSS probe reproduced
WebKit reporting transitions as `finished` at 150 ms while computed font sizes
still held 13.08444 px or 25.904247 px, instead of the final 13 px or 26 px.
Two of 25 text-size changes exposed that discrepancy; this is mechanism
evidence, not a failure-rate estimate. Sampling on a render frame removed the
discrepancy in the corresponding probe. `awaitSettled` now makes its existing
subtree animation check from `requestAnimationFrame`, so a between-frame
animation state cannot release a stale geometry read.

Three controlled scheduler regressions failed before the repair and pass
afterward. They cover finished state before layout, a paused intermediate
frame, continuing motion, and exclusion of unrelated document animations.
All 34 directly impacted browser cases passed, including the unchanged toolbar
matrix, exact scroll invariance and coordinate-press checks. The complete
focused pool was 37 cases in 72 seconds. Strict TypeScript, the Rust subtree
contract and diff checks passed. No product motion or geometry assertion changed.
Logs: `.storyhook/logs/sh812-resumed/sh812-settle-*`; diagnostic probes are
retained there too. Fresh matched measurements are still required.

### What slicing exposed

- **Order-dependent specs.** Slices change which files run before a spec. The
  one-file-per-slice isolation proof is the detector: exactly one spec fails
  alone, `story-submenu-hover.spec.ts`'s right-side pointer travel, on both
  engines and on the unchanged runner too. It was green only because
  `dispatch.spec.ts` ran first and moved AA-1 out of todo (bisected). SH-811;
  not quarantined, since it may be a product defect.
- **First-wave load grace.** Slices that start together sample the lagging
  1-minute load at config evaluation and keep an ungraced `expect.timeout` for
  their whole life. SH-813 corrects this at assertion entry, as described below.
- **A dead release gate nobody saw.** The baseline found
  `settings-version.spec.ts` red on both engines since SH-756 added a Settings
  section on 2026-09-21: the browser suite runs only in the release tier. A
  leg measured in minutes can run far more often.

### Assertion defaults after startup (SH-813)

The shared `expect` now calls the existing `gracedPatience()` policy when an
assertion is constructed. `expect.configure()` supplies that default to
Playwright, which still owns matching, polling and failures. Saved `soft` and
`poll` functions also sample on invocation; configuring or extending an expect
preserves this behavior. Explicit matcher, poll and configured timeouts remain
exact, including zero. Configuring a timeout of `undefined` restores dynamic
sampling. `toPass` retains its separate native timeout policy.

The two custom text guards forward `options.timeout ?? this.timeout` to their
native delegates. Otherwise those delegates would discard the new default and
fall back to startup patience. The hidden-decoration checks remain in place.
The source fence requires both the text guards and the grace adapter.

Idle assertions still have 5,000 ms; `E2E_LOAD_GRACE=0` still disables grace;
the assertion ceiling remains 300,000 ms. Startup config and the whole-test
watchdog retain their existing roles. Each test reports each distinct graced
default once, on stderr and in annotations. These reports describe defaults,
not overrides of explicit proof deadlines.

Call-time sampling was selected over a jobs/core floor and staggered startup.
At SH-792's recorded mean load 87 on 10 cores, the existing policy calculates
43,500 ms, whereas a 12-jobs/core floor calculates only 6,000 ms. Staggering
cannot refresh a default retained for an entire slice. The one-minute metric
still lags new bursts; this change neither predicts them nor extends an
assertion that has already started.

Controlled regression evidence uses the pinned Playwright 1.63 runtime:

| Check | Before | After |
|---|---|---|
| Sample changes from ratio 0.3 to 8.7 | Matcher receives 5,000 ms | Matcher receives 43,500 ms |
| DOM becomes ready after 6,000 ms; new ratio is 2 | Startup budget is only 5,000 ms | Visibility and both text matchers pass with 10,000 ms on Chromium and WebKit |
| Remove effective timeout forwarding from text delegates | Exact-budget regression rejects the config's 50 ms fallback | Correct delegates report the requested 100/200 ms on both engines |

The eight Node regression cases and the original seven text-guard cases per
desktop engine pass. Tests also preserve explicit deadlines, disabled grace,
the ceiling, soft failures, promises, custom messages, asymmetric matchers,
derived instances and `toPass` behavior. Strict TypeScript checking passes.

The comparison also reproduced a different cause in `card-exit-reclaim`:
freezing JavaScript time does not freeze the 200 ms CSS exit animation. Its
real `animationend` could detach the card between driver calls, before the
test reclaimed it. The adopted repair pauses only `.card.exiting` animation
playback in a scoped helper, alongside the JavaScript clock, and removes the
rule in `finally`. The renderer and both removal callbacks remain real.
The new regression was red on both engines (`running` instead of `paused`).
All five card-exit cases now pass on both engines. A separate native animation
witnesses more than one exit duration of CSS time while the card stays held;
the production fallback then preserves it at 599 ms and removes it at 600 ms.
This distinguishes controlled completion from merely increasing a timeout.
See the timing APIs covered by [Playwright's clock](https://playwright.dev/docs/clock).

The 2026-10-02 comparison used the five files named by SH-813 on both desktop
engines: `card-exit-reclaim`, `board-sort`, `create-story-project`,
`verify-override-drop` and `card-transient-classes`. Each arm selected 86 cases
in 10 concurrent slices with `STORYHOOK_E2E_JOBS=12`, one worker per slice and
no retries. The card-exit repair and its new case were identical in both arms;
only baseline versus corrected assertion integration changed. The locked
Playwright dependencies and Rust binary bytes were identical throughout.
The binary SHA-256 was
`861dd08b694705cbf681b8ddbe013b57725347912a4a702a1dabf299eb939507`.

A temporary runner copy excluded compilation and otherwise retained the
production selection, binary lease, isolated fixtures, browsers and cleanup.
Two preliminary runs were discarded when their build changed the binary hash;
even a fixed `STORYHOOK_BUILD_ID` did not establish byte equality. The table
contains only the four valid, interleaved prebuilt-binary runs. Wall time
includes setup; pool time covers the concurrent slices. Load was sampled each
second on 10 cores. Budget ranges include config/worker startup samples and
the corrected adapter's assertion-entry diagnostics.

| Run | Wall / pool (s) | Load mean / max | Pass / fail | Startup default (ms) | Assertion-entry default (ms) |
|---|---:|---:|---:|---:|---:|
| Baseline 1 | 223.0 / 216 | 98.0 / 134.9 | 82 / 4 | 24,531–64,954 | Retained startup default |
| Corrected 1 | 282.1 / 277 | 128.7 / 155.7 | 83 / 3 | 51,536–76,463 | 50,361–77,868 |
| Baseline 2 | 202.7 / 200 | 130.5 / 150.4 | 82 / 4 | 56,753–69,923 | Retained startup default |
| Corrected 2 | 260.5 / 252 | 136.1 / 161.4 | 81 / 5 | 53,224–80,663 | 55,337–80,693 |

These runs do **not** establish a speed or failure-rate improvement. Load was
unequal, and startup was already contended: the original ungraced 5-second
first-wave signature was not reproduced. The controlled runtime regressions
above establish the stale-default correction. All card-exit and board-sort
cases passed in every arm. Remaining failures were the transient disabled
controls and animation-owned classes retained by SH-812, plus response-rewriting
routes reading disposed responses during teardown. The latter occurred in the
stale-draft case and `injectVerifyingCard`; SH-813 repaired that separate
lifetime defect after the comparison, as described below.
The raw logs, per-second samples and traces are retained under
`/tmp/SH-813-final-pairs` and summarized in SH-813's discussion.

`withDrainedRoutes(page, body)` now waits for response-rewriting handlers in
`finally`, before their page context can dispose fetched responses. The two
verifier-injection files use it inside their page fixture. The stale-draft and
project-deletion tests use local scopes; the latter previously drained only
after success. Other tests that intentionally hold requests remain unchanged:
a suite-wide drain could wait forever for a test-owned latch that was never
released. Callers must release such holds before ending this scope. The helper
uses [Playwright's native wait behavior](https://playwright.dev/docs/api/class-page#page-unroute-all)
and does not suppress handler failures.

A real fetched response held beyond both successful and failed body completion
made all four new desktop cases fail against the non-draining stub. They pass
with the helper, preserving the body's value or error. All 34 new or affected
browser cases pass: the four lifetime cases, 20 override cases, six verifier
status cases and four create-dialog cases. The four affected source-corpus
guards and strict TypeScript checking also pass. Evidence is in
`/tmp/SH-813-route-{red,green}.log` and `/tmp/SH-813-create-routes-green.log`.

## Verifier Python cases: bounded process concurrency (SH-793)

The lifecycle and verdict Rust wrappers now call
`scripts/tests/run_verifier_tests.py`. Each suite admits two cases at a time,
each in a fresh interpreter, so their combined default bound is four cases.
The gate-scheduling suite stays unchanged. This small fixed bound does not
multiply by the host's CPU count or increase the outer Rust battery budget.

The runner discovers methods of module-owned unittest classes. In particular,
the verdict suite's imported `VerifierLifecycle` fixture is not another copy
of the lifecycle tests. Each child uses the parent's `sys.executable`, `-B`,
and the original file's exact `Class.method` selector. Case globals, mocks,
repositories and locks remain isolated, and no bytecode enters the checkout.

Each child's stdout and stderr share a temporary capture. The parent emits
whole captures with the case name and exit status, followed by aggregate
counts and elapsed time. Failed or crashed cases cannot hide later cases.
INT, TERM and HUP stop admissions and drain active cases before returning a
cancellation status. Children remain in the enclosing process group; the
verifier retains cancellation authority. Existing load-graced case deadlines
remain authoritative, without a second, shorter runner deadline.

```sh
python3 -B scripts/tests/run_verifier_tests.py lifecycle --list
python3 -B scripts/tests/run_verifier_tests.py verdict --jobs 1
python3 -B scripts/tests/run_verifier_tests.py lifecycle --jobs 2 VerifierLifecycle.test_invalid_cleanup_budget_cannot_launch_a_session
```

Direct execution of the original unittest files still works. Runner contracts
exercise real children with release barriers to prove overlap and the bound,
process isolation, exactly-once execution, serial mode, complete diagnostics,
failures, crashes, launch refusal, and all three cancellation signals. The
Cargo contract is `verifier_case_runner`; its checkout inputs are declared in
the impact manifest. Python 3.9 and 3.14 both run the contracts.

The lifecycle suite now has 79 cases and the verdict suite 27. The ordinary
orphan case moved entirely to
`merge_gate::a_red_gate_whose_test_left_an_orphan_is_red_not_infrastructure`.
That retained regression also checks the completed owner's cleared gate fields
and the exact survivor PID in the cleanup log. TERM-resistant, slow-census,
pinned-leader and cancellation cases remain distinct coverage.

The first parallel run reproduced the cancellation/restoration failure described
by SH-862. A controlled reproduction established that forced lifecycle
cancellation can stop restoration after gate quiescence. The fixture now tests
the documented owned recovery boundary after a matching escalation; ordinary
cancellation still requires immediate restoration. This adopted repair adds
three cases, explaining the increase from the planned 76 to 79. Production
deadlines remain unchanged. See [the diagnosis](../rca/sh-793-cancelled-restoration.md).
The landed SH-789 change adds four verdict cases. Its repeated-kill behavior
and fixed reaping deadline are preserved; the runner's inventory contract reads
the direct verdict command's declared test classes instead of keeping a second
manual class list.

All three Rust verifier wrappers pass, as do the new runner contract, retained
orphan regression, and directly affected isolation, timing, impact-selection
and fixture-containment contracts. Formatting and targeted Clippy with warnings
denied pass. The full suite remains with the centralized verifier.

Measured on 2026-10-02, on the ten-core M1 Max with Python 3.14.7. All runs
used the reconciled implementation in `c775c476` (the later ancestry merge
changed no source). Runs were sequential, with no concurrent build from this
lane. Serial commands used `python3 -B scripts/tests/test_verifier_<suite>.py -v`;
parallel commands used `python3 -B scripts/tests/run_verifier_tests.py <suite>
--jobs 2`. Wall time includes interpreter startup. One-minute machine load
was sampled approximately once per second. Every selected case passed.

| Suite | Execution | Cases | Wall time | Mean load | Load range |
|---|---|---:|---:|---:|---:|
| Lifecycle | Original serial | 79 | 330.004 s | 15.212 | 8.965–26.996 |
| Lifecycle | Two workers | 79 | 159.879 s | 10.493 | 8.725–13.479 |
| Verdict | Original serial | 27 | 239.259 s | 23.529 | 12.309–35.563 |
| Verdict | Two workers | 27 | 57.854 s | 11.874 | 8.674–16.023 |
| Lifecycle | Serial confirmation after parallel | 79 | 290.868 s | 16.954 | 7.857–40.284 |
| Verdict | Serial confirmation after parallel | 27 | 140.869 s | 8.912 | 7.396–13.200 |

Against the faster serial observations, elapsed time fell 45.0% for lifecycle
and 58.9% for verdict. Ambient contention varied; the lifecycle serial samples
had higher mean load, while the verdict confirmation had lower mean load than
its parallel sample. The figures include those effects. The barrier-based
runner contracts independently prove overlap and the concurrency ceiling.
No timing ratio is encoded as a test assertion.

## Roadmap to 15 minutes

What this story leaves, in order of leverage. Each is a story related to
SH-783.

1. **SH-792 — e2e: shard the browser projects and decide cross-engine
   coverage.** Done: see "The browser leg as concurrent slices" below
   (about 47 to about 10 minutes under ambient load). What it left: SH-811
   (an order-dependent spec the slices exposed), SH-812 (pack slices by
   measured duration, cut per-slice setup), SH-813 (first-wave load grace),
   SH-807 (an interrupted run's dispatch child can outlive cleanup).
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
