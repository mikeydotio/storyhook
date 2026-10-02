# Workspace trust during autonomous startup

SH-859 changes the v3.0.3 dispatch startup protocol. A provider can wait for
workspace consent before it exposes the evidence that dispatch needs. Claude
can withhold its SessionStart hook. Codex cannot accept its initialization turn
until the consent dialog clears. Waiting for that evidence alone cannot advance
an untrusted workspace.

## Authority and scope

Only managed `--auto` and `--auto --full-auto` launches enable consent handling.
A fresh launch through resume uses the same path. Attended dispatch, doctor,
notification and already-running sessions do not acquire this authority.
Inherited environment variables cannot enable it.

Dispatch captures the pane, provider, process start identity and worktree.
Before each key, the handler checks that identity, the provider process and the
canonical pane directory. It repeats ownership checks after the final screen
capture. An expert readiness-pattern override cannot widen this consent check.

A displayed directory must resolve to the dispatched worktree. A Codex dialog
which explicitly discloses a repository-wide trust target must name the primary
worktree registered by Git. NUL-delimited Git metadata preserves exact paths.
A symlink alias may resolve to the same directory; a parent, prefix, unrelated
path or truncated path does not establish authority.

The provider may persist the accepted trust decision. Claude documents trust
at repository-root scope, including linked worktrees. StoryHook does not write
provider trust files or change tool permission settings to implement this step.

## Supported dialogs

| Provider evidence | Required screen elements |
| --- | --- |
| Codex 0.159.3 source | `Folder access`, exact directory, `Trust this folder?`, `Trust and continue`, `Quit`, one selected row |
| Claude Code 2.1.287 installed renderer | `Accessing workspace:`, exact directory, the project safety question, `Yes, I trust this folder`, one supported negative choice, one selected row |

Claude's supported negative choices are `No, exit` and
`No, continue without these permissions`. Option numbers may precede labels.
The current Claude picker starts on the negative choice; the implementation
does not assume either provider's default is affirmative.

The parser strips SGR styling and supported frame decoration. Internal path
spaces and Unicode remain significant. `tmux capture-pane -J` joins terminal
soft wraps. Physical line breaks and truncated paths fail closed; the handler
does not guess how to concatenate path fragments. A narrow terminal can thus
require a wider pane before retrying dispatch.

Login, update, hook-settings trust and tool-approval dialogs receive no automatic
consent. These are version-evidenced signatures, not a general onboarding API.
Provider UI changes may require a new fixture and parser update.

## State and effects

The pure classifier returns an observed selection and a fingerprint independent
of cursor focus. The shell handler owns effects and dispatch-local state:

1. Observe the same complete dialog for at least one second. Use the shared OS
   monotonic clock; macOS Python 3.9's `time.monotonic()` origins differ across
   short-lived processes.
2. If needed, send one arrow based on the observed row order. Reobserve the
   affirmative selection; an ignored arrow never permits confirmation.
3. Revalidate the dialog, selection and launch ownership. Send Enter once.
4. Wait for the dialog to disappear. Continue the original readiness checks.

All polls consume the existing readiness attempt budget. A changed dialog,
uncertain owner, failed input or unconfirmed selection refuses dispatch. A
persistent dialog after Enter times out without another confirmation.

Trust handling is disabled before Codex Plan-mode input and its initialization
turn, and after Claude's first readiness gate. Trust completion does not replace
Claude's exact plugin-root sentinel, Codex's bootstrap receipt and completion,
Plan mode, the guarded charter sender, or cleanup ownership checks.

The existing `pane-not-ready` JSON envelope carries `wait_ready_reason`,
`trust_phase` and the captured pane tail. Unsafe consent failures have `trust-*`
reasons. A later ordinary readiness failure retains its own reason and the
completed consent phase. Existing rollback controls resource preservation.

## Regression evidence

| Test | Boundary exercised |
| --- | --- |
| `test_startup_trust.py` via `test-startup-trust.sh` | Complete dialogs, exact paths and aliases, malformed/foreign choices, navigation order, cross-process clock |
| `test-startup-trust-readiness.sh` | Production readiness with real process identity, protected input, delayed consent, ownership and screen changes, one-shot confirmation, ordinary readiness after consent |
| `test-dispatch-workspace-trust.sh` | Real dispatch for both providers: Auto, Full Auto, fresh resume, attended refusal and missing-hook refusal |
| `notify_reasons` | The new key sender remains in the explicit sender inventory with its guard rationale |

The baseline replay keeps production dispatch unchanged and places the trust
screen at the external terminal boundary. Both providers fail without consent
and send no charter. The fixed tests permit ordinary readiness only after the
correct interaction. No current live-provider end-to-end compatibility run is
claimed. The central verifier owns the full suite.

## Sources

- [Codex 0.159.3 trust renderer](https://github.com/openai/codex/blob/rust-v0.159.3/codex-rs/tui/src/onboarding/trust_directory.rs)
- [Claude workspace trust](https://code.claude.com/docs/en/permissions#project-allow-rules-and-workspace-trust)
- [Python clocks](https://docs.python.org/3/library/time.html#time.monotonic)
- [Git worktree porcelain](https://git-scm.com/docs/git-worktree#_porcelain_format)
