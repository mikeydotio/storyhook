# Rust battery discovery — SH-795

Design and measurements for storyhook v3.0.3, starting at
`3b6cc7c6c339caccd51695b402abc8305b2dd2a5` (2026-10-02).

## Problem and decision

SH-783 measured a fresh-build core leg at 694 s, of which pooled execution
accounted for 443 s. That difference was not a measurement of linking.
`run-tests.sh` listed the selected binaries twice, serially through Cargo;
`test-pool.py` then listed each binary again to choose a thread share.

The pool now owns binary discovery. Binary boundaries, linker configuration,
core/contracts classification, ledger output, and shared TestEnv daemon
lifetimes remain unchanged. Consolidation is not necessary to remove the
redundant launches.

## Execution contract

| Phase | Owner and behavior |
|---|---|
| Mode | One shell predicate selects a pool only for a nonempty targeted selection with a positive thread budget. Workspace, zero-budget and empty selections retain serial behavior. |
| Doctests | Cargo discovers their runnable total before pooled execution. Doctests still execute serially after all selected binaries, including after an ordinary test failure. |
| Build | The existing diagnostics adapter pre-builds selected targets. A failed build terminates discovery without executing binaries or doctests. |
| Artifacts | The discovery helper asks Cargo for test artifacts by package and target. Failed lookup or missing selected artifacts is an error. |
| Binary listing | At most the thread budget's number of listing workers run. Each applies the execution filters; default selection subtracts an ignored-only listing. Explicit ignored/include-ignored selection needs one listing. Both test and benchmark listing records use the existing counting convention. |
| Progress | One total includes every selected runnable binary case plus doctests. Discovery starts before that total, completes after it, and finishes before the first execution. No estimate or partial denominator is published on discovery failure. |
| Execution | Runnable counts also determine thread shares, with one slot for a zero-case binary. Longest-first scheduling, ordered captures, no-fail-fast behavior, rebuild refusal and duration history remain intact. |
| Refusal | Artifact, launch, listing, timeout and inconsistent-count errors name the command/target and preserve diagnostics. Queued discovery stops; owned listing children are killed and reaped. |
| Cancellation | Pool signal handlers are installed before artifact lookup. Listing checks the same cancellation flag as execution. Captures use regular files so inherited pipes cannot prevent cleanup. |
| Readiness | A private per-run marker is written only after successful complete discovery and progress publication. Its absence prevents the shell from running doctests. |
| Receipt reuse | Both pool and discovery modules are inputs to the core Rust fingerprint. Contracts already fingerprint the whole tracked tree. |

Internal pool arguments `--additional-total` (nonnegative, default zero) and
`--discovery-ready` carry the doctest count and shell handshake. They do not
change the public StoryHook CLI or the progress journal schema. Progress still
uses `gate-progress.sh`; with no journal its emitters remain inert.

The existing 120 s per-listing bound is retained. Discovery now reports its
expiry instead of silently falling back to an estimated scheduling count.

## Discovery measurements

The benchmark uses a dependency-free disposable crate named `storyhook` with
16 integration binaries. Each contains one passing test and one ignored test.
Its scripts resolve to the tracked production helpers; only `run-tests.sh`
and `test-pool.py` switch between baseline and changed versions. A private
fixture lock, separate target directory, budget 8 and a progress journal are
identical for all samples. No storyhook production tests execute here.

The first before/after groups encountered different machine load and are not
used to claim improvement. The retained comparison alternates warm runs:
before, after, after, before, before, after. All Cargo artifacts are fresh
(no `Compiling` lines). These samples omit doctests to isolate binary discovery.
The pool's existing `started` timer is printed at full precision immediately
after `pool.run` in both disposable script copies; this diagnostic is not a
production change. Non-test overhead is leg wall time minus that interval.

