# SH-586 Handoff

## Implemented

- `worktree-SH-586` adds one clickable chip per open PR linked to a Web UI
  story card. Each chip opens the exact GitHub URL in a new browser tab.
- `/api/repos/{project}/data` adds `open_prs` only to story views that have
  current open links. CLI report and detail response contracts are unchanged.
- Card clicks still open the drawer; PR-chip clicks stop propagation. The card
  accessible name includes the full `owner/repo#number` identity.
- Current `origin/main` through SH-574 / #673 and SH-374 / #680 was merged
  without rewriting the published PR history.

## Targeted validation

- `cargo test --test web_test web_serve_api_data_carries_only_open_pr_links`
- `cargo test --test scaffold --test e2e_fixture_hygiene`
- Two focused `e2e_browser_coverage` guards for SH-374.
- `make e2e ARGS='specs/open-pr-chip.spec.ts'` — Chromium, WebKit,
  mobile Chromium and mobile WebKit.
- `notice-autorepeat.spec.ts` — six cases in Chromium and WebKit.
- Targeted formatting and diff checks are recorded on SH-586.

## Operational limits

- The single open PR is linked on SH-586. Central verification owns the full
  suite, merge, completion and cleanup.
- Run Rust builds and browser tests sequentially: both use `target/debug/story`,
  and executable mtime changes invalidate a running daemon.
- Commit/push identity checks run before receipt bypass and inspect outgoing
  history. Explicit identity alternatives require a role and reason.
- Release-observer results are advisory; scheduler setup belongs in a durable
  checkout. Do not run manual release assembly concurrently with the observer,
  whose passes share a machine lock.
- Do not version, release or deploy from this linked worktree.

## Preserved context

SH-374 makes clipboard failure deterministic and exercises notice auto-repeat
in Chromium and WebKit. SH-342's post-git sync remains silent and best-effort.
SH-577's manual-press helper validates a reachable hit target after settling.
Detailed closed-story evidence remains on each owning story.
Do not remove `.git/storyhook/verification-recovery-SH-584-20260906`.
SH-557 owns separating the project roadmap from the generated template;
AGENTS.md and its template remain synchronized here.
