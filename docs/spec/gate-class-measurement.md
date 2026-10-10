# Verifier scheduling measurement

SH-801, StoryHook v3.0.3. This operation collects evidence. It does not certify
a tree, merge a branch, or change the ordinary verifier scheduling policy.

Run `story verifier measure-gate-class <checkout> <commit> --output <directory>`
with a clean committed source and a new output directory. The local CLI uses
its embedded verifier bundle. It needs no store or installed daemon change.
macOS is the supported collection platform. Linux and Xcode inheritance remain
unmeasured. The caller must have a normal scheduling class, with no background
flag or positive nice value.

A dispatcher can itself inherit utility QoS. Do not relabel that as the control
or change only its main thread: its spawned children can still inherit utility.
For the SH-801 collection, a uniquely named transient launchd job with explicit
Interactive process type supplies normal ancestry. A disposable reporter proved
QoS 21, nice 0 and no Darwin background flag; an unspecified launchd type still
gave QoS 17. Use RunAtLoad with KeepAlive false, retain the temporary definition
and logs, and remove only this job after collection. The command's own class
checks remain mandatory. No persistent LaunchAgent or live daemon policy changes.

Record the normal caller's soft and hard resource limits before a transient launch.
Match them explicitly in that user job and verify the actual child limits before
collection. In particular, the SH-801 shell allowed 1,048,576 open descriptors,
while the initial launchd job inherited 256 and its warmup encountered `EMFILE`.
That failed warmup remains separate evidence. Do not change global launchd limits.
The manifest pins every available `resource.getrlimit` pair. Collection and each
gate refuse missing or changed limits; each gate retains the observed values with
its scheduling class. The collector observes policy and never raises limits itself.

Preserve the reference locale, including `LC_ALL`, in the launcher. Forcing `C`
changed tmux protocol output and caused two cleanup cases to fail in the first
warmup; the same executable passed both with `C.UTF-8`. Locale is part of the
pinned environment identity and each gate's observed scheduling record. This
operation must not add locale differences to its scheduling comparison.

## Ownership and authority

The operation pins a commit, tree, configured gate argv, toolchain, binary and
fixture. It acquires the same project gate lock as ordinary verification, then
the existing verifier workspace owner. The measurement checkout lives at
`<output>/worktree`. It is separate from the implementation checkout and the
ordinary verifier workspace. The output bundle never sweeps daemon bundles.

The measurement manifest, workspace mapping, live session, boot identity and
owner nonce must all agree. A flag alone grants no authority. A measured leg
does not reuse a verdict or publish a receipt. Compiled artifacts remain warm.
Fixture isolation clears the measurement context. Normal gates and receipts
retain their existing behavior. The ordinary supervisor still applies utility
QoS; only an owned measurement gate can select the unclamped control condition.

Cancellation uses the existing gate-session cleanup and durable owner record.
The output and checkout remain for diagnosis or restart. No receipt deletion,
workspace theft, automatic orphan adoption, or daemon replacement is permitted.

## Protocol

1. Warm the build with a separate full gate. Keep its evidence out of statistics.
2. Before each run require load average per logical CPU below 0.5 for a continuous
   60 seconds. Retain load, CPU count, memory pressure and process observations.
3. Run ten alternating control/utility pairs against the same tree on one local
   calendar day. Time the gate with `/usr/bin/time -p`. Verify its actual class.
4. Keep the collector and interactive probes outside the gate clamp. During
   active test execution, time one real `story list --json` and one real
   SessionStart hook. Require test execution before and after each probe.
5. Use a leased CLI binary and one isolated, warmed daemon with ten fixed stories.
   Remove dispatch and measurement identity from probe environments. Validate
   the full list and meaningful hook context; `{}` is a degraded response.
6. Preserve every attempt, exit, cleanup result, failed probe and interruption.
   No retry replaces a failure. Stop an invalid cohort with its evidence intact.

The project gate lock is not a host-wide lock. Admission proves the stated idle
criterion only. Per-run pressure/process observations support interpretation of
external activity. Do not describe this as proof that no outside work ran.

## Evidence and restart

The immutable manifest identifies the experiment. Each date has a cohort record,
append-only sample journal, warmup, per-run command/supervisor/probe logs,
scheduling observations, pressure log, and derived JSON/Markdown report.
The lock watchdog receives actual gate events and measured idle observations;
arbitrary stdout cannot keep it alive.

A restart requires the same identity and a valid completed prefix. Missing exits,
partial JSON lines, changed identities and interrupted attempts fail loudly.
An output directory cannot automatically start a new date or campaign.
Never pool dates or count a warmup as a sample. Retain incomplete cohorts; they do not satisfy the story.

Report attempts, valid samples and failures with denominators. For each condition,
report gate median/min/max and list/hook medians. Report the gate median percentage
change. Historical gate times from other trees are not controls. Do not infer a
causal red-rate change from unmatched logs.

## Acceptance

Regressions cover statistics, ordering, warmups, idle waiting, class selection,
owner validation, receipt suppression, ordinary receipt/reuse behavior, real
subprocess cleanup, meaningful probes, overlap, failures, corrupt evidence,
restarts and date boundaries. Only new and directly impacted tests run in the
implementation lane. The explicit measurement operation owns the full gates.

Completion requires ten valid samples per condition, their probe samples, a
committed compact evidence/report artifact, and results on SH-801 and SH-785.
Tooling alone is not completion. The central verifier owns submission and merge.

## Containment and current readiness

The bounded collector has a 22.25-hour campaign ceiling, 40-minute initial
preparation ceiling, 60-second project-lock wait, 300-second quiet admission
wait, 60-minute per-gate ceiling and 30-second helper/probe ceiling. The probe
fixture gets at most ten minutes of preparation. It samples host observations
every five seconds. The same boot, day and manifest are required for resumption.
Every started gate, including warmup, failed and interrupted gates, consumes
one of 21 slots. No failed slot can be replaced.

These are containment ceilings, not measured runtimes. Taking every individual
maximum sequentially (21 gates, 21 quiet waits and 40 minutes of preparation)
would total 23 hours 25 minutes before cleanup overhead, exceeding the campaign
ceiling. The collector must stop before admitting work that does not fit; the
22.25-hour reservation does not promise a complete cohort at those maxima.
Acceptance still requires all twenty valid same-day samples. A longer
reservation or another campaign needs an explicit coordinated decision.

Every admission-to-settlement duration at or above 900 seconds is reported as a
production target breach. Cancellation requests use existing exact-owner
settlement. Cleanup excess retains ownership and prevents another sample; a
containment deadline is never proof that children disappeared. Helper streams
and failed command custody records remain in the private output namespace.

Source readiness still requires the pressure/storage controls shared with
SH-872 and focused regression validation. Do not launch a full campaign from
this incremental port. The current manual campaign also requires a separate
quiet-host start signal. Disk is not reserved and existing caches are retained.
