# SH-566 Handoff

## Delivered

- Engine runs persist immutable agent, model, effort, and speed configuration.
- Initial and replacement lane dispatches reuse the stored configuration.
- CLI, HTTP, human output, JSON, project starts, and epic starts expose the
  configuration through one validated contract and one shared dashboard modal.
- Current main's lifecycle controls, persistent alerts, and lane chips coexist
  with the configured launch surface.

## Commits

- `55605278`: backend persistence, service, CLI, and wire behavior.
- `d093d4d2`: dashboard modal and browser coverage.
- `78eb9ace`: documentation and original handoff.
- `93769252`: verifier-found engine-status golden correction.
- `29d427bb`: fake-tmux composite liveness probe repair for SH-575.

## Reconciliation

- Published history remains unchanged; `origin/main` at `78c2bde0` is merged
  additively into `worktree-SH-566`.
- Main's verification incident is migration 31; engine run options are
  migration 32.
- D15 records verification incidents; D16 records immutable provider options.
- AGENTS.md and its canonical template name SH-566 as the current verifier
  item.
- Dashboard overlay precedence is alert, stop confirmation, launch modal, then
  older surfaces.

## Verifier repairs

- Central verification found two stale complete-output snapshots. `93769252`
  updates only those expected human and JSON outputs.
- The desktop-Chromium engine project exposed a fake-tmux contract defect:
  production requested pane pid, command, and dead state together, but the fake
  returned only the first matching field. `29d427bb` renders the composite and
  derives dead state from the placeholder process; its exact regression test
  passed after reproducing RED.

## Focused verification

- Fake-tmux state regression: 1 passed.
- Desktop Chromium `specs/engine.spec.ts`: 24 passed, including the formerly
  repeatable real-daemon failure.
- Desktop Chromium `specs/overlay-modality.spec.ts`: 6 passed.
- Rust `web_test`: 219 passed.
- Engine-status goldens: 2 passed with snapshot updates disabled.
- AGENTS/template equality: 1 passed; `cargo fmt --check` passed.

## Submission boundary

Run only the new and directly impacted tests, push the additive merge to
existing PR #652, record the evidence on SH-566, then move SH-566 to
`verifying` as the absolute last action. Central verification owns the full
suite, merge, completion, and worktree cleanup.
