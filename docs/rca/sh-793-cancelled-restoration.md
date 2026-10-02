# Cancellation completion does not guarantee restoration completion

SH-793's first parallel lifecycle run failed the resistant-subgroup case:
gate ownership was clear and every recorded session was quiet, but HEAD still
named the speculative commit. SH-862 and later verifier returns recorded the
same state with the serial runner. Process concurrency was not established as
the cause.

Five isolated traced repetitions passed. A controlled reproduction then paused
only the real restoration worker, in a private script copy, after gate
quiescence. The outer lifecycle owner reached its half-budget deadline and
killed the worker. The old HEAD assertion failed with the same state. This
rules out an observation race for the reproduction. Attribution of the original
load-dependent sighting remains an inference: its temporary repository had
already been removed.

The production contract is bounded cancellation followed by journaled recovery
on admission. SIGKILL cannot run a cleanup trap. The fixture incorrectly required
restoration to finish even when the owner enforced that deadline. Increasing a
fixed allowance only moves that race; removing the deadline changes production
policy and can leave a hung cancellation.

The repair leaves production deadlines, process authority and recovery intact.
It adds the missing escalation diagnostic, with field and session identity.
The fixture still proves gate ownership clear and all sessions quiet. Only a
matching lifecycle escalation permits it to run production admission before
checking the pinned base, clean state or retained gate damage. Ordinary
cancellation and gate-only escalation still require immediate restoration.

The new regressions stall a real restoration worker until the real supervisor
kills it. Before readmission they prove the worker is gone, the pinned base and
private lease remain journaled, and the speculative HEAD is still present.
Readmission must restore a clean lease or retain the damaged checkout and its
objects. A separate harness test rejects recovery permission from another
session, gate-only escalation, or no escalation, and rejects failed admission.
The clean-lease regression was RED at the original HEAD assertion before the
repair. Cooperative, resistant, subgroup and delayed-gate cases retain their
existing process and filesystem checks.
