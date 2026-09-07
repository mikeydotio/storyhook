# SH-587 Handoff

## Defect and repair

- SH-577 was returned for PR reconciliation twice. Each conflict released
  verifier ownership, so another story merged and invalidated SH-577 again.
- `src/daemon/verification.rs` treated conflicts like red tests and invalid
  submissions; the serialized worker had no reservation state.
- A conflict still records its diagnosis, moves the story to `in-progress`,
  and notifies the exact agent. The verifier now retains its activity and
  shutdown-drain guards while waiting for that story's newer `verifying`
  generation, then retries it immediately.
- Queue changes and unrelated events cannot transfer the reservation. Repeated
  conflicts preserve it; merge, red, invalid, and infrastructure paths do not.
- Notification failure releases ownership with the existing `awaiting`
  diagnosis, preventing an unreachable agent from deadlocking the queue.
- Shutdown cancels the process-local wait. No lease, schema, CLI, or REST
  change was added.

## Original validation

- RED: the reservation integration test failed to compile until the
  reconciliation-aware cycle existed.
- GREEN: `verification_queue` 46/46 and `verification_shutdown_drain` 3/3.
- Scaffold and scope-rubric contracts passed 24/24. Timing-policy passed 5/5.
- Targeted Clippy denied warnings; formatting and whitespace checks passed.
- One sandboxed daemon-access denial was environmental; the identical
  permitted run passed.

## Main reconciliation

- PR #682 conflicted after SH-574 merged as #673. Merge origin/main
  `563ee7e7a` additively; never rewrite the published SH-587 history.
- Preserve SH-574 unchanged: effective author/committer validation before
  commit, stored outgoing identity validation before push, explicit role and
  reason for alternatives, full outgoing-range checks, stdin preservation,
  and checks before receipt bypass.
- Preserve imported GitHub committers through SH-574's documented
  committer-only alternative and keep daemon containment in identity fixtures.
- Incoming identity implementation and tests had no SH-587 implementation
  overlap. Resolve only the roadmap/template and this handoff.
- Preserve SH-342 and SH-577 completion in both synchronized roadmaps, mark
  SH-574 complete, and retain SH-587 as current.
- Reconciliation validation passed all 78 SH-587 tests, all 96 identity and
  hook/gate tests, and all 10 browser-gate tests. Targeted Clippy denied
  warnings; ShellCheck, Bash syntax, formatting, and whitespace checks passed.
- The additive merge commit and final evidence are recorded on SH-587.

## Operational constraints

- SH-579's tag/preflight observer is advisory; do not repair published tags or
  run manual release assembly concurrently with it.
- SH-581 preserves extracted-source Lima builds and caller cwd. SH-585 owns
  installed-launcher reader exceptions. SH-584 owns dispatch isolation,
  installer validation, and verifier restoration/remediation behavior.
- Preserve `.git/storyhook/verification-recovery-SH-584-20260906`.

## Submission boundary

Continue PR #682. Run only SH-587 and incoming identity/hook impacted tests,
commit the additive merge, and push normally. Comment results on the PR and
SH-587, then make `story move SH-587 verifying` the absolute last operation.
The centralized verifier owns the full suite, merge, completion, and cleanup.
Do not release, version, deploy, land, reap, or run the full suite here.