| Median of three warm samples | Before | After |
|---|---:|---:|
| Complete fixture leg | 7.212 s | 4.633 s |
| Pooled execution interval | 2.571 s | 1.224 s |
| Non-test overhead | 4.522 s | 3.520 s |
| Binary listing launches per run | 48 | 32 |
| Serial Cargo binary-discovery invocations | 2 | 0 |
| Runnable progress total | 16 | 16 |

Non-test overhead fell approximately **22%** in this fixture. Correctly
excluding ignored cases from thread shares also permits eight one-test jobs
instead of four two-slot jobs, explaining part of the execution improvement.
One-minute machine load ranged from 74.4 to 80.2 on 10 logical cores; other
sessions were active. This is evidence for the removed mechanism, not a
whole-gate speedup claim. Raw samples are in
[data/sh-795-discovery-timings.json](data/sh-795-discovery-timings.json).

To reproduce, generate `tests/case00.rs` through `case15.rs` with
`#[test] fn passes() {}` and `#[test] #[ignore] fn ignored() {}`, compile once,
and alternate the two runner versions with
`STORYHOOK_TEST_THREAD_BUDGET=8 bash scripts/run-tests.sh --only-no-doc case00 … case15`.
Use one persistent target directory, a separate journal per run, the same
budget and process class, and record load alongside each sample. Count
launches independently of elapsed-time comparisons.

## Build and linker measurement method

A disposable `git archive` of the starting HEAD preserves the workspace and
`.cargo/config.toml` compiler-slot wrapper. It runs only
`cargo test --workspace --no-run --offline --timings`: no test execution.
The sequence is initial build, unchanged warm build, then a one-line comment
appended to `src/lib.rs` and the same build again. An initial sandboxed attempt
could not acquire the machine-wide slot files; it was stopped and excluded.
The restarted initial build therefore has some dependency cache already warm.

The measurement sets `CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER` to a transparent
Python executable which forwards every argument to `/usr/bin/cc`, preserves
its exit status, and appends monotonic start/end times and the `-o` output path
as one JSON record. The workspace compiler-slot limit remains eight; it is
not replaced by a private bound. The timed interval includes the compiler
driver's link process, not just Apple's internal linker algorithm.

