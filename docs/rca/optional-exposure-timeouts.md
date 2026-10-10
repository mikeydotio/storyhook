# Optional exposure timeouts and campaign readiness

An owned gate previously stopped when a descriptive CPU/RSS helper exceeded its
30-second allowance. That failure did not prove the gate itself was unhealthy.
The retained evidence cannot establish whether command execution, launch, fsync,
or native observation caused the delay. The boundary diagnostics remain useful;
this change does not retrospectively diagnose the nine historical attempts.

## Narrow exception

Throughput policy version 3 names exactly two optional commands: the `ps -axo`
CPU/process and CPU/RSS process listings. Admission still runs all observations
strictly. After the gate root starts, one owned worker process handles these two
commands, with one request outstanding at a time. It is bound to the collector's
native PID/start/boot identity, direct parent relationship, verifier session,
nonce, workspace, and immutable manifest digest. The worker validates the clean
pinned workspace before readiness, then rechecks its parent capability around
requests and while idle. Its control socket does not reach observed commands.
Normal verifier authorization remains unchanged.

A typed local deadline is eligible only when the supervisor has finished native
cleanup, has no cancellation or observation failure, and has not observed another
failure. A known nonzero/malformed command result, nonzero helper exit, or signal
not delivered by this supervisor remains fatal. The failed helper record stays
unfinished. Native session emptiness and an exclusively lockable exact lifetime
guard are both proved and rechecked; a surviving participant, escaped guard,
changed record/inode, or unknown observation is fatal. The shorter overall
observation deadline always wins, including during cleanup proof.

The first eligible timeout ends optional sampling for that attempt and records a
permanent completeness gap. The healthy root may continue while its mandatory
checks and progress polling continue independently. The worker is settled before
returning terminal evidence; late replies cannot erase a gap. Root exit zero plus
proved settlement is still an **unaccepted measurement** when telemetry is
incomplete. Version 2 cohort records require explicit completeness on finish and
replay, preventing reuse, retry, or progression after a gap. Old evidence is not
upgraded or adopted. This is never a production gate receipt.

Memory-pressure queries, native snapshots, storage admission, identity,
cancellation, custody and resource controls retain strict behavior. Native CPU
or swap errors are not optional. The host-admission runnable-process sensor is
unchanged. The collector does not presume a host policy is active. Five-second
scheduling is not a proved freshness bound for mandatory sensors, and these CPU
and RSS observations are descriptive, not a calibrated resource cap.

Worker requests, packets, sample files and retained helper output have explicit
bounds. Reports include observed resource samples and terminal completeness;
missing exposure is not inferred. Raw samples and failed custody remain local.
The two external-fixture/input-deadline preparation corrections from the earlier
local adapter are included, without its initial-three policy or start authority.

## Validation and limits

Focused Python tests cover the worker's actual launcher and validator against a
private pinned Git workspace; single-flight requests and late terminal replies;
mandatory-check cancellation; incomplete root success and replay rejection;
record/session/guard changes; and actual managed synthetic commands stalled at
descriptor capture, spawn, wait and result fsync. Injected clocks avoid waiting
30 seconds per fixture. The synthetic executable replaces `ps`, so these stall
tests do not measure host performance. Other regressions exercise known nonzero
results and unprompted signals coinciding with the supervisor's deadline check.
A tiny owned fixture leg verifies cold/warm/reuse integration and preserves
production receipts. The Rust wrapper registers the focused tests for future
normal gates; no Rust build or full gate was performed for this draft.

Independent review found and corrected three defects before publication: sample
records lacked the reader's version, input captures inherited the broader window,
and a coincident deadline could mask an already-failed helper. The regression
suite now exercises each failure through the corresponding boundary.

This source composes development commit `73257ca56328679f34ad554e2d9c204a9639e1ed`
with PR993 and PR997. It remains a draft and is not installed or enabled. The
[bounded preparation plan](../plans/next-performance-campaign.md) records the
remaining launch prerequisites and proposed cost limits.
