# SH-204 Handoff

## Delivered

- All label producers and selectors use one Unicode-aware lowercase
  canonicalizer; case variants are one label identity.
- Schema migration 30 appends compensating `StoryLabelsSet` events for every
  affected open or closed story and rebuilds its label read model.
- The drawer and create modal share one chip combobox. Comma, Enter, and field
  exit commit; suggestions and removals persist; failed additions remain
  available for retry.
- CLI help, README guidance, and plugin reference document the invariant.

## Preserved state

- Branch: `worktree-SH-204`.
- Storage/API commit: `b6a87191` (`fix(labels): canonicalize labels as lowercase`).
- Dashboard commit: `6d7b3def` (`fix(dashboard): unify label combobox behavior`).
- Store schema is version 30; upstream migration 29 remains unchanged.
- Reconciliation merges `origin/main` at `e56df766` without rewriting the
  published SH-204 commits.

## Focused verification

- After reconciliation, domain, service, migration, label CLI/query, integrity,
  help, and static dashboard suites passed.
- Clippy with warnings denied and formatting checks passed.
- New label-editor E2E: 2 passed in Chromium and 2 in WebKit.
- Directly impacted label E2E: 8 passed in Chromium.
- Main's directly impacted peer-panel E2E: 4 passed in Chromium.

## Submission boundary

Push the reconciliation merge to existing PR #636, then move SH-204 to
`verifying` as the absolute final action. The centralized verifier owns the
full suite, merge, completion, and cleanup.
