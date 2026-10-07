# SH-882: Keep submissions moving when verification is stopped

## Summary

In v3.0.3, implementer test restrictions already exist. The missing behavior is that stopped verification also prevents PR publication and landing.

Separate test execution from submission processing. When verification is stopped, eligible submissions must still publish, merge, complete, and clean up—with explicit evidence that verification was skipped.

## Implementation sequence

1. Run `story comment SH-882 <exact-approved-plan>`, passing this entire approved plan verbatim before changing files or running tests.
2. Repeat `story help obviation-review` and `story load-context --story SH-882 --format json`. Review every candidate’s discussion and relevant implementation evidence. Record the findings before implementation; preserve work owned by active sessions.
3. Record the decisions below using **Context, Question, Decision, Rationale**. Capture the behavior and compatibility contract in `docs/spec/stopped-verifier-submission.md`.
4. Add failing regression tests, implement the changes, and run only SH-882’s added or changed tests.
5. Commit focused changes with their regression tests. Record results and remaining limitations. From this worktree, make `story move SH-882 verifying` the absolute last action.

No push, PR creation, merge, release, version change, or live verifier-control change belongs to this implementation session.

## Behavior and implementation

### Implementer test scope

- Retain the shared rule: run only tests the story adds or changes.
- Retain the explicit exception allowing exact named failures from a central gate to be rerun. Do not broaden it to targets, files, scripts, or suites.
- Update instructions that unconditionally promise central testing to explain that stopped verification leaves testing to release gates.
- Extend instruction-contract coverage to prevent conflicting guidance. Do not build a new test-command interception system.

### Stop, Drain, and Start

| Control | Active attempt | Subsequent eligible submissions |
|---|---|---|
| Start | Preserve its admitted mode | Run the configured gate before landing |
| Drain | Finish normally | Publish and land without running the gate |
| Stop | Cancel and settle owned processes | Publish and land without running the gate |

- Retain the durable enabled flag as the gate policy; separate it from permission to own submission work.
- Add an explicit attempt mode: gated or verification-skipped. Select it atomically during admission and retain it throughout that attempt.
- Stop must settle cancellation and descendant cleanup before another attempt starts. Restart must recover unresolved landing authority before admitting fresh work.
- Existing stopped projects receive the requested behavior after upgrade; no additional opt-in is required.
- Keep manual blocks, dependencies, human-only reservations, reset fences, infrastructure halts, and project-recovery holds authoritative. A stop command does not clear them.
- Process skipped submissions individually. Do not create new verification batches in skipped mode; preserve recovery of existing batch landing intents.

### Submission and landing authority

The principal changes belong in the verifier admission/orchestration, durable landing model, and shell landing boundary.

- Reuse normal branch publication, PR adoption, remote-default-branch discovery, merge preparation, ownership, and cleanup.
- Add a distinct prepared-without-verification outcome carrying the exact submitted head and proposed merge tree.
- Represent landing authority explicitly as either certified or verification-skipped. Skipped authority includes the admitted control policy and attempt identity; it must never masquerade as `VerifiedSubmission`.
- Read existing persisted landing intents as certified authority. Preserve their recovery behavior and reject malformed or incomplete new authority.
- Admit skipped landing through the same transactional generation, PR, dependency, and human-reservation checks as certified landing.
- Add a private landing path for durable skipped authority. It must omit gate execution and certification requirements while retaining merge locking, metadata validation, conflict detection, exact-head matching, and post-merge ancestry/tree confirmation.
- Ordinary landing entry points retain certification requirements. Do not create a fake passing receipt or use a successful no-op command as a gate.
- A changed head or proposed merge tree requires fresh preparation and authority. An uncertain external merge retains its intent and permits observation-only recovery, not another blind merge request.
- Preserve ordinary GitHub requirements and merge commits. Exact-head matching follows the [GitHub CLI merge contract](https://cli.github.com/manual/gh_pr_merge); do not introduce administrator bypass.

### Completion and visibility

- Add a distinct durable completion marker stating that the PR merged while verification was stopped and no gate ran.
- Teach completion recognition, generation history, cleanup eligibility, and restart recovery to recognize that marker without counting it as certification.
- Atomically record completion and resolve landing authority using the existing transaction boundary, consistent with [SQLite atomic commit](https://www.sqlite.org/atomiccommit.html).
- Update CLI help, status, dashboard controls, progress messages, and generated agent instructions.
- Show “Verification stopped — submissions continue without tests,” alongside active publication or landing progress. Do not label skipped tests passed or reused.
- Preserve release gates and their receipt requirements.

### Pending landing recovery (SH-892)

Stop cancels active owned work and disables future gates; it does not revoke an
existing landing intent. After cancellation settles, recovery may observe that
intent in its own project, including after a daemon restart. Recovery must not
publish again, prepare a new merge, execute a gate, or send another merge request.
An uncertain observation (including an open PR) retains the exact intent and
leaves the story verifying. A confirmed merge completes it once and resolves the
intent atomically. Later ticks do not repeat recovery or completion.

The intent's original authority determines the completion evidence. A certified
intent retains its original certification even when recovery occurs under Stop.
A verification-skipped intent retains its admitted attempt identity and skipped
marker; recovery cannot turn it into a passing gate receipt.

Project manual mode is a separate, stronger boundary. With `automations.enabled`
false, no automatic landing observation or completion runs, even when verification
is stopped. Re-enabling automations does not replay pending authority from before
the disable boundary. See [Per-project manual mode](project-manual-mode.md).
Human reservations, reset fences, project identity and other authority checks
remain in force; Stop grants no exception to them.

The old `pending_landing_recovery_respects_project_and_stop_permission` assertion
expected Stop to hold recovery. That expectation predated SH-882. Commit
`921bd2bf` corrected it to require completion while retaining project isolation
and the prohibition on a second merge. SH-892 adds uncertain-to-confirmed recovery
coverage for both authority types and covers their interaction with manual mode.

## Validation

Use isolated stores, real local Git repositories, production orchestration, and mocked external endpoints where necessary.

- Stopped mode publishes or adopts a PR, prepares its merge, lands it, completes the story, and performs cleanup without invoking a gate or creating certification.
- Running mode still requires certification; Drain finishes its active attempt; Stop settles cancellation before admitting skipped work.
- Mode selection survives restart and remains stable across concurrent control changes.
- Multiple queued submissions advance; repeated ticks cannot duplicate publication, landing, completion, or cleanup.
- Dependencies, human-only reservations, infrastructure/recovery holds, and reset fences prevent unauthorized progress.
- Conflicts, changed heads, changed bases, stale generations, malformed authority, uncertain merge responses, and cleanup failures retain correct recovery behavior.
- Legacy certified intents remain readable; skipped intents cannot authorize the ordinary certified path or satisfy release receipts.
- CLI/API and focused browser tests show stopped verification with continuing submission activity and truthful skipped outcomes.
- Update tests whose specified stop behavior intentionally changes; retain their ownership and race assertions. Never weaken unrelated failing tests.

Run tests by exact case or explicit new-case selection. Run relevant non-mutating formatting and static checks with warnings treated as errors. Leave all other tests to the central verifier and release gates.

## Defaults and boundaries

- “Stopped” changes gate execution, not the submission workflow.
- Already queued, otherwise eligible submissions also advance without verification.
- Admission fixes an attempt’s mode; Start affects later admissions.
- Existing holds remain holds. SH-882 does not take over the active resource-admission, causal-diagnosis, or recovery-status stories.
- All implementation remains assigned to SH-882. Record later decisions immediately before acting.
