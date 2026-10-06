# Per-project manual mode

The dashboard exposes a persistent checkbox labelled `enable automations`.
Existing and new projects default to enabled. Disabling it makes StoryHook a
manual board: user actions retain input validation, data integrity and access
control, while automated workflow policy, receipts and status changes stop.

## Execution boundary

Every daemon project worker, startup recovery, hook and asynchronous callback
must respect the setting. Disabling cancels only verifier-owned processes and
drains already admitted automatic operations before acknowledgement. It must
not terminate provider sessions. Persistent queue authority is invalidated so
re-enabling cannot replay stale deliveries or land stale submissions. Explicit
manual operations remain available.

## Validation and rollout

Cover default/persistence, project isolation, manual state and bulk operations,
enabled regressions, cancellation and repeated toggles, startup, and dashboard
error handling. Run targeted tests serially, then the applicable aggregate gate.
Before installation, preserve the installed binary and a consistent database
backup, inspect startup effects, and ensure the StoryHook project is disabled
before the replacement daemon can schedule it. Preserve other projects and
provider sessions. Verify the installed build, plugin, dashboard and persisted
setting after restart.
