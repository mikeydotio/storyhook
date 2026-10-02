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

### What slicing exposed

- **Order-dependent specs.** Slices change which files run before a spec. The
  one-file-per-slice isolation proof is the detector: exactly one spec fails
  alone, `story-submenu-hover.spec.ts`'s right-side pointer travel, on both
  engines and on the unchanged runner too. It was green only because
  `dispatch.spec.ts` ran first and moved AA-1 out of todo (bisected). SH-811;
  not quarantined, since it may be a product defect.
- **First-wave load grace.** Slices that start together sample the lagging
  1-minute load at config evaluation and keep an ungraced `expect.timeout` for
  their whole life. SH-813.
- **A dead release gate nobody saw.** The baseline found
  `settings-version.spec.ts` red on both engines since SH-756 added a Settings
  section on 2026-09-21: the browser suite runs only in the release tier. A
  leg measured in minutes can run far more often.

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

## Scheduled file-isolation proof (SH-814)

SH-811 passed only after another spec moved a seeded story. A fresh fixture
exposed it. The normal sliced leg changes its grouping over time and cannot
guarantee that each file passes alone. This detector runs every selected
project/file pair in its own production fixture. It does not change the
normal slice planner, browser matrix, workers, retries, or gate receipts.

| Command | Contract |
|---|---|
| `make e2e-isolation ARGS=--project=webkit` | Isolate every selected file in this checkout; optional ordinary selection filters |
| `make e2e-isolation-watch` | Fetch integration and run one complete scheduled observation |
| `make e2e-isolation-status` | Read the latest result and age; no fetch or test |
| `make e2e-isolation-plist` | Render the daily LaunchAgent; does not install or start it |

