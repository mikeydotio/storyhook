# Project verification views — SH-748

Each registered project owns one `verification` window in the tmux session
named by its project slug, on the default server. The daemon uses the registered
checkout as authority, never a test fixture, speculative worktree, caller pane,
or Git-directory-derived display name. This supersedes SH-662’s hashed windows
and SH-590’s automatically opened store activity window.

## Ownership and recovery

The daemon reconciles activated project journals at startup, before verifier
subprocesses, and every five seconds independently of running gates. Journal
existence in a registered checkout preserves activation across restart. A missing
session is created detached. A healthy window is reused without resetting its
reader or stealing focus. The view reads the continuous project journal; phase
banners are records and do not replace readers.

A bounded Python helper takes a nonblocking per-directory flock. It identifies
owned windows with a canonical journal hash, pane ID/PID, and original reader
command. An unrelated occupant or multiple panes named `verification` is a
visible ownership conflict. Missing windows are created; dead or changed owned
readers are replaced. A replacement is allocated and marked before retirement
of the old exact window ID, whose evidence is checked again before removal.
Failures are logged and retried on a later reconciliation tick. A successful
reconcile is not journaled at all (SH-761): the helper runs under the
project's own journal scope every five seconds, and announcing each child's
start and exit at INFO filled the window it exists to keep alive with two
lines about itself every tick. `run_captured_quiet` records a non-zero exit
or a timeout as one ERROR and mirrors no output; the daemon's WARN still
carries the helper's stderr.

SH-737’s native macOS `forkpty` failure regression remains required. Never use
`respawn-pane`: a failed respawn can corrupt tmux 3.7c and crash unrelated
sessions. Literal argv, exact targets, stable reader cwd, and detached window
creation remain required. A missing tmux or Python interpreter is nonfatal to
the verifier. `STORYHOOK_VERIFIER_MIRROR=0` prohibits even a tmux probe.
The reconciler may start the default server, so every client it runs passes
only the server-start allowlist (SH-758, `docs/spec/provider-pane-routing.md`).

## Fixture lifetime and legacy cleanup

Gate scripts only print diagnostics; they cannot allocate readers. Test stores
keep their own activity journals and disable mirrors through `Environment` and
the shared containment table. Their stdout/stderr reaches the project journal
through the parent gate capture. Explicit mirror tests own foreground private
servers, bound all control operations, and check reader termination on teardown,
including assertion failure. Production reader ownership never belongs to a
fixture or a story’s dispatch-window cleanup.

`scripts/cleanup-verifier-fixtures.py` inventories legacy default-server windows.
`--apply` removes only single-pane fixture windows whose names and complete
reader commands prove known fixture origins. It rechecks exact evidence before
removal and confirms disappearance. Production readers, unknown occupants,
logs, and the shared server survive. Existing installed binaries are not updated
or restarted by this cleanup; they can retain old routing until normal rollout.
