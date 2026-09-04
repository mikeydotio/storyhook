# Reap certified cleanup after losing the resources it was responsible for

- **Date**: 2026-09-03 (failure observed 2026-09-02)
- **Severity/Impact**: One observed autonomous verification run (SH-522) recorded terminal
  cleanup success while its original local worktree and branch survived. No code, story
  history, merge, remote branch, or tmux window was lost; the false receipt hid required local
  recovery. The condition was reachable by centralized verification from 2026-08-30 until the
  fix.
- **Status**: Fixed by SH-473, PR #615

## Summary

Central verification could report `CENTRAL VERIFICATION CLEANUP COMPLETE` after a project
checkout changed even though the dispatched story worktree and branch still existed. The
dispatch-to-verification interface did not retain the exact resources dispatch created, and
the helper and daemon treated an incomplete or contradictory cleanup result as success. The
fix persists a generation-scoped cleanup lease, reaps only that exact identity, and accepts
success only from a typed receipt proving every leased resource absent. The lesson is that
asynchronous cleanup must carry immutable creation-time identity and verify postconditions;
mutable discovery at cleanup time cannot establish ownership or completion.

## Timeline

| When | What | Anchor |
|---|---|---|
| 2026-08-04 | Repository-side helper verbs begin resolving resources from the project's one currently registered checkout. This creates the latent identity-loss condition when registration later changes | `428d658` |
| 2026-08-08 | `reap` gains worktree and branch deletion but returns unconditional `ok:true` after deletion errors | `940261e` |
| 2026-08-22 | Provider-specific worktree namespaces make ambient provider selection a second way to omit an original resource | `ef976a1` |
| 2026-08-30 | Central verification begins trusting the helper's `.ok` value as a terminal cleanup receipt, making the latent helper contract reachable after asynchronous verification and restart | `c0b092f` |
| 2026-09-02 | SH-522 beta acceptance temporarily registers a standalone release checkout. Reap records cleanup complete while the original worktree and branch survive | SH-522, SH-473 |
| 2026-09-02 | A deterministic same-checkout control and checkout-switch regression reproduce the false completion | `8263950` |
| 2026-09-02 | Direct helper deletion failures become nonzero `ok:false` results | `abf543c` |
| 2026-09-03 | A versioned cleanup lease and exact receipt redesign land on the feature branch; formatting and event-size follow-ups preserve the same wire contract | `26af45f`, `cc44cd2`, `2da37b0` |
| 2026-09-03 | The complete focused regression matrix and full `make test` gate pass at the exact PR tree | `4380f10` (tree `5673c6a17522305fcb2dbd4d8d4fd1232dd9c7e9`) |
| 2026-09-03 | Direct and final-session reap begin consuming the caller's private cleanup marker, so they retain exact ownership even when project registration changes | `ae088f4` |
| 2026-09-03 | Dispatch, unclaim, capabilities, and verification notification reject contradictory nonzero `ok:true` helper results; all focused boundary suites pass | `54742da` |

## Root cause & trigger

The verified defect→infection→failure chain had two paths to the same false terminal
receipt:

1. **Defect — missing durable ownership.** Dispatch created a provider-scoped repository,
   worktree, branch, and tmux window identity, but no story or verification event preserved
   that identity. Verification later rebuilt cleanup targets from the project's mutable
   current checkout and ambient provider (`plugins/story/bin/story.sh`,
   `src/service/verification.rs`).
2. **Infection — the cleanup plan no longer named the original resources.** Replacing the
   registered checkout with a clean clone made reap attempt no original worktree or branch.
   Experiment 2 made the failure follow only the checkout registration value; experiment 4
   returned success with no errors while both original resources remained.
3. **Independent boundary violation — discovered failures also became success.** When a
   collision or provider mismatch did expose a Git deletion error, `cmd_reap` serialized the
   error alongside unconditional `ok:true`. Experiment 1 moved the regression
   RED→GREEN→RED by changing only that predicate.
4. **Failure — a claim replaced proof.** `ShellVerificationActuator` trusted `.ok`, ignored
   process status and exact resource postconditions, and wrote a terminal cleanup-complete
   comment. That comment excluded the story from restart recovery even though local resources
   survived.

**ODC classification:** primary **Interface / Missing**, triggered by configuration change,
provider change, or daemon restart; secondary **Interface / Incorrect**, triggered on the
cleanup recovery/error path. The observed **NOW** trigger was SH-522's temporary project
checkout relink during beta acceptance. Provider loss is a verified sibling trigger, not a
requirement: the checkout-switch reproduction also failed with Claude resources.

## Contributing factors

- A project store retains one current checkout, not checkout history, so cleanup-time lookup
  could not recover the displaced repository identity.
- Full Auto lane records clear their resource fields after a lane completes and therefore
  could not serve as a restart-safe cleanup ledger.
