# Concurrent verifier views — SH-662

Attach with `tmux attach -t storyhook-verifier`. The default tmux server keeps
one stable session. Each checkout has a verification window; each store has
an activity window. The implementation decision and alternatives are recorded
on SH-662. This extends SH-545 and preserves SH-590's continuous journal.

## Ownership

- Project identity is the canonical Git common directory, hashed in full with
  `git hash-object --stdin`, like the gate lock. All linked worktrees share
  the window. Equal directory basenames in different repositories do not.
- Verification windows are named `verification-<directory-label>-<digest>`.
  The label is sanitized and the digest is complete. A window follows its
  current attempt log or shows a phase banner. Daemon and standalone
  invocations use the same identity.
- Activity windows are named `activity-<store-directory-label>-<sha256>`.
  Python 3 resolves file symlinks and hashes canonical path bytes in full,
  independently of Git or the daemon's starting directory. These windows run
  `story daemon logs --follow`; project phases cannot replace those readers.
  Banners also continue to enter the activity journal through stderr.
- Session creation tolerates a concurrent creator. Window creation/reuse is
  atomic within tmux. Exact targets prevent prefix matching another view.
- Existing legacy `verification` windows are left intact. New windows persist
  for later runs. No background cleanup or per-story window is introduced.

## Preserved boundaries

The daemon owns verification. Panes only read independently written logs;
they never pipe gate output or decide results. Paths and banner text remain
literal arguments. Panes start in stable HOME, never a temporary checkout.
The default server stays independent of story dispatch cleanup.

`STORYHOOK_VERIFIER_MIRROR=0` prohibits every tmux call. Journal banners still
emit when enabled by the activity environment. Missing Git identity or tmux
causes a helper failure that verification ignores; identity never falls back
to a shared project window. The activity mirror also needs Python 3; macOS's
system Python 3.9 is supported. Strict path resolution rejects missing stores,
dangling links, and link cycles before any tmux call. A missing interpreter
leaves the journal and verifier working without that view.

## Verification

Tests exercise distinct projects, linked worktrees, directory aliases, repeated
attempts, concurrent creation, daemon phases, separate stores, literal shell
metacharacters, legacy windows, exact targets, and non-fatal mirror failure.
macOS regressions select the system interpreter explicitly so a newer Python
on the interactive shell's PATH cannot hide a compatibility failure.
Real tmux runs use private test sockets. Only new and impacted tests run in
the agent worktree; the central verifier owns the full suite.

## Fixture containment — SH-699

Mirror policy belongs to `Environment`, alongside store and state-home
selection. `from_process` captures the existing switch: only the exact value
`0` disables mirrors. `Environment::at` always disables them, independent of
the invoking shell. `child_vars()` serializes that resolved policy as `0` or
`1`; verification children apply it after their allowlist, and activity
startup consults it before dispatching its helper. A disabled mirror does not
disable the journal.

Standalone verifier-script fixtures apply the table-derived
`daemon_containment()` settings at their command builder. Tests that explicitly
enable mirrors own a foreground `tmux -D` process through `ChildGuard` and route
every client through `-S <private socket>`. Startup and control clients have
bounded waits. Teardown closes the private server, reaps the owned process,
and checks pane-reader identities before deleting fixture directories, even
during assertion unwinding. A second private server proves cleanup does not
affect another owner.

The verifier fixture hygiene test scans direct script launches and opt-ins at
function/helper boundaries. This is a conservative textual fence, not arbitrary
Rust dataflow analysis; behavioral subprocess and real-tmux tests prove the
actual policy and ownership contracts. Production windows, including the
legacy `verification` window, retain their persistent lifetime. Fixture cleanup
never targets the operator's default server or historical processes.
