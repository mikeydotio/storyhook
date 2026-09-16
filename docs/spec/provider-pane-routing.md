# Provider-pane routing ownership

SH-738 · reviewed against v3.0.1

Dispatch orchestration may hold a leased Storyhook binary and a pinned GitHub
operation. Starting a provider pane begins a different GitHub operation, rooted
in the pane's checkout. It does not transfer the parent's origin authority.

## Contract

| Selector | Owner | Terminal boundary |
|---|---|---|
| `STORY_BIN` | Current caller | Pass its exact value with a lifecycle `-e` override; absent and empty become empty, preserving `${STORY_BIN:-story}` fallback. |
| `STORYHOOK_GITHUB_AUTHORITY` | Parent GitHub operation | Empty in the pane; the shell adapter resolves the pane checkout. |
| `STORYHOOK_GITHUB_EXPECTED` | Parent GitHub operation | Empty in the pane; `github_begin` establishes the pane's own origin pin. |
| `GH_CONFIG_DIR`, `GH_TOKEN`, `GITHUB_TOKEN`, `GH_ENTERPRISE_TOKEN`, `GITHUB_ENTERPRISE_TOKEN` | Authenticated orchestration | Existing client removal and empty lifecycle overrides remain in force. |
| `STORY_WORKSPACE_LOCK_FD` and inherited descriptors | Dispatch guardian | Descriptor ownership ends when the terminal client returns; the detached server cannot retain it. |
| Store/state-home discovery and `PATH` | Existing dispatch/runtime contract | No new filtering or rewriting. `STORY_BIN` selects shell-adapter execution; it does not rewrite bare `story` commands or `PATH`. |

`tmux-launch.py` removes credentials and the three routing selectors from the
client environment before invoking tmux. This prevents a new server's global
environment from caching them. It then supplies explicit overrides to
`new-session`, `new-window`, or `respawn-pane`. A new session receives its own
environment; existing global/session environments are never rewritten.

This two-part boundary matters because tmux copies the startup environment into
the server, then merges global and session values for new windows. Filtering only
the current client leaves retained values effective. See the
[tmux environment contract](https://man.openbsd.org/tmux.1#GLOBAL_AND_SESSION_ENVIRONMENT).

The binary exception is deliberate: the daemon sets `STORY_BIN` to its own
executable before dispatch. Removing it as a release build/test scrub would
discard legitimate ownership. Passing it explicitly makes fresh and retained
servers agree. No public CLI or storage schema changes.

## Reproduction and regression evidence

The finalized private real-tmux matrix produces 75 failed assertions against a
temporary copy of the original launcher, over 15 lifecycle/binary combinations.
All combinations pass against the corrected launcher. A fresh pane retained foreign authority and
the real local GitHub resolver refused its own origin. An existing server
supplied `/stale/session/story`, so real project reads failed with exit 127 even
when the caller selected its current lease. No last-known-good release is claimed.

The routing matrix runs through `test-dispatch-workspace.sh` under its isolated
store and leased binary. It covers fresh servers, new sessions on retained
servers, new windows, respawn, and a doctor-style scratch checkout, each with
`STORY_BIN` absent, empty, or a spaced executable path that execs the actual
lease. The probe performs real `story show` and `github_begin` calls. The
doctor-style checkout copies the project pointer and has no origin: project
reads succeed and GitHub resolution explicitly refuses the missing origin.

Assertions also cover unchanged parent inputs, existing global/session state,
unrelated live panes, store/state-home values, additional `-e` arguments, chained
commands, and tmux failure status. The adjacent credential and real-flock tests
retain their original coverage. All native fixtures use private sockets and
bounded waits; cleanup kills only fixture-owned servers.

`dispatch_tmux_context` extends the real manual/HTTP dispatch boundary into the
production launcher. Actual panes must retain the selected binary while clearing
GitHub routing and credentials, for Claude and Codex in attended and autonomous
modes. Full helper dispatch, resume, and doctor readiness tests cover the callers.

The impacted selector reported `ALL` because the certified baseline has no
coverage map. Only new and directly impacted tests run in this worktree; the
central verifier owns the full suite. Exact run results are recorded on SH-738.
