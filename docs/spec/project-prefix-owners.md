# Prefix changes and retained operation identity (SH-853)

Changing a prefix refolds stories and relationships. It does not rewrite names
already captured by a running operation or its worktree, branch, pane and lease.
The conservative contract is to refuse while such an operation can still act.
The error names its run/request and asks the operator to finish or resolve it.
`--force` changes confirmation only.

Both `set-prefix` preview and the actual rename call the same guard inside their
existing store transaction. The final rename rechecks after preview; an engine
start admitted in between cannot strand a lane under the old prefix. The normal
verified maintenance backup and compensating-event rewrite remain unchanged.

The owner sweep includes:

- All unfinished Full Auto runs, including paused/halted runs and idle epic
  runs: their retained scope and lanes remain resumable.
- Pending landing intents, legacy reset reservations, unfinished native card
  resets, engine resets, unreleased dropped cleanup and unfinished closure
  cleanup. A reset or cleanup's old lease must never acquire a renamed resource.
- Live verification batches and unfinished bisections, including a released
  parent batch whose bisection still uses its member IDs.
- Outstanding continuations, including uncertain/needs-attention handoffs.
- Attempting block deliveries, whose existing commit fence pins project
  identity while their external effect is unsettled.

Checks are project-local. Finished runs, completed card/closure reset receipts,
released dropped cleanup, terminal batches without unfinished bisection, and
acknowledged/superseded continuations are history and do not block a rename.
Retained history is not rewritten to pretend it was created with the new prefix.

Other durable records were checked for their identity policy. Pending block
deliveries store the project and story number and derive the current display ID
when acted on. Project recovery already validates retained managed leases by their minted
story number (SH-848). Attribution evidence and gate-attempt journals are retained
evidence keyed by numeric story/generation; they do not grant execution authority
(SH-870). None should turn retired history into a permanent rename prohibition.

The guard neither stops runs nor retires owners. It performs no resource cleanup,
changes no automation policy, and grants no broader reset or landing authority.

Single-story verification handoffs also own their minted cleanup lease without
an engine run, batch or landing intent. A current leased Verifying generation
blocks rename even when the queue is held or stopped. Unreaped In Progress
repair/publication work and closed leased generations remain owners too. The
latest generation's cleanup-complete marker or a completed, newer closure
receipt for the exact same lease retires historical cleanup authority; an older
generation's marker cannot release a new submission. A current Verifying
submission still blocks even if a cleanup marker was manually added. Todo after
a finished reset carries history, rather than a current verifier handoff.
