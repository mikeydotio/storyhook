# Project verification views — SH-748

Each registered project owns one `verification` window in the tmux session
named by its project slug, on the default server. The daemon uses the registered
checkout as authority, never a test fixture, speculative worktree, caller pane,
or Git-directory-derived display name. This supersedes SH-662’s hashed windows
and SH-590’s automatically opened store activity window.

## Ownership and recovery

The daemon reconciles activated project journals at startup and every five
seconds independently of running gates. Verifier phases only enqueue a project
ID and wake the view poller; they never wait for Python or tmux (SH-808). Journal
existence in a registered checkout preserves activation across restart. A missing
session is created detached. A healthy window is reused without resetting its
reader or stealing focus. The view reads the continuous project journal; phase
banners are records and do not replace readers.

Requests coalesce by project and resolve against the current catalog, including
when a phase first activates a journal. Deleted projects and invalid checkout
paths cannot authorize a view. The one background worker drains requests before
running them, keeps requests received during a pass, and services periodic work
even during request traffic. A project waits five seconds after a completed
attempt before its next attempt. Catalog failures also wait five seconds.

The daemon prepares the journal directory, ignore file first, before it runs
the helper. The helper never creates the directory. If the directory is absent,
it fails loudly before any tmux call (SH-771, `activity-log.md`).

A bounded Python helper takes a nonblocking per-directory flock. Each owned
window carries the canonical journal hash; the reader additionally records
its exact pane ID/PID and command. An unrelated occupant or a second window
named `verification` or `verifier` is a visible ownership conflict. Missing
windows are created detached. A dead, single proven reader is replaced by a
new owned window before retiring the old exact window ID, whose evidence is
checked again before removal.

A person may add panes. A healthy mixed window is left alone. If its reader
dies or is repurposed, the helper checks the complete window inventory, renames
it `verification-retained-<uuid>`, and releases reader ownership. It never kills
any of that window's panes, including dead user panes. The next pass allocates
a fresh single-pane `verification` window. A disabled legacy agent is retained
under its existing owner in the renamed window until it can migrate safely;
its marker prevents duplicate launches. After migration, the retained user
window's ownership is released. Retained windows are never temporary cleanup
candidates.
Creation and the first ownership tag run in one tmux command group. Staging
names use `verification-pending-<uuid>`: a period is a pane delimiter even in
an exact window target. Cleanup also accepts the old `.verification-` prefix,
but only with matching ownership and unchanged inventory evidence. Inventory
parsing preserves empty fields, including those on the final row.
Failures are logged and retried on a later reconciliation tick. A successful
reconcile is not journaled at all (SH-761): the helper runs under the
project's own journal scope every five seconds, and announcing each child's
start and exit at INFO filled the window it exists to keep alive with two
lines about itself every tick. `run_captured_quiet_cancellable` records a non-zero exit
or a timeout as one ERROR and mirrors no output; the daemon's WARN still
carries the helper's stderr.

The embedded `probe_budget.py` gives the complete operation 30 seconds; every
tmux client, including cleanup, receives only what remains. The Rust
`VIEW_RECONCILE_TIMEOUT` is 45 seconds, leaving one third for interpreter startup
and exit. A test pins that relationship. Load does not multiply production
deadlines; timeout diagnostics include the observed load. Python process startup
can itself outlast a subprocess timeout, so the outer process-group bound remains
necessary. Neither bound promises progress under arbitrary scheduler starvation.

Shutdown checks the daemon stop flag in both the idle wait and the process wait
at most 100 milliseconds apart. It kills the owned helper process group and reaps
its leader instead of waiting for the outer deadline; expected cancellation emits
no failure warning. A cleanup with no budget starts no client, preserves the
original diagnostic, and leaves the tagged allocation for a later pass to
recover. None of these failures blocks verification work.

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

## The Verifier Agent window — SH-822, SH-861

The `verifier` window runs the Verifier Agent (`plugins/story/agents/verifier.md`,
`storyhook::plugin::VERIFIER_AGENT`) in the registered checkout. The separate
`verification` window follows the journal. The daemon resolves the launch
(`src/daemon/activity/verifier_agent.rs`): `claude` on its own PATH, kept as
spelled so an updater's versioned symlink keeps working, and the first plugin
root that carries `agents/verifier.md` — the copy `STORYHOOK_DISPATCH_SCRIPT`
names, the binary's release projection, a checkout, then Claude Code's registry.
It reads files only; no provider CLI runs on the reconcile tick. The argv is
`--plugin-dir <root> --agent story:verifier --model opus --effort xhigh`.
Without `claude` or a root, only the reader is created and the project journal
records why once, on the edge.

The agent pane runs `/bin/sh -c <loop> storyhook-verifier:<owner> <argv>`.
The `$0` marker identifies it from creation; duplicate agents are a visible
conflict, never grounds to kill a live process. When `claude` exits, the loop
prints its status and waits for Enter, so a person restarts it and a failing
launch cannot loop. A missing agent window is created at most once per 60
seconds. The owner-scoped `@storyhook-agent-started-<owner>` session option is
stamped before allocation, surviving either window's closure/replacement.
Legacy window timestamps migrate into that session option. Launches use the
shared pane overrides (`tmux_server_env.pane_overrides`) after
`scrub_owned_session` removes retained provider state.

A legacy shared window is migrated by moving the exact agent pane with
`break-pane -d`, keeping its pane ID and PID. If its reader was already closed,
the single remaining agent window is renamed instead. No process is restarted
or killed to migrate. Creation and migration never take focus. Each pass makes
at most one structural change (allocation/replacement, migration, mixed-window
release, or retirement of one interrupted allocation), within the shared
30-second operation budget. Production code never subdivides a window.

`STORYHOOK_VERIFIER_AGENT=0` creates no agent window and leaves an existing
agent alive, including a legacy shared pane. `STORYHOOK_VERIFIER_MIRROR=0`
makes no tmux call. The shared test environment disables the agent
(`Literal("0")`), and `tests/verifier_fixture_hygiene.rs` refuses a fixture that
turns it on without a fake `claude`: a real provider session is paid and
network-bound. Private-server tests cover layout, migration, ownership,
preservation and cooldown; a production-daemon test proves the launch end to
end. A source fence rejects window subdivision in production source.