Build wall time, sum of individual link-driver intervals, and union of those
intervals are different quantities. The sum is aggregate work; intervals
may overlap. Wall time minus their union includes compilation, slot waits,
Cargo overhead and other non-link activity, and is not pure compiler CPU time.
[Cargo timings](https://doc.rust-lang.org/cargo/reference/timings.html) supply
per-unit context, not an isolated linker measurement.

## Build and linker results

Rust 1.98.0 (`88d9e12ae`), Cargo 1.98.0 (`797e8a9bc`), Apple clang 21.0.0
(`clang-2100.3.34.2`), Apple Silicon, 350 workspace integration targets.
All three commands succeeded without warnings. Machine load was high and
other sessions shared the compiler slots.

| Build-only sample | Wall | Link calls | Sum of link intervals | Union of link intervals | Wall without a link driver active |
|---|---:|---:|---:|---:|---:|
| Initial, partly warm dependency cache | 1167.134 s | 353 | 320.341 s | 266.909 s | 900.225 s |
| Unchanged warm tree | 2.941 s | 0 | 0 s | 0 s | 2.941 s |
| One-line library comment | 764.548 s | 353 | 527.278 s | 371.427 s | 393.121 s |

The edited build's median link-driver interval was 1.226 s (maximum 6.408 s).
Linking is material: at least one link driver was active for 48.6% of that
build's wall time. This is **not** a 48.6% achievable speedup: compilation can
overlap those intervals, slot waits affect scheduling, and the measurement
wrapper adds overhead. The initial and edited samples started at one-minute
loads of 65.3 and 99.9, respectively. Their link medians cannot establish a
cache effect or a change in linker performance under such different load.

A ten-pair `/usr/bin/cc --version` probe measured median outer wall times of
0.373 s directly and 0.807 s through the instrumentation wrapper (median
paired difference 0.425 s). This noisy process-start probe quantifies that
instrumentation is not free; it is not a production-link benchmark. Python
startup precedes the inner interval recorded for each actual link. Do not
subtract a per-call estimate from build wall time: concurrent calls overlap.
Raw per-link intervals, phase summaries and overhead probes are retained in
[data/sh-795-build-timings.json](data/sh-795-build-timings.json).

The result supports evaluating the alternatives below, but it does not select
a replacement toolchain or justify changing the suite's scheduling units.
SH-795 therefore retains those interfaces and ships the independently
measured discovery improvement.

## Alternatives

| Option | Benefit | Costs and disposition |
|---|---|---|
| Keep binary boundaries; remove duplicate discovery | Removes one launch per binary and all serial binary listing; preserves current scheduling and selection. | Implemented and covered by real-Cargo and fault-injected regressions. |
| Consolidate into fewer integration binaries | Fewer links and process launches; Cargo documents this arrangement for large integration suites. | Would change `select-tests.sh`, core/contracts partitioning, coverage-map keys, pool granularity and duration history. Combining core and checkout readers would defeat receipt separation. Shared TestEnv daemons would live across larger test populations. Retained boundaries avoid these unmeasured trade-offs. |
| Adopt Mach-O LLD | Potentially faster individual links without changing test identities. | `ld64.lld` is absent from PATH and the installed Homebrew Rust sysroot. Requires a provisioned, versioned toolchain and platform-specific configuration, plus debug-info/signing/runtime validation. No evidence here measures its benefit against this machine's current Apple linker; no linker change is made. |

Cargo's [integration-target documentation](https://doc.rust-lang.org/cargo/reference/cargo-targets.html#integration-tests)
explains consolidation. LLVM's [Mach-O LLD documentation](https://lld.llvm.org/MachO/index.html)
describes its driver configuration; its generic speed claim is not a benchmark
of this repository or the installed Apple toolchain.

## Regression evidence

`tests/battery_completion/discovery.rs` drives real Cargo/libtest selection,
workspace integration/library artifacts, ignored modes, filters, exact/skip
selection, zero cases, doctests, and progress ordering. Its Python bridge runs
`scripts/tests/test_test_discovery.py`, which injects external-command faults
while production code owns discovery and execution.

The fault cases cover missing/failed artifacts, same-name application/test artifact collisions, missing executables, all-case
and ignored-case listing failure, inconsistent counts, timeout cleanup,
cancellation before launch and during listing, descendants, queued work,
parallel listing, budget one, failed progress writes and continuation after a
red binary. Existing battery tests retain ordered output, ledger attribution,
no-fail-fast and running-binary cancellation. Serial gate-lock progress tests
remain unchanged. The receipt regression edits each of the two pool modules.

Validation of the implementation:

| Check | Result |
|---|---|
| New Python discovery/runner regressions | 16 passed |
| `battery_completion` | 10 passed; two real-Cargo counting cases passed again after target-kind hardening |
| Directly impacted serial progress cases in `gate_lock` | 4 passed |
| Expanded pool/discovery receipt case in `gate_leg_reuse` | 1 passed |
| Targeted Clippy (`battery_completion`, `gate_lock`, `gate_leg_reuse`) | Clean with `-D warnings` |
| Rust formatting, shell syntax, Python syntax, whitespace | Clean |
| Full suite | Not run; owned by the central verifier |

Red-to-green evidence also includes running the new duplicate-discovery
regression against the original runner, a same-name application artifact
replacing a test artifact before hardening, and an unchanged core fingerprint
after editing the discovery helper before registering its dependency.

The measuring host also retains the three standalone Cargo HTML reports,
raw build logs, timing wrapper and build driver under
`~/Enderchest/storyhook/sh-795-build-timings-b92ddb6e/`. The committed JSON data
above remains available without that machine or its disposable build directory.