- The provider selected the worktree namespace but was ambient at reap time; existing Codex
  coverage explicitly supplied `STORY_AGENT=codex` while the daemon did not.
- The helper's result schema allowed `ok:true`, false removal fields, and error strings to
  coexist. The daemon reduced that contradictory structure to one Boolean.
- Tmux closing was best-effort while the durable success wording claimed the window had been
  reaped, extending the same false-proof class beyond Git resources.
- Tests covered same-checkout cleanup, safety refusals, and idempotent absence, but not
  checkout replacement, daemon restart with changed configuration, provider drift, or exact
  receipt validation.

## The fix

The verdict was **REDESIGN** at the dispatch→verification cleanup contract, plus a separate
**SURGICAL** helper correction:

- **`abf543c`** makes direct `reap` deletion failures return nonzero `ok:false` results and
  reports surviving resources accurately. This closes the explicit-error false-success path.
- **`26af45f`** records a versioned `StoryCleanupLeaseRecorded` event immediately after the
  active generation enters `verifying`. The lease contains canonical repository and worktree
  paths, exact branch, and a complete tmux fingerprint. Immediate and restart cleanup consume
  the latest generation's lease rather than current configuration.
- The same commit requires a zero helper exit plus a typed receipt echoing the exact story and
  lease and proving worktree registration, worktree path, branch, and tmux fingerprint absent.
  Missing legacy leases remain explicitly cleanup-required instead of fabricating success.
- **`cc44cd2`** applies canonical formatting. **`2da37b0`** boxes the rare lease event payload
  to preserve the event-store large-enum invariant without changing JSON. **`4380f10`** derives
  the protocol diagnostic assertion from the public required-protocol constant.
- **`ae088f4`** makes a reap invoked from the dispatched worktree load its private marker before
  any mutable project lookup and route through the same exact leased cleanup path. A present but
  invalid marker fails closed rather than falling back to discovery.
- **`54742da`** requires process exit zero wherever dispatch, unclaim, capabilities, or
  notification claims `ok:true`. Structured `ok:false` business refusals keep their diagnostic
  payloads even when they intentionally exit nonzero.

This fixes the origin: dispatch publishes identity before configuration can drift, append-only
story history retains it across restart, and cleanup proves absence of exactly what dispatch
created. Protocol 3 keeps the daemon and helper contract in lockstep. No SQL migration or
history rewrite is required; older histories remain readable and safely require operator
cleanup when no lease exists.

## Preventative action — killing the class

The following guards landed with the fix:

1. **`plugins/story/tests/test-reap-checkout-switch.sh`** runs the real helper against both a
   same-checkout control and a clean replacement checkout. Its leased case requires removal of
   the original worktree and branch plus true postconditions in the versioned receipt, without
   ambient provider selection.
2. **`tests/verification_queue.rs::real_shell_actuator_reaps_the_leased_original_from_a_clean_replacement_checkout`** exercises the production Rust actuator and production shell helper end to end, proving mutable current checkout cannot redirect cleanup.
3. **`latest_generation_shadows_old_leases_and_restart_cleanup_survives_checkout_change`**
   makes event order an executable generation invariant: a later unleased submission cannot
   inherit an old lease, and restart recovery retains the current generation's exact lease.
4. **`verifying_transition_validates_and_atomically_records_a_private_git_marker`** rejects
   malformed or contradictory dispatch markers and pins lease capture to the same transition
   batch as `verifying`.
5. **`shell_cleanup_requires_a_latest_generation_lease_before_spawning`** and
   **`shell_cleanup_rejects_nonzero_identity_version_and_postcondition_receipts`** make false
   completion loud for missing identity, nonzero exit, wrong story, wrong protocol version, or
   any unproven postcondition.
6. The public cleanup types in `src/domain.rs` encode the durable contract: centralized
   cleanup completion now means the exact leased worktree registration and path, local branch,
   and tmux window generation were all verified absent.
7. Dispatcher, capabilities, unclaim, and verification-notify regressions independently prove
   that parseable success JSON cannot override a failed process exit.

## Lessons

- Resource discovery and resource ownership are different facts. Configuration can locate a
  resource today; it cannot prove which resource an earlier asynchronous operation created.
- A cleanup API must return evidence, not intent. Exit status, exact identity, and explicit
  postconditions all participate in success; a standalone `ok` field is insufficient.
- Recovery identity belongs in durable lifecycle history before the producer exits. Caller
  working directory, provider defaults, tmux pane discovery, and cleared lane state are not
  restart-safe contracts.
- Event adjacency is a useful generation boundary in an append-only model: it prevents a new
  submission from accidentally inheriting cleanup authority from an older one.
- Durable status text must be no stronger than the proof behind it. If any named resource is
  best-effort or unverifiable, the result remains cleanup-required.
