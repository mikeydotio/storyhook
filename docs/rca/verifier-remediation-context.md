# SH-584: Verifier preparation and remediation delivery

## Reported failure

PRs 668 (SH-584) and 669 (SH-585) returned to In Progress almost immediately
with a claim that their merge trees failed tests. Their logs contain only a
checkout refusal for modified `scripts/release.sh` and `tests/gate_tiers.rs`.
The gate never started. Callback failure then blocked both stories, claiming
no live window existed.

Read-only inspection found SH-584 at default-server window @86, pane %86,
and SH-585 at @87, both running Codex. Daemon PID 92099 instead inherited
`TMUX=/private/tmp/tmux-501/tmux-status-usage,91965,0`. The original agent
session had stopped at its required final verifying action; the callback was
the mechanism meant to resume it. No background session watch was running.

## Competing explanations and experiments

| Explanation | Controlled evidence |
|---|---|
| Candidate tests failed | Refuted for these logs: preparation aborted before any test command. |
| A moving base ref corrupts the shared verifier view | A real-Git gate that only updates origin/main leaves newer files under the old shared index. A second base advance reproduces the exact checkout refusal. Pinning commit IDs makes both runs clean. |
| Gate-created or pre-existing edits explain the same symptom | Independent tests cover staged and unstaged edits before/during a gate. They must be preserved, never mistaken for a test failure or cleaned by reset. |
| Story windows do not exist | Live inspection contradicts this. Real verifier-actuator/helper tests reach default under the corrected environment and unrelated under the original-environment control. |
| A failed server query proves absence | A failing tmux fixture reproduces lost stderr and false pane-unavailable. Successful empty queries and failed queries now have distinct diagnostics. |
| Correct routing alone guarantees Codex submission | Existing notification test exposed hard-coded Enter despite the configured Codex Tab. Provider-key assertions now cover Codex Tab and Claude Enter. |

Private worktree administration arrived in `56277ba5a`; `c0d0ca5e6` captured
base for private HEAD but still dereferenced mutable refs elsewhere in the
transaction. The fixture proves this defect class, not the provenance of the
live modified files. Notification and its hard-coded submit key originated in
`c0b092fd1` (SH-521). The earlier SH-584 environment fix already covers the
Rust callback boundary, but had not changed the installed running daemon.

## Corrections

- Resolve both transaction refs to commit IDs before checkout, preflight,
  speculative commit creation, and restoration.
- Reject pre-existing tracked edits. Preserve private recovery administration
  when a gate creates edits. A missing legacy HEAD is recoverable only when
  files equal the index and that index equals the pinned base.
- Record the actual gate exit only after restoration succeeds. Missing
  completion evidence is infrastructure failure; a recorded nonzero gate
  exit is a test failure. Infrastructure detail retains the full log path.
- Preserve tmux lookup failures at the shared lookup function, including
  stderr. Notification reports the queried server and uses the provider's
  configured submit key.

## Operational recovery

With no verifying stories queued, gracefully stopped daemon 92099. Used
`git worktree move` to preserve the entire dirty verifier checkout at
`.git/storyhook/verification-recovery-SH-584-20260906`. Both modified files
remained intact. Central verification can create its managed checkout anew.
Restarted the same installed v2.4.2 daemon on port 3456 without TMUX/TMUX_PANE;
new PID 94452 has neither variable, and both agent windows remained live.
No binary/plugin installation, release, reset, stash, gate, merge, or agent
worktree cleanup was performed. The preserved checkout remains evidence.

## Focused validation

All 24 merge-gate tests pass, including advancing refs, phase classification,
and four combinations of tracked-edit preservation. Real tmux integration
passes all three tests, including the production callback and causal control.
Notification lookup and provider-key regressions pass, along with directly
impacted capture/resume/reap/unclaim/reset/completion shell tests. All 44 verifier-queue contract tests also pass, as do scoped Clippy with
warnings denied, formatting, shell syntax, and diff checks. Full-suite
verification remains centralized.
