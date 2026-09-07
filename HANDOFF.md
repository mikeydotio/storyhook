# SH-586 Handoff

## Implemented

- `worktree-SH-586` adds one clickable chip per open PR linked to a Web UI
  story card. Each chip opens the exact GitHub URL in a new browser tab.
- `/api/repos/{project}/data` adds `open_prs` only to story views that have
  current open links. CLI report and detail response contracts are unchanged.
- Card clicks still open the drawer; PR-chip clicks stop propagation. The card
  accessible name includes the full `owner/repo#number` identity.
- Current `origin/main` through SH-579 / PR #671 was merged without rewriting
  the published PR history.

## Targeted validation

- `cargo test --test web_test web_serve_api_data_carries_only_open_pr_links`
- `cargo test --test scaffold`
- `make e2e ARGS='specs/open-pr-chip.spec.ts'` — Chromium, WebKit,
  mobile Chromium and mobile WebKit.
- Targeted formatting and warning-as-error checks are recorded on SH-586.

## Operational limits

- The single open PR is linked on SH-586. Central verification owns the full
  suite, merge, completion and cleanup.
- Do not version, release or deploy from this linked worktree.
- Release-observer results are advisory; scheduler setup belongs in a durable
  checkout. Do not run manual release assembly concurrently with the observer,
  whose passes share a machine lock.

## Preserved context

SH-579, SH-581, SH-584 and SH-585 merged as #671, #670, #668 and #669.
Do not remove `.git/storyhook/verification-recovery-SH-584-20260906`.
SH-557 owns separating the project roadmap from the generated template;
AGENTS.md and its template remain synchronized here.
