# SH-826: three contract failures in PR 882

The central gate rejected tree `3ce7f94859bce25627582d2499d38a5b584e7481`
on 2026-09-28. The failures were independent of the dashboard repair.

| Boundary | Cause | Repair and regression |
| --- | --- | --- |
| Native interruption fixture | `exists()` accepted a PID file before `write_text()` wrote its contents. The gate read an empty string. Other PID files used the same unsafe assumption. | Writers terminate PID records with a newline. Readers wait for a complete, positive ASCII PID. Tests cover absent, empty, partial, complete, and malformed records. The writer installs its signal handler before publication. |
| Speculative signal cleanup | The direct `merge-watch.sh` entry used the lock wrapper's generic two-second termination grace, outside a verifier cleanup ladder configured for 30 seconds. Under load, the wrapper killed restoration and left the speculative HEAD. | Match `verify-pr.sh`: validate and normalize the configured budget, then give the outer wrapper three quarters of it. The existing real-Git signal test now takes four seconds to clean up and still must restore HEAD, remove private objects, and re-raise HUP/TERM. Invalid budgets fail before mutation; a leading-zero decimal budget works. |
| Scheduling-class fixture | Synchronous commands used `subprocess.run(timeout=...)` with contention measured during fixture setup. Load rose from 2.65 to roughly 12 threads per core; the command was killed at its stale 251.8-second deadline. | Resume communication with the same process when the existing `Patience` policy extends the deadline. Do not restart execution. Keep one start time, the 15-minute policy ceiling, and bounded failure at stable load. Deterministic tests prove extension and kill/reap on true expiry. |

The slow-cleanup test reproduced the central HEAD mismatch before the script
repair. Its log showed the outer wrapper sending SIGKILL after two seconds.
The PID and command-observation tests also failed before their repairs.

Sibling review: all three PID publications in the interruption fixture use
the complete-record reader. The lifecycle fixture's existing asynchronous
waits already use adaptive patience; the synchronous command door was the
gap. `verify-pr.sh` already passes a coordinated termination grace; the direct
speculative entry was the missing owner.
