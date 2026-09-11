# Concurrent verifier views — SH-662

Attach with `tmux attach -t storyhook-verifier`. The default tmux server keeps
one stable session. Each checkout has a verification window; each store has
an activity window. The implementation decision and alternatives are recorded
on SH-662. This extends SH-545 and preserves SH-590's continuous journal.

## Ownership

- Project identity is the canonical Git common directory, hashed in full with
  `git hash-object --stdin`, like the gate lock. All linked worktrees share
  the window. Equal directory basenames in different repositories do not.
- Verification window names contain a sanitized directory label and the full
  identity digest. A project window follows its current attempt log or shows
  a phase banner. Daemon and standalone invocations use the same identity.
- Activity windows use canonical store identity. They continuously follow
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
to a shared project window.

## Verification

Tests exercise distinct projects, linked worktrees, directory aliases, repeated
attempts, concurrent creation, daemon phases, separate stores, literal shell
metacharacters, legacy windows, exact targets, and non-fatal mirror failure.
Real tmux runs use private test sockets. Only new and impacted tests run in
the agent worktree; the central verifier owns the full suite.
