# Throughput measurement collector

SH-872's collector source supplies the C/W/R controller, input observer, owned
execution bridge, per-leg reuse path, bounded setup and disposable target pool.
Native fixtures exercise ownership, cancellation, probes, target turnover and
C/W/R leg execution. The retained integration and input-preflight receipts
determine readiness. No baseline/optimization measurement or production
performance claim is implied by source or native fixture tests.

## Entry and containment

The internal operator entry is `gate_measurement_campaign.py prepare` with six
arguments: an existing empty private output root, source checkout, exact source
revision, validated current StoryHook binary, `baseline` or `optimization`, and
an external coordinated-start receipt. There is no daemon/automation entry.
The receipt must carry version 1, kind `coordinated-measurement-start`, story
`SH-872`, the exact campaign root and revision, and the actual coordination
authority. A receipt records a real start decision; do not invent one to pass
the check. Source-preparation permission is not a measurement start decision.

Each window is durably consumed before preparation. The implementation uses
one continuous, same-boot 20-hour campaign deadline, including the gap used to
choose and implement the optimization, with two windows of at most 10 hours.
This is a conservative containment interpretation; it never grants extra time.
Each revision gets exactly three C/W/R blocks. Cold/warm ceilings are 75 minutes,
reuse is 10 minutes, initial preparation is 40 minutes, lock wait is 60 seconds,
and quiet admission requires 60 consecutive seconds below load/core 0.5 within
five minutes. Interrupted windows/slots cannot silently restart or be replaced.
The optimization window requires a complete accepted baseline and unchanged
controls apart from source and fresh targets. Every duration at or above 900
seconds remains a production-target breach.

Setup strips unrelated credentials and authority from the environment, uses
offline Cargo, and fixes Cargo/test/plugin/browser worker controls to one. It
uses the current complete `make test` gate and serial legs. It neither enables
host admission nor raises a resource limit. Limits must be reviewed with the
frozen manifest before an actual start.

## Inputs and evidence

Every boundary re-observes committed source, selected tools, dependency bytes,
active Cargo/Git configuration, inherited environment, actual resource/worker
limits and exact target identity. The inventory includes selected Rust/Apple
toolchains and SDK, selected Homebrew LLVM/configuration dependencies, Python
distribution and installed packages, Cargo registry/Git sources and
metadata, external Cargo packages and Node modules. Git config discovery asks
for names/origins, never values; raw environment values are hashed, not stored.
Files are hashed without an mtime digest cache and rechecked for concurrent
mutation. Optional missing files are part of the fingerprint. Symlinks may only
resolve inside declared dependencies. SDK aliases to an ancestor directory are
recorded as references while its complete contents and identity are audited;
they do not expand recursively. Unknown types, escaping or unresolvable links,
unsupported Git origin escaping, unavailable sensors and more than 250,000
inventory entries refuse execution. Do not weaken checks to obtain data.
Actual inventory cost and platform compatibility require the integration window.
Homebrew Python links add complete exact installed packages in the selected
interpreter's physical Cellar, including transitive package links. Discovery has
a 30-second/250,000-entry bound; links to another prefix, external configuration,
unversioned packages or missing targets refuse. Package selection is checked
again after hashing to catch a changed link or tool selection during capture.
The standard profile permits tracked source wrappers such as rustc-slot and
host-admit. Arbitrary external compiler/runner programs, compiler flags with
unreviewed dependencies, Cargo include/env injection and gate command aliases
are refused until their input closure is explicitly reviewed; they are never
silently dropped or treated as equivalent.

SH-801 uses the same capture and pins one identity across its entire matched
comparison. Each gate also requires complete applicable-leg progress and actual
exit/settlement. Its existing same-day, 21-gate and 22.25-hour ceilings remain;
individual maxima can exceed that reservation, so an incomplete cohort is a
possible honest outcome.

The execution bridge persists each slot before launch and compares identity
afterward. Missing progress, exit or cleanup leaves an incomplete attempt.
`OwnedGate` uses current verifier admission, session custody and the execution
result channel. Cancellation signals only its direct supervisor; an expired
cleanup observation retains ownership. Private probe shutdown has a separate
bounded cleanup allowance after admission expires.
Darwin session liveness uses its documented basic-status API so setuid `ps`
children remain observable without privileged inspection. Full authority identity
checks remain separate; denied, truncated or mismatched basic status fails closed.

Cold and warm legs execute regardless of ordinary receipts. R needs the
immediately preceding successful W and matching per-leg command/environment
records. Attempt telemetry and the verified empty build-feedback output channel
are excluded from detector-input comparison; unknown environment changes refuse
reuse. Measurement records cannot publish ordinary gate/tree certification.
The internal leg decision uses an exit status to preserve shell inputs such as
`SHLVL`; failures retain changed environment key names and hashes, never values.

## Targets, monitoring and reports

The pool creates at most two new targets beneath its exact private root. Each
cold slot has a never-reused name and directory identity. Removal requires a
settled live verifier owner, exclusive existing `ProductLease(reclaim=True)`, an
exact pending removal record, and a bounded child that inherits lifetime locks.
Symlinks, substituted/shared/unknown targets and unfinished custody are retained.
A partial deletion blocks further admission. Existing caches are never adopted,
enrolled or swept. All logs and custody journals remain.

Admission checks require 130 GiB initially free, two targets of at most 40 GiB
each, 10 GiB evidence and 40 GiB system headroom. Disk is not reserved. Health
checks also preserve free space for the active targets' remaining permitted
growth and the unused evidence allowance, rather than checking headroom alone.
Health
observations retain native memory pressure, load, process census, detected
competing builds/tests and observed aggregate descendant CPU/RSS. Five-second
sampled RSS can count shared pages more than once; it is not a calibrated host
memory cap. Sensor failure or contamination stops admission. Summary JSON keeps
all attempt denominators, distributions, breaches, pending slots, failures and
resource peaks; per-slot journals retain detailed cost boundaries.

## Required integration before measurement

Build the exact source and run the story-added native CLI/owner/receipt/probe
regressions in the reserved lane. Validate actual tool/config inventory, input
capture cost, source/binary correspondence, pressure/census sensors and cold
target peak storage. Exercise real cancellation, descriptor inheritance, R
coverage and target turnover using disposable fixtures. Freeze and review the
actual manifest, then obtain the separate coordinated start. Draft publication
does not authorize merging, measuring, activation or a production release.
