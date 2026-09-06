# SH-584: Web dispatch context and stale Codex capabilities

## Incident and evidence

On September 6, 2026, the operator reported that dashboard dispatch changed a
story to In Progress without visible work. The installed binary and restarted
daemon reported v2.4.2, build `fe688cdbe42e`. The previous installation was
v2.4.0; that is not a verified good release baseline.

Three persisted SH-583 Codex auto attempts at 20:39, 20:43 and 20:54 UTC
reported successful handoff and a worktree. Their cleanup receipts named
`/private/tmp/tmux-501/tmux-status-usage`; earlier successful attempts named
`default`. The named server no longer existed when inspected. This contradicts
the initial assumption that no resources had been created, but cannot prove
how much work a historical agent performed before its server disappeared.

SH-583's missing Astra selection was absorbed into SH-584. The checkout already
contained `gpt-6-astra`, while the enabled Codex helper omitted it. Read-only
`story doctor install` reported both provider installations at v2.4.0 against
the v2.4.2 binary. No production installation was changed during this fix.

The installed-path guard also rejected launcher execution. Its reproduction
and abandoned draft were posted on SH-585 after the operator transferred
ownership. No guard implementation belongs to this change.

## Competing explanations and reproduction

| Explanation | Experiment | Finding |
|---|---|---|
| The helper aborts before creating anything | Inspect persisted outcomes and independently exercise the production helper | History reports resources and handoff; a general pre-launch abort does not explain the recorded server identities. |
| The daemon's starter chooses the dispatch server | Start a real daemon with unrelated `TMUX` context and two private real tmux servers; dispatch through HTTP | Before correction, dispatch reports `ok` on `unrelated`, although the expected server is `default`. |
| The web UI omits Astra from a correct catalog | Read enabled helper capabilities | Astra is already absent before the response reaches the UI. |
| Provider install success guarantees fresh capabilities | Run the real installer with an external provider retaining stale bytes at the expected enabled version | Installer reports success with the stale helper still present. |
| Refreshing the helper immediately refreshes the dropdown | Replace a fixture helper between two real options requests | Before correction, the cache still returns the previous model. |

The tmux boundary test deliberately replaces the helper with a socket probe.
It isolates routing; it is not evidence of Git or provider behavior. Separate
helper and browser tests exercise claims, worktrees, prompt submission, and
the executed model argument.

## Causes and corrections

**Terminal identity crossed a daemon lifetime boundary.** Initial web dispatch
commit `eb61f4d1ad2d416f1f91e86cc6333cc821147b7d` inherited the initiating
process's whole environment. Commit `55f9c86281` later retained `TMUX` and
`TMUX_PANE` explicitly in the dispatch allowlist. These identify the terminal
that started the daemon, not the server for later web work. The failure needs
both an unrelated starter server and an operator observing the default server.
This is a latent configuration-dependent defect, not a verified v2.4.2 regression.

Daemon helpers now exclude those handles. Direct interactive helpers retain
their caller's context. Full Auto's direct tmux monitoring uses the same
boundary; cleanup operations retain explicit socket selection from their
leases. This also covers verifier helper spawning without changing claim
rollback rules or interpreting ambiguous submission as definite failure.

**Installation trusted a response instead of its resulting payload.**
`5f67c4942d` introduced the unchecked Codex installation result;
`2dd58d19f` added embedded payload refresh without closing that postcondition.
Installation now verifies the enabled version/path, complete regular-file set,
bytes, and executable permissions against the binary's embedded plugin before
reporting success or changing the stable launcher. Codex remains the cache
writer; Storyhook refuses mismatches rather than patching installed files.
The original stale machine may simply not have refreshed its plugins; the
fixture proves the separate false-success gap, not that an install was attempted
in the original incident.

**Capabilities were cached by provider alone.** The cache now also identifies
the resolved helper path and content digest, so replacement or removal cannot
continue advertising the previous helper's catalog within the TTL.

These are surgical interface/checking corrections. No new public endpoint,
store schema, model default, version, installation, or deployment is required.

## Verification boundary

Regression coverage includes real isolated tmux servers and a daemon, both
providers in plain/auto mode, direct interactive context, installation freshness
and failures, immediate catalog invalidation, and browser Astra selection through
the real dispatch helper to an executed argv-recording provider fixture.
The latter proves process arguments, not hosted model availability or inference.

Two existing engine shell unit tests initially exceeded their 3-second command
deadlines. Isolated and serial reruns passed; the timing cause is unverified.
The real two-server integration tests passed independently.

Only new and directly impacted tests run in the agent worktree. SH-584's linked
PR is handed to the centralized verifier for the full suite, merge, completion,
and worktree cleanup. SH-585 independently owns installed-path guard behavior.