**Cadence: daily at 04:17, machine-local time, eight concurrent slices.** The
historical 807-second proof supports daily execution instead of adding this
cost to each merge. The schedule repeats even when the tree is unchanged.
It uses the integration role in `scripts/branch-policy.sh` (currently `dev`),
so it detects a merged regression before the next stable release. There is no
GitHub Actions job and no change to `make test` or `make test-full`.
[Apple's calendar scheduling](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/ScheduledJobs.html)
catches up after sleep; a powered-off or logged-out machine cannot run a user
LaunchAgent. Status becomes stale after two daily intervals or a locally known
integration-tree change. Missing and interrupted observations never mean success.
The proof retains the runner's display-wake assertion for WebKit safety and can
turn on the display. Its child sets `CI=1` so the existing `forbidOnly` policy
rejects an accidental focused test.

### Evidence and diagnosis

The isolation planner writes one exact Playwright test-list for each selected
project/file pair, independent of test counts and duration weights. The built-in
JSON reporter proves the executed project/file, test count and completed
results agree with the plan. Empty selections, discovery errors and missing
evidence fail. Caller-owned partitions and overrides of the config, reporters,
output, concurrency or retries are incompatible with isolation mode.

After the initial pool finishes, a one-job pool repeats only its failed files,
each with a fresh daemon and seed. Initial failures remain failures even when
the repeat passes. Reports say `failed again`, `not reproduced on serial rerun`,
or `rerun infrastructure failure`; none automatically diagnoses an order
dependency or proves a load flake. Failure output gives the exact filename,
artifacts and a serial command using its retained test-list. Normal Playwright
selection filters still apply within each isolated file.

`STORYHOOK_E2E_RESULTS_DIR` selects an absolute, new output directory. Its parent
must exist; an existing output directory is refused without deleting it.
Without this variable the runner retains its existing `e2e/test-results/current`
behavior. A run contains `slices/manifest.json`, per-slice `logs/`, `reports/`,
`selected/`, `executed/` and `verdicts/`, separate `reruns/` evidence, and the
final `isolation.json`. Use the reported command from the same checkout and
revision. Set a new output directory to keep the rerun's output too.

The scheduled controller and subject checkout live under
`~/.local/share/storyhook/e2e-isolation`. Each attempt retains a unique
`runs/<attempt>/run.log`, `record.json` and `artifacts/`; `latest.json` is replaced
atomically. An advisory lock prevents overlapping passes. The watcher refuses
dirty tracked files, unavailable remotes and absent or mismatched toolchains.
Reports are retained for investigation; no automatic deletion is performed.

### One-time schedule activation

Run from a clean committed implementation checkout. This creates a durable
controller, not a linked worktree. It never reads or changes the main checkout.
The controller must remain at the recorded commit. Each pass requires that
commit to be an ancestor of fetched integration; until merge, status is
`awaiting integration` and no proof runs. Installation does not run the proof.

```sh
set -e
bash scripts/python-runtime.sh -- python3 - <<'PY'
import json, pathlib, subprocess
source = pathlib.Path.cwd()
base = pathlib.Path.home() / '.local/share/storyhook/e2e-isolation'
if base.exists():
    raise SystemExit(f'{base} exists; inspect it instead of overwriting it')
def git(*args):
    return subprocess.check_output(['git', *args], text=True).strip()
if git('status', '--porcelain', '--untracked-files=no'):
    raise SystemExit('Commit tracked edits first')
revision = git('rev-parse', 'HEAD')
remote = git('config', '--get', 'remote.origin.url')
base.mkdir(parents=True)
for name in ('controller', 'checkout'):
    destination = base / name
    subprocess.run(['git', 'clone', '--no-hardlinks', '--no-checkout',
                    str(source), str(destination)], check=True)
    subprocess.run(['git', '-C', str(destination), 'checkout', '--detach', revision], check=True)
    subprocess.run(['git', '-C', str(destination), 'remote', 'set-url', 'origin', remote], check=True)
(base / 'settings.json').write_text(json.dumps({'schema': 1, 'required_commit': revision}) + '\n')
PY
observer="$HOME/.local/share/storyhook/e2e-isolation"
(cd "$observer/checkout" && make e2e-install)
shasum -a 256 "$observer/checkout/e2e/package-lock.json" | awk '{print $1}' > "$observer/checkout/e2e/node_modules/.storyhook-lock-sha256"
make -s e2e-isolation-plist > /tmp/io.mikey.storyhook.e2e-isolation.plist
plutil -lint /tmp/io.mikey.storyhook.e2e-isolation.plist
mkdir -p "$HOME/Library/LaunchAgents"
test ! -e "$HOME/Library/LaunchAgents/io.mikey.storyhook.e2e-isolation.plist"
install -m 644 /tmp/io.mikey.storyhook.e2e-isolation.plist "$HOME/Library/LaunchAgents/"
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/io.mikey.storyhook.e2e-isolation.plist"
launchctl print "gui/$(id -u)/io.mikey.storyhook.e2e-isolation"
```

The plist captures the invoking PATH. Use a durable PATH containing `story`,
`cargo`, `node`, `npm`, `git` and the supported Python; do not capture an agent
worktree's runtime shim. HTTPS credentials must work without interaction.
Console output goes to `launchd.log` under the observer directory. If integration
changes its package lock, rerun the provisioning and digest commands above in
the subject checkout identified by the failure. Update controller code only by
deliberately installing a new tested commit and matching `settings.json`.

To stop recurrence, run
`launchctl bootout "gui/$(id -u)/io.mikey.storyhook.e2e-isolation"`, then remove
only its LaunchAgent plist. Keep the durable checkout and reports for inspection.

### Regression coverage

`tests/e2e_isolation.rs` runs planner, evidence and watcher contracts. Watcher
contracts use real temporary Git repositories; only the external proof command
is a fixture. The Makefile and reusable-leg contracts also fence target wiring
and Python-helper fingerprint invalidation.

Run the new production-flow regression explicitly with
`bash scripts/python-runtime.sh -- python3 -B tests/support/e2e_isolation_live.py`.
It creates two temporary Node-project specs that use the real daemon and seed:
the first writes a fixture marker and the second incorrectly needs it. Together
they pass. Isolation must identify only the second file, reproduce its failure
in a different fresh fixture, and keep the result red. The temporary specs are
removed after the test. This targeted regression does not run the existing
browser suite.

Validation on 2026-10-02: the live regression passed with two shared-fixture
tests, two isolated files and one fresh serial repeat. The planner/evidence
contracts cover nine cases; the watcher contracts cover eleven, including a
child deliberately forked during cancellation. After normal teardown, the
watcher kills remaining members of its owned process group. Wiring, fingerprint,
selection, fixture ownership and display-safety contracts passed independently.

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
