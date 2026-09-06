# Claude and Codex plugin reinstall inherited a deleted daemon working directory

- **Date**: 2026-09-06 PDT
- **Severity/Impact**: Loud supported-path failure. `story plugin install` could not reinstall either supported provider, blocking Story SH-582 and SH-566 verification. No store corruption, silent data loss, or unrelated command failure was observed; the duration before detection is unknown.
- **Status**: Fixed on `fix/SH-582-daemon-cwd` in `73ef69cb8` and `764af159a`; PR #666 is open and not yet merged.

## Summary

Claude and Codex plugin reinstall failed because their provider processes inherited the Storyhook daemon's deleted working directory. The long-lived daemon had retained the cwd of the client that started it, even though that cwd belonged to a worktree that was later removed. Commit `73ef69cb8` establishes a stable cwd at the common daemon run boundary, and commit `764af159a` applies the same lifetime rule to the machine-wide verifier tmux pane. The durable lesson is that every process or pane that can outlive its caller must explicitly own a working directory with the same lifetime.

## Timeline

- **2026-06-05** — Commit `5274a65ec` moved Claude plugin management to marketplace subprocesses without assigning those subprocesses an explicit cwd.
- **2026-07-29** — Commit `74bd28e90` added daemon auto-spawn. The daemon detached its process group and streams but continued to inherit its initiating client's cwd.
- **2026-08-02** — Commit `552933e3e` routed ordinary CLI commands through the daemon, making the daemon's inherited process state relevant to later provider operations.
- **2026-08-22** — Commit `5f67c4942` introduced the shared Claude/Codex provider-command builder, which also inherited ambient daemon cwd.
- **2026-09-04** — Commit `2dd58d19f` added release-backed replacement and remove-before-add behavior. The removal step exposed the latent invalid-cwd state; it did not create it.
- **2026-09-06 11:03 PDT** — Both supported reinstall commands failed. Claude reported a deleted current directory; Codex failed configuration loading with `ENOENT`. The running Storyhook 2.4.0 daemon was found holding the already-deleted SH-569 worktree as its cwd.
- **2026-09-06** — FULL RCA for Story SH-582 reproduced both failures deterministically, eliminated `PWD` filtering and reinstall ordering as durable causes, and verified the daemon-cwd chain with a fail → pass → fail experiment.
- **2026-09-06 12:53 PDT** — Commit `73ef69cb8` established the daemon cwd invariant and added the two-phase Claude/Codex regression.
- **2026-09-06 13:01 PDT** — Commit `764af159a` fixed the same lifetime defect in the verifier tmux pane and added exact argv coverage.
- **2026-09-06 13:03 PDT** — Commit `42648983f` recorded the SH-582 verification handoff and roadmap state. PR #666 remains open pending centralized verification.

## Root cause & trigger

The verified chain was:

1. Daemon startup did not initialize process cwd to lifecycle-owned state.
2. The daemon inherited the cwd of the first client and outlived that client's worktree.
3. Worktree cleanup deleted the directory while the daemon remained alive.
4. Claude and Codex provider builders inherited the daemon's invalid OS cwd. The valid cwd serialized with the later request was logical request context and could not repair process state.
5. Release replacement invoked cwd-sensitive provider removal. Claude reported the deleted directory directly; Codex surfaced configuration loading failure with `os error 2`.

The trigger was the conjunction of daemon startup from an ephemeral directory, later deletion of that directory, and execution of a cwd-sensitive child. Commit `2dd58d19f` made that state visible during reinstall, but the missing lifetime invariant originated at the daemon boundary.

ODC classification: **Assignment/Init / Missing / startup-restart plus deleted-directory boundary condition**. The architectural pattern was a missing invariant coupled through ambient OS process state.

## Contributing factors

- The daemon's detachment contract explicitly controlled file descriptors, environment, streams, and process group, but did not include cwd ownership.
- Existing plugin tests started the daemon from a stable harness root. Their provider fakes modeled registration changes without deleting or invalidating the invocation directory.
- Request cwd and daemon process cwd are intentionally separate. Correct request serialization therefore concealed rather than corrected the daemon's stale OS state.
- Provider availability and lifecycle commands shared the same ambient-cwd behavior, so one daemon defect affected both supported providers.
- Worktrees and provider caches are intentionally removable, while the daemon and verifier pane are intentionally long-lived. Their lifetimes made caller-cwd inheritance unsafe.
- The release replacement sequence supplied the first repeatable cwd-sensitive operation after infection, making remove-before-add look causal even though bypassing it left the daemon invalid.

## The fix

Commit `73ef69cb8` adds `enter_stable_working_directory` to `src/daemon/lifecycle.rs` and calls it from `daemon::lifecycle::run` before the daemon's first fallible initialization. The common run boundary covers auto-spawn, direct `daemon --serve`, and launchd entry. It changes process cwd to `Environment::home()` and reports both the target path and OS error if that invariant cannot be established. Request-scoped paths and provider protocols remain unchanged.

The same-class sweep found a machine-wide verifier tmux pane that could outlive the candidate worktree that created it. Commit `764af159a` updates `scripts/verify-window.sh` so `new-session`, `new-window`, and both `respawn-pane` forms pass `-c "$HOME"` as separate argv elements. Commit `42648983f` contains lifecycle documentation only.

The verdict was **SURGICAL**: one common daemon boundary could establish the missing invariant without changing wire data, provider ordering, or project-path semantics. Focused Rust, shell, formatting, Clippy-with-warnings-denied, and diff-hygiene checks passed as recorded in Story SH-582 and `HANDOFF.md`. The repository's full suite was not run locally; centralized verification owns it after PR #666 enters verification.

## Preventative action — killing the class

The production invariant is now explicit: a daemon enters canonical `Environment::home()` before it initializes or serves work, so no later child can inherit a removed client checkout.

Two tests in `tests/plugin_reinstall_cwd.rs` enforce the live two-phase failure shape:

- `claude_reinstall_uses_stable_cwd_after_daemon_startup_directory_is_deleted`
- `codex_reinstall_uses_stable_cwd_after_daemon_startup_directory_is_deleted`

Each starts a real daemon from an ephemeral directory, removes that directory, invokes reinstall from a different stable client cwd, proves the daemon PID did not change, and requires the provider's canonical cwd to equal `HOME`. This separates daemon lifetime from provider removal ordering and prevents a provider-local workaround from satisfying the contract accidentally.

The verifier-pane sibling is guarded by exact argv tests in `tests/verify_window.rs`:

- `a_new_session_and_its_banner_respawn_use_the_stable_home_cwd`
- `existing_session_banner_and_tail_respawns_use_the_stable_home_cwd`

Together they cover `new-session`, `new-window`, banner respawn, and tail respawn, including a `HOME` path containing a space. Future long-lived process and pane additions must follow the same rule: select an explicit lifecycle-owned cwd, and test the actual spawn argv or child-observed cwd.

## Lessons

- Cwd is inherited process state with a lifetime, like file descriptors and environment variables; detachment is incomplete until all three are owned explicitly.
- A request's logical cwd cannot repair the daemon's OS cwd. Project-scoped work must use request paths, while projectless long-lived infrastructure needs a stable neutral directory.
- A last-aligned change can expose an older defect. Removing the reinstall trigger would have hidden the failure while preserving invalid daemon state.
- Regression tests for lifetime defects must separate startup, resource deletion, and later use. One-call fixtures can prove a symptom without proving which lifetime boundary owns the cause.
- Sibling sweeps should include non-process abstractions such as tmux panes when they survive the checkout that created them.
