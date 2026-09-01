# Handoff

## Completed in SH-523

- `story.sh dispatch <id> --resume` inventories and safely reconstructs the
  active claim, deterministic branch/worktree, and named tmux pane.
- Existing commits and dirty files survive; failure cleanup removes only
  resources created by the current attempt.
- Attended adapters ask once on `resume-available`; dashboard dispatches pass
  `--resume` automatically. Full Auto lanes retain `resume:false`.
- Dispatch protocol 2 prevents the daemon from invoking a stale helper that
  cannot accept automatic resume.
- Shell, daemon, and browser regressions cover preservation and UI reachability.

## Next

- Continue the Full Auto epic from `story next`; reconciliation and close-out
  stories remain the source of truth for lane supervision and real-run proof.
- Keep engine-lane recovery separate until SH-466 defines its reconciliation
  policy; do not change `DispatchOptions::default().resume` to true.
- Treat `resume-unsafe` as evidence requiring operator repair, never as a reset
  opportunity.
