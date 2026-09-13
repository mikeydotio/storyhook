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

## Remaining baseline repairs

The impacted run also reproduced two defects at unchanged base `db827a022`:
`dashboard_local_time` lacks its test-impact manifest row, and a leased-reap
fixture has no origin although default-branch lookup now requires one. Both are
adopted into SH-702 for continuation, with exact diagnostics on the story. They
must be repaired and tested before the widened story is submitted to verifying.
