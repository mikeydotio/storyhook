# SessionStart evidence and dispatch readiness

SH-736, investigated against v3.0.1.

## Failure and evidence

The CLI replaced all SessionStart invocation errors with a message that claimed
project state could not load in time. The hook discarded stderr, and sentinel
publication occurred only in the daemon handler. An immediate error before RPC
therefore left neither a sentinel nor its cause. A real hook/CLI regression uses
a directory at the daemon spawn-lock path to reproduce that failure. A held-lock
case separately covers deadline expiry. Neither experiment proves the reported
enterprise-Claude startup trigger or an MCP startup race.

The previous helper doctor checked a terminal marker and paste delivery. It did
not exercise the sentinel gate used by dispatch. A marker-only fixture reproduced
an incorrect all-green result.

## Evidence contract

- Protocol 2 remains unchanged. `context_status` is an optional diagnostic:
  `loaded`, `unavailable`, or unknown when absent in an older receipt.
- A sentinel proves hook execution at its cwd. It does not prove daemon health,
  story ownership, plan approval, or permission to implement.
- The resolved-project service publishes the actual context outcome. Failure to
  write evidence cannot discard successfully loaded context.
- After invocation failure, the CLI may publish `unavailable` without opening
  the store. This requires a valid nearest project pointer, an enabled plugin,
  a SessionStart payload with a nonblank session ID, canonical cwd agreement,
  and an existing absolute plugin directory.
- Invalid, unreadable, broken, unsupported, or disabled local configuration does
  not qualify. A bad nearer pointer cannot borrow a parent's identity. Read the
  plugin setting beside the pointer, including the supported legacy file.
- Preserve the underlying invocation error on stderr. Stdout remains hook JSON.
  An immediate I/O error must not be described as a timeout.
- Each publisher uses a unique sibling temporary file, syncs it, and atomically
  replaces the final sentinel. Concurrent writers cannot share a temporary path.

Plugin identity uses physical directory equality. Symlink aliases are accepted;
different, missing, broken, or relative directories are rejected. Session,
attempt, turn, transcript, pane, and process checks remain independent.

## Doctor contract

The helper doctor creates a private temporary Git checkout carrying the current
project pointer and legacy plugin configuration. It cannot consume or overwrite
the caller's sentinel. It uses dispatch's tmux launcher, captures the process
incarnation, and rechecks ownership before input and cleanup.

Claude uses the production sentinel gate. Codex uses its existing task-free
initialization, stopped-turn receipt, and transcript-completion gate. No story is
claimed, no task charter is sent, and no approval watcher is armed.

The report distinguishes terminal readiness, sentinel readiness, project-context
status, Plan mode, paste delivery, project integrity, and cleanup. Overall `ok`
requires every check. A marker, an unknown context status, or an incomplete
bootstrap cannot produce an all-green result. If cleanup cannot prove ownership
or terminate its pane, it reports and preserves the scratch path.

Installation doctor still checks installation consistency. It does not certify
provider startup or dispatch readiness.

## Validation

Rust regressions exercise the real hook and CLI, fast pre-RPC failure, held spawn
locks, malformed authority, disabled configuration, write failure, and concurrent
atomic publication. Shell regressions execute the real Claude/Codex hooks with
controlled provider events and tmux boundaries. They cover canonical and foreign
packages, missing/late sentinels, process death, incomplete initialization,
degraded context, and cleanup refusal. Live enterprise-Claude behavior remains
unverified; retain that limitation in the story's final diagnostic record.
