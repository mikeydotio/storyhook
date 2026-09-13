# SH-702: cleanup hid a completed gate verdict

## Evidence and cause

PR 793 twice completed a failing gate on 2026-09-12, then received only an
infrastructure halt after surviving test processes prevented owner cleanup.
SH-695 added reaping, but could still refuse after observing the command exit.

Three mechanisms discarded evidence: the inner supervisor returned its own
refusal status; the speculative runner published completion only after
restoration; the outer JSON supervisor discarded buffered output on refusal.
The daemon represented test failure and infrastructure failure as alternatives,
so its halt comment explicitly denied that a RED had been observed.

Competing explanation: the gate never completed and the halt was correct.
Isolated production-flow regressions falsified it: commands exited 3 and 7,
printed a named failure, then injected OS census errors caused infrastructure-only
output. The same experiment with exit 0 lost execution success. These three
regressions failed before the fix and passed afterward. Real signal termination
remains an infrastructure result, which distinguishes interrupted execution.

## Repair and class coverage

Persist attempt-bound command completion before cleanup and carry cleanup as a
second result fact. Retain owner and checkout invariants. Record verdict and halt
atomically; a completed command alone never becomes a merged PR or Done story.

A read-only challenger confirmed the cause and found neighboring loss paths:
final owner publication, restoration signals, the verifier signal trap and
restoration-marker unlink. Each is covered. Tests additionally exercise launch
failure versus exit 125, malformed or foreign evidence, retained live ownership,
buffered merged output, atomic rollback, duplicate recording and stale generation
rejection. The execution artifact lives outside the existing log/compiler pairs.

The challenger also found daemon cancellation and timeout discarded buffered
stdout after terminating the wrapper. Two regressions reproduced infrastructure
instead of completed RED and a late stop instead of verdict-plus-halt. Progress
capture now retains bounded output alongside its error after reaping. Validated
completion with cleanup metadata survives manual cancellation; withdrawal and
generation checks still reject obsolete attempts. Pre-completion interruption
remains unjudged. A real signal-handler regression covers cancellation output.
Bare final answers followed by a wrapper timeout also retain both facts: the
daemon synthesizes a cleanup halt and explicitly marks unreported resource paths
unknown. Partial answers do not establish a verdict.

## Continuation integration and validation

The impacted run also reproduced two defects at unchanged base `db827a022`:
`dashboard_local_time` lacked its test-impact manifest row, and a leased-reap
fixture had no origin although default-branch lookup now requires one. SH-698
landed both repairs as `c38d8e0da` and `b58639762`, plus the process-registration
exit-observation repair `574286228`. Integration commit `c38782322` merges their
landed history at `d3a01a0e2`, preserving primary fix `5c66ca540` and both stories'
lifecycle contracts. The adopted baseline work is resolved.

Continuation validation exposed three journal-damage assertions that still
required discarding a completed merge. Commit `612f52ffa` reconciles them with
the approved contract: missing, replaced and truncated journals each retain a
previously published verdict plus permanent cleanup failure, while the same
damage without completion remains infrastructure-only. All eight progress
timeout tests pass, including both states for each damage case.

The cancellation registration fixture also failed repeatedly: it published
readiness before starting a foreground worker, whose wait could defer the
parent's TERM trap. A controlled TERM-ignoring worker reproduced the missing
handler marker. Commit `b98b59506` uses an interruptible asynchronous wait and
publishes readiness only after the worker installs its handler. Both cooperative
and ignoring-parent cases retain cancellation and leader-reaping assertions.
The foreground version failed; the repaired process filter passes all ten tests
after integration. Production cancellation policy is unchanged.

The sibling sweep fixed the same cooperative foreground-wait pattern in the
graceful-timeout, withdrawal and manual-stop fixtures (`c5e9ef93e`). All their
assertions remain, and the withdrawal/control targets pass 6 and 11 tests.
Fixture constraint: a cooperative shell must use an interruptible `wait` when
its trap is expected to finish while a child is still alive. Deliberately
TERM-ignoring workers remain unchanged because they test escalation.

Across thirteen directly impacted integration targets, 232 Rust tests pass;
three focused library filters pass another 29. The two lifecycle wrappers run
the production-flow Python suites. Targeted Clippy with warnings denied,
formatting and whitespace checks pass. The selector ran against each changed
tree and returned `ALL` because certified baseline `7d8c74f5` has no coverage
map; the full suite remains the central verifier's responsibility.

The initial sandboxed compile was interrupted after shared build-slot access
failed. Approved escalation restored access; subsequent compiler waits confirmed
the normal shared bound. Cargo replayed cached fallback diagnostics from the
interrupted run. No lock relocation, guard bypass or installed-artifact edit was
used. Detailed commands, logs and regression evidence are recorded on SH-702.

## Central verification return: process inventory

PR 803's merge-tree gate failed the process-spawn inventory: the new Bash
cancellation probe in `src/process/activity_tests.rs` had no classification.
The exact inventory test reproduced that sole missing entry locally. Classifying
it as `Kind::Waited` reflects its regular-file capture and bounded process-group
termination/reaping; the census and production process handling are unchanged.
The inventory is also the regression and sibling sweep: both inventory tests
and both activity tests pass after the repair. Scoped Clippy with warnings denied,
formatting and whitespace checks pass. Out-of-line process tests participate in
the source census, so adding a probe requires a classification and an inventory
test run. The selector still reports `ALL` without a baseline coverage map;
the central verifier retains ownership of the full suite.
