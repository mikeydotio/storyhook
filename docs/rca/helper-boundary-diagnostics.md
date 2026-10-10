# Measurement helper timeout boundaries

A bounded process-observation helper failed during an owned gate. Its retained
command-start marker did not show whether the marker's fsync returned, descriptor
capture completed, Popen returned from its exec handshake, or the command exited.
The older collector helper had no such stage markers at all. No retained evidence
proves that process polling contention, a command, spawn, or fsync caused the
failure. This repair addresses the diagnostic gap, not a proven runtime cause.

The collector now retains `boundaries.json` in each helper's existing private
custody directory. It records begin/end/error boundaries for custody file and
directory fsync, descriptor capture, launcher spawn and handshakes, command spawn
and wait, result flush/fsync, waitid, native session census and liveness checks,
publisher failure, the first observed deadline failure, cancellation/signalling,
reap, settlement and finish. Command spawn completion includes its PID; command
wait completion includes its exit status. Popen returning is an exec-handshake
boundary, not proof of how much command CPU time ran. The existing command wall
interval still includes descriptor capture, spawn and wait.

Parent events stay in bounded memory: first 32, last 256, plus the latest record
for each source/phase. A nonblocking local datagram channel carries only child
boundary events. At most 32 packets of 256 bytes are read per existing supervisor
iteration. A full or broken channel drops events; sequence gaps and possible
unobserved tails remain explicit. There is no per-participant logging, extra
process census, thread, subprocess, sleep or polling-path disk write/fsync. The
shared supervisor defaults to no trace; only measurement helpers enable it.

The file is written once after supervision and cleanup (or a failed initial
custody write), without fsync. It is best-effort evidence: supervisor death, a
blocked final write, a dropped packet or an evicted event can leave an unknown
boundary. Arrival order across parent/child is not chronological; use monotonic
timestamps and source labels. A begin without end narrows an observed interval
only when delivery/retention is adequate; it cannot prove a kernel-level cause.
No argv, paths, environment, command streams, exception strings, custody tokens or
host process lists enter these diagnostics. Only fixed labels, monotonic times,
counts, sequence numbers and optional numeric PID/exit status are retained.

Custody remains authoritative. Existing durable record fsyncs, observation
cadence, the 30-second helper default, admission and TERM/KILL allowances,
non-reaping leader observation, descendant checks, inherited locks, settlement
refusals and failed-record retention are unchanged. The trace descriptor crosses
the blocked launcher explicitly and becomes non-inheritable before the observed
command's descriptors are captured. The command retains its custody descriptors.
The split Popen/wait preserves subprocess.run's direct-child kill/reap on a wait
exception; the outer supervisor continues to own descendant cleanup. Diagnostic
channel setup/close failures cannot replace a command result.

Twenty-one new focused Python regressions cover injected descriptor/spawn/wait/
fsync stalls; bounded/lost/malformed telemetry; privacy; setup/close failures;
unchanged deadline and TERM/KILL failure handling; surviving descendants and a
held guard; and a real synthetic Python child through the blocked launcher.
Native identity/census are mocked in that last fixture: it verifies descriptor
transport, not real host custody or performance. The Rust contract wrapper
registers the suite for future normal gates; no Rust build or gate was run for
this source-only repair.

Future normal operation, after applicable review/approval, could distinguish a
helper that had not returned from Popen, a running command still in wait, delayed
result durability, slow native census/liveness, and delayed cancellation. It
cannot retrospectively identify the cause of the frozen nine attempts or justify
a timeout/policy change. Their evidence remains untouched.

This draft stacks on PR993 (`shepherd/representative-load`, 830906022cb07625409107f851a0f2611ae83b08),
because the collector is not on dev. It does not promote that stack or include
the later native custody corrections already merged in PR995. The stack must be
composed with current dev and validated before any merge/install; this draft is
not a deployable release and supplies no gate receipt or performance result.
