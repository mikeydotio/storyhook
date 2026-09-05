# SH-566 Handoff

## Delivered

- Engine runs persist immutable agent, model, effort, and speed configuration.
- Every initial and replacement lane dispatch reuses that configuration.
- CLI, HTTP, human output, and JSON expose the fields with shared validation.
- Project and epic starts use one Full Auto modal backed by dispatch
  capabilities and provider preferences without changing attended Auto mode.
- Live dashboard controls show the active configuration.

## Preserved state

- Branch: `worktree-SH-566`.
- Backend commit: `55605278` (`feat(engine): persist launch configuration`).
- Dashboard commit: `d093d4d2` (`feat(dashboard): configure Full Auto launches`).
- Store schema is version 31.
- Explicit standard speed preserves the historical lane argv; fast emits its
  provider override.

## Focused verification

- Focused engine model, dispatcher, reconcile, restart, shell-reuse, wire, CLI,
  daemon, HTTP, and dashboard structural tests pass.
- `specs/engine.spec.ts`: 15 pass in each of Chromium, WebKit, mobile
  Chromium, and mobile WebKit.
- Formatting and focused documentation tests must remain green before push.

## Submission boundary

Push one SH-566 PR, link it to the story, then move SH-566 to `verifying` as
the absolute final action. The centralized verifier owns the full suite,
merge, completion, and cleanup.
