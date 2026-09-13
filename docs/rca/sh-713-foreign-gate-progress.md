# SH-713: Foreign-gate execution and output reporting

## Failure and evidence

In v2.4.2, AGE-93 / PR190's ordinary gate log grew while the progress comment
reported `NO GATE OUTPUT` and three completed preflight rows. No false receipt
or automatic kill was observed in the reported specimen. Last known good is
unknown; the historical live attempt was not restarted or modified.

Two independent regressions reproduce the display defects with fresh inputs:

- Completed preflight rows remove `running` from an owned attempt's header.
- A journal-only age claims absent process output without observing that output.

A real verifier-script regression also captures the journal from inside an
ordinary stdout/stderr gate: the baseline has no release-gate running item.
The absence of a structured foreign producer exposes an unowned lifecycle
contract, not a test failure. A stale-generation explanation alone cannot
explain these fresh-input reproductions.

## Corrected contract

| Evidence | Owner | Meaning |
|---|---|---|
| Attempt UUID and generation | Daemon registry | Which process attempt owns this story |
| Release-gate item | Verifier script | Command running or validated command completion |
| Structured items/cases | Gate instrumentation | Declared legs and test counts, when available |
| Output reference and file growth | Verifier capture and read-only observer | Ordinary stdout/stderr activity |
| Receipt and verdict | Existing exact-tree verification | Certification and disposition |

An owned attempt remains running regardless of how many checklist rows have
finished. The verifier explicitly starts and finishes its gate item, including
validated completion recovered during signal cleanup. An explicit parent stays
running while children finish. Missing certification is a separate receipt
failure; it does not overwrite a known successful command exit.

A bare running gate has unavailable detailed counts and no aggregate completion
fraction. Instrumented child counts retain their existing meaning. Unknown
dashboard test counts remain absent; the public dashboard JSON shape is unchanged.

## Output identity and observation

The daemon initializes the journal with the current attempt UUID. The verifier
registers the exact unique log path, device, inode, capture time and UUID before
execution. New UUID-bearing journals must match the current attempt. Legacy
journals remain readable but cannot establish current raw-output activity.

The observer belongs to the active slot and is discarded on transfer/release.
It checks regular-file identity and monotonically observed length. It never
reads log contents, scans for the newest log, writes heartbeats, or changes a
deadline. Replacement, truncation, binding loss and clock reversal after a valid
baseline leave observation unavailable for the remainder of that attempt.

Before the first valid sample, missing or uncertain evidence is unavailable and
retryable. An initial nonempty log uses its modification time, normalized to the
daemon clock's whole-second precision and checked against capture/current time.
This is an inference: no prior length sample exists. Subsequent growth records
observation time; a timestamp-only touch cannot renew activity. An empty log uses
capture start for an observation interval without asserting that bytes arrived.

Publication holds the registry through the generation-checked store upsert after
comparing the expected active snapshot. This follows the existing registry-then-
store lock order and prevents an old queued/running comment from overwriting a
same-generation replacement. Structured inactivity and raw-output silence are
reported separately. Neither establishes semantic progress or successful tests.

## Validation and operating limit

Focused tests cover renderer behavior, real green/red/default/configured gate
execution, signal cleanup, missing receipts, observer identity/clock failures,
same-generation retries, supersession, publication guards, and unchanged idle
timeout/withdrawal behavior. Tests use isolated repositories/stores and owned
processes, with deterministic times and barriers rather than long sleeps.

Final focused validation passed 200 tests:

| Direct selection | Passed |
|---|---:|
| Library gate service, verifier control, publisher units | 36 |
| Foreign/output rendering, queue, control, timeout, withdrawal, bundle and foreign checkout integrations | 136 |
| `merge_gate foreign_gate_` real-script regressions | 3 |
| `selective_gate` impact-manifest and selection contracts | 25 |

Targeted Clippy with `-D warnings`, formatting, shell syntax, and diff whitespace
checks passed. The actual-tree selector returned `ALL` because baseline
`183fcd0f26c34b8da6b0ad7e4b4489f7c85a30c9` has no coverage map; only the new and
directly impacted tests above ran. Earlier lifecycle checks also covered signal
interruption and missing-receipt handling.

The central verifier owns full-suite validation. Source tests do not certify the
installed release. Keep the operator's exact-log/process inspection bridge until
a containing release is installed and the foreign-gate regression passes there.

## Adopted submission-head reporting repair

The 2026-09-13T05:33:27 story comment separately adopts stale submission-head
reporting. `submit-leased` verified the pushed branch HEAD, but its receipt copied
lagging PR API metadata and the central comment called that value the origin
head. SH-708's live gate used the correct head; its submission comment named an
older one. No live gate was interrupted or changed during this investigation.

The real-helper reproduction returned successful receipts in six scenarios;
independent remote reads equalled committed HEAD in every case. Three stale-API
scenarios failed: fast-forward adoption, unchanged adoption, and creation. Three
current-metadata controls passed. Passing those exact typed receipts to the
production comment writer reproduced all three stale central comments. This
falsifies an unsuccessful-push explanation and locates the reporting defect at
the receipt mapping.

The helper now maps the independently verified branch commit to the existing
`head_oid` receipt field. PR metadata still supplies the PR identity, base and
adoption decision; it cannot replace the verified origin observation. No new
field, fallback, API wait or submission block is added. This follows the distinct
evidence supplied by [Git remote refs](https://git-scm.com/docs/git-ls-remote) and
[GitHub PR metadata](https://cli.github.com/manual/gh_pr_view).

The receipt remains a submission observation, not certification or proof of API
convergence. `verify-pr.sh` independently checks the API head, fetched PR ref,
and remote branch before choosing the merge tree. The source audit found no
other runtime consumer of the submission receipt's `head_oid`.

Adopted-scope validation passed all five verification-service units (including
the six real-helper receipt/comment scenarios), 18 submission queue tests, four
head-convergence cases, and the existing `test-submit-leased.sh` suite. Targeted
Clippy with warnings denied, formatting, shell syntax and diff checks passed.
The actual-tree selector again returned `ALL` for the missing baseline map;
the central verifier retains the full suite.

SH713-D10/D11 record explicit native continuation, the competing hypotheses,
independent challenge and this separately committed repair. The operational
workaround still retires only after a containing release is installed and its
stale-metadata regression is validated.

## Central-verifier return: library-test lint coverage

PR808's proposed merge failed Clippy on six output-observer fixture constructors
using `tempfile::tempdir()`. Earlier targeted Clippy invocations checked the
library without its test configuration, so their success did not cover these
unit tests. The same six errors reproduced locally before correction.

Those fixtures now use the repository's `storyhook_test_support::scratch_dir()`
constructor. The existing disallowed-method lint remains the regression guard;
test assertions and runtime behavior are unchanged. Setting `TMPDIR` at test
invocation is not a substitute for using the required constructor in source.

For focused library-unit linting, use
`cargo clippy --offline --lib --profile test -- -D warnings`. Cargo's
[test-profile check mode](https://doc.rust-lang.org/cargo/commands/cargo-check.html)
enables the test configuration while retaining library-only target selection.
The local failing run verified that this command reaches all six violations.
After correction it passes with warnings denied, and all seven output-observer
unit regressions pass. Formatting and diff checks pass; no full suite was run
in the repair worktree.
