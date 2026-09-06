# SH-566 Handoff

## Delivered

- Engine runs persist immutable agent, model, effort, and speed configuration.
- Every initial and replacement lane dispatch reuses that configuration.
- CLI, HTTP, human output, and JSON expose the fields with shared validation.
- Project and epic starts use one Full Auto modal backed by dispatch
  capabilities and provider preferences without changing attended Auto mode.
- Live dashboard controls show the active configuration.

## Evidence

- Branch: `worktree-SH-566`.
- Backend commit: `55605278` (`feat(engine): persist launch configuration`).
- Dashboard commit: `d093d4d2` (`feat(dashboard): configure Full Auto launches`).
- Documentation commit: `78eb9ace` (`docs(engine): describe configured Full
  Auto runs`).
- Store schema is version 32: main's verification incident is migration 31 and
  SH-566's engine run options are migration 32.
- Explicit standard speed preserves the historical lane argv; fast emits its
  provider override.

## Reconciliation

- Current `origin/main` is merged additively; published SH-566 history remains
  unchanged.
- Main's verification incident store/API/dashboard behavior and SH-566's run
  configuration behavior are both retained.
- The append-only design ledger assigns D15 to verification incidents and D16
  to immutable provider configuration.
- AGENTS.md and its canonical scaffold template both name SH-566 as the current
  verifier item.
- Focused engine, CLI, wire, HTTP/dashboard, migration, verifier-incident, help,
  README, and roadmap-contract Rust targets pass.
- `specs/engine.spec.ts` passes 15/15 on Chromium, WebKit, mobile Chromium, and
  mobile WebKit. `specs/verification-incident.spec.ts` passes on Chromium and
  WebKit; it intentionally selects no mobile cases.
- Formatting and workspace/all-target warnings-as-errors checks pass.

## Submission boundary

Push the additive merge commit to existing PR #652, comment final evidence,
then move SH-566 to `verifying` as the absolute last action. Central
verification owns the full suite, merge, completion, and worktree cleanup.
