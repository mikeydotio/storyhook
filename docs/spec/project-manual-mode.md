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

## Implemented behavior

`automations.enabled` is stored per project; unset means `true`. The dashboard
and CLI use the same control operation. A disable closes admission, cancels only
that project's verifier processes, and waits for admitted daemon work before
returning. It supersedes pending continuation/delivery intent and pauses Full
Auto runs without terminating their provider sessions. Previously installed
StoryHook Git hooks are refreshed in place; other hooks are untouched.

A generation watermark excludes old verifier submissions and cleanup work after
re-enabling. Pending reset retries and repair publications retain their original
boundary. Re-submit a story to verification and explicitly resume a paused Full
Auto run when automation should work on it again. Existing verifier stop/incident
controls remain in force when the project is re-enabled.
Re-enabling retires stale intent and changes the durable switch in one
transaction, so interruption or retirement failure leaves automations disabled.

Manual state changes, including completion from verifying, bulk edits, blocked
creation and epic states do not require automated workflow approval or generate
new delivery/cleanup intent. Input validation, graph integrity, resource ownership
and authentication remain enforced. Explicit user-requested reset/cleanup is
still available; resource deletion continues to require ownership evidence.

Daemon audit covers verifier startup/ticks/progress, engine startup/steady
reconciliation, GitHub polling, continuation intake/recovery/delivery, block
delivery, cleanup and closure retries, repair publication, project recovery,
reset adoption/retry, and project journal hygiene. Global database backup,
HTTP serving, authentication and dashboard refresh remain operational.
Retained-dispatch restoration is also project-gated. After re-enabling, an old
dispatch needs a fresh story state generation; comments do not reactivate it.
