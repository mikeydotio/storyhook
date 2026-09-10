# Safe daemon restart

`story daemon restart` replaces the daemon that owns one canonical store
without abandoning accepted commands or letting two daemon processes own the
store concurrently.

## Transition

1. The client observes the live daemon's published identity and port.
2. It takes the store-scoped spawn lock and rechecks that identity.
3. If another waiter already installed a healthy daemon from this build, the
   caller adopts it. A different or unhealthy replacement is refused.
4. The incumbent accepts shutdown, refuses work dequeued afterward, and drains
   its registered in-flight operations before releasing the pidfile lock.
5. The client starts the current binary on the incumbent's loopback port and
   keeps the spawn lock until the successor answers its authenticated health
   check.

The pidfile's lifetime lock remains the ownership fact. Restart never starts
the successor before the predecessor exits, never force-stops, and never turns
an absent daemon into a fresh start. A concurrent explicit `daemon start`
waits on the same bounded spawn lock and adopts the successor. If a graceful
drain outlasts that contention budget, the start request times out truthfully;
it neither reports the draining predecessor nor interrupts restart.

## Durable handover

Story, engine, verification, and named-token state already lives outside the
daemon process. Before the successor publishes its portfile it synchronously
runs Full Auto restart reconciliation. Occupied lanes follow D11: completed or
verifying stories keep their durable outcome; otherwise interrupted lanes are
quarantined with their worktree, branch, pull request, and tmux evidence left
intact. The steady engine poller cannot claim more work until server readiness,
and stops beginning passes when shutdown starts draining.

The per-daemon master bearer token rotates. Persistent named dashboard tokens
remain valid because their registry is durable daemon state.

## Failure and version policy

A parseable older portfile is enough to authenticate graceful shutdown; the
successor always runs the current client binary and wire version. That is also
why an *uninstalled* client — one still inside the directory cargo wrote it to —
is refused a restart of the default store's daemon before anything is stopped:
the successor would be that build (`src/daemon/seat_guard.rs`, SH-634). A live
daemon with an unreadable portfile is refused because only a force-stop could
replace it, and force can lose work.

If the old daemon drains but the successor cannot open the store, reconcile,
bind, or become healthy, restart returns the successor's startup diagnostic.
The store remains durable, no healthy portfile is fabricated, and a later
explicit `story daemon start` may recover it.
