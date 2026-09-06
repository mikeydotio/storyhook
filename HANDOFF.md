# SH-584 Handoff

## Delivered

- Daemon dispatch excludes inherited TMUX/TMUX_PANE; Full Auto monitoring and
  stop use the same environment boundary. Interactive context and explicit
  cleanup-lease socket targeting remain intact.
- Codex installation verifies the enabled release path, complete embedded
  payload, bytes, and executable permissions before reporting success.
- Dispatch capabilities cache tracks helper path and content digest so a
  refreshed helper immediately supplies Astra to the web dropdown.
- Browser regression selects Astra through the real catalog and dispatch
  helper, then checks the executed provider argv, claim/comment, worktree,
  session/window, and prompt handoff.

## Evidence and limits

- Committed RCA: docs/rca/web-dispatch-tmux-and-stale-catalog.md.
- Historical attempts reported success on a now-absent unrelated tmux server.
  Real isolated two-server tests reproduced the routing defect. Historical
  agent activity remains unproven; v2.4.0 is not a verified good baseline.
- Installed plugins were stale v2.4.0 against binary v2.4.2. No production
  installation was changed. Provider fixtures prove installer false success;
  they do not prove an install was attempted during the original incident.
- SH-583 is absorbed. SH-585 exclusively owns the installed-path guard;
  reproduction and abandoned draft were posted there, with no guard edits here.

## Focused verification

- Plugin install 25; freshness 1; reinstall cwd 2; plugin units 9.
- Options endpoint 9; dispatch endpoint 24; engine dispatcher 9; claim reuse 1.
- Real tmux integration 2; spawn environment units 6; engine shell units 2.
- Eight directly impacted shell dispatch scripts passed.
- Dispatch browser spec: Chromium 6 and WebKit 6; final strengthened Astra
  assertion rerun passed on both. E2E environment isolation regression passed.
- Scoped Clippy with warnings denied, formatting, shell syntax, and diff checks.
- Engine shell units initially exceeded their existing 3-second deadlines;
  isolated and serial reruns passed. Timing cause remains unverified.

## Submission boundary

One PR on worktree-SH-584 is linked to SH-584, then the story moves to verifying.
Central verification owns the full suite, merge, completion, and lane cleanup.
Do not version, release, deploy, merge, or remove this worktree. Repair the same
PR with additional commits if verification returns it.
