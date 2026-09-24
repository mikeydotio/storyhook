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
| Store/state-home discovery (`STORYHOOK_STORE_PATH`, `XDG_STATE_HOME`, `STORYHOOK_VERIFIER_MIRROR`) | Current caller | Passed with a lifecycle `-e` override, the `STORY_BIN` pattern (SH-758). `new-session -e` scopes it to the storyhook-owned session; a server never retains it. |
| Server-start environment | Machine baseline, not the starting process | A client that may start a server passes only `tmux_server_env.SERVER_MAY_SEE` (spawn_env `COMMON_MAY_SEE` plus `TMUX_TMPDIR`/`TMUX`/`TMUX_PANE`) with `PATH` filtered of host roots (SH-758). |
| Host session state (`PLUGIN_ROOT`, `CLAUDE_PLUGIN_ROOT`, `CODEX_*`, Claude child-session names, per-call storyhook context) | The host process that exported it | Never reaches a server storyhook starts. A retained server keeps it; storyhook truly unsets it (`-r`) only in the storyhook-owned target session, and doctor reports the rest. |

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

## Server-start environment (SH-758)

A server copies the environment of the client that starts it, so whichever
storyhook process first needed a server decided what every later pane on the
machine inherited, the user's own terminals included. The daemon's
verification-view reconciler started the default server with the daemon's
whole environment. A daemon auto-started from a Codex SessionStart hook
therefore gave every Claude pane the Codex plugin root, and hook commands
written `${PLUGIN_ROOT:-$CLAUDE_PLUGIN_ROOT}` ran the Codex copy: another
plugin's safety hook exited 127 on every Bash call, and Claude dispatch failed
`hook-identity-mismatch`.

A council (architect, security, CLI UX; recorded on SH-758) chose an
allowlist-first boundary. A denylist had already missed names during the
investigation: the live server held `CLAUDE_CODE_MESSAGING_TOKEN`, shell
session state, and storyhook's own `STORYHOOK_ACTIVITY_*`, which labelled any
pane's `activity_run` output as verifier output.

| Part | As built |
|---|---|
| Policy | `plugins/story/lib/tmux_server_env.py`, the only statement. A Rust unit test in `src/env/spawn_env.rs` pins its baseline, credential and selector names to `COMMON_MAY_SEE`, `GITHUB_CREDENTIAL_MAY_SEE` and `Environment::child_vars`. |
| Dispatch launcher | `tmux-launch.py` probes with `list-sessions` before `new-session`. Only tmux's absent-server diagnostics select the allowlist; any other probe failure is surfaced. An existing server ignores its client's environment for new panes, so other calls keep the caller's environment (fake-tmux harness knobs included). |
| View launcher | The daemon runs `tmux_server_env.py` followed by `verification-view.py` as one program (`window.rs` `VIEW_PROGRAM`); every view client passes the allowlist. |
| `PATH` | The only denylist: entries under `$CODEX_HOME` (default `~/.codex`), `~/.cache/codex-runtimes`, `${CLAUDE_CONFIG_DIR:-~/.claude}/plugins`, `${XDG_DATA_HOME:-~/.local/share}/storyhook/plugins`, and the caller's plugin-root values, matched by path component. App bundles and macOS cryptex `codex.system` entries stay. |
| Retained servers | Engine, dashboard and verifier dispatches run `tmux-env.py scrub-session <target>` before any lane starts. The owned session gets `-r` for each retained name and a filtered `PATH`; a failed scrub rolls the dispatch back. Global and user sessions are never rewritten; `story.sh doctor` reports their retained names. |
| Hook roots | `hooks.json` resolves `${CLAUDE_PLUGIN_ROOT:-$PLUGIN_ROOT}`. Claude sets only `CLAUDE_PLUGIN_ROOT`, Codex sets both, so a Claude hook can no longer be redirected by an inherited `PLUGIN_ROOT`. |
| Sweep | `tests/tmux_server_start_sites.rs` pins which production files may issue `new-session`/`start-server` and requires each to apply the policy. |

Accepted cost: configuration that exists only in a launching process or an
interactive-only rc file (for example a `.zshrc`-only proxy or `CODEX_HOME`)
does not reach `$SHELL -c` provider panes on a server storyhook started. Put
such configuration in `~/.zshenv`, which every zsh reads. `SSH_AUTH_SOCK`
returns through tmux's `update-environment` when a terminal attaches, but not
in a session storyhook created; engine lanes already excluded it.

Evidence: `plugins/story/tests/test_tmux_server_env.py` starts private real
servers through both production launchers from a poisoned environment. Before
the fix the view path retained every host and per-call name the incident
listed; after it, `show-environment -g` holds only the allowlist and the
tmux-recorded `PWD`. The routing matrix below still passes with store
selectors carried by `-e`.

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
