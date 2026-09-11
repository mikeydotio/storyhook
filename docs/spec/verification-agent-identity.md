# Verification agent identity — SH-677

## Failure and evidence

In v2.4.2, notification requires the window's `@storyhook-agent` option.
Live SH-656 and SH-677 Codex panes lacked this option despite running in their
story worktrees. Their direct startup commands differed from managed dispatch.
The original launch provenance is unproved. Managed dispatch also discards
provider-option write failures. Its fake terminal always records the option;
the native callback test supplies the option itself.

The approved council decision is recorded on SH-677. Do not infer process
identity from a window name, a generic process name, or a SessionStart sentinel.

## Contract

- Use one shared identity implementation for managed registration and notification.
- Bind a versioned pane-local record to project, story, Git common directory,
  canonical worktree, pane ID, PID, native process start token, and executable.
  Read the record back before accepting registration. Keep the window provider
  option for the existing census and dead-agent resume consumers.
- Register only the exact pane returned by dispatch, after provider readiness
  and before its story charter. Registration failure follows the existing
  pre-charter ownership-aware rollback and preserves diagnostics.
- Notification selects one unique matching pane, irrespective of active splits.
  A verification cleanup lease constrains its socket and worktree when present.
  Otherwise, untagged recovery requires an exact conventional story worktree
  registered by Git in the requested repository, plus a direct provider startup
  executable corroborated by the live executable. Custom worktree locations
  require managed registration or a lease.
- Existing records must match current evidence. Do not reinterpret stale,
  malformed, or conflicting records as permission to adopt a process again.
- Recheck pane, process incarnation, executable, worktree, and provider before
  registration and before delivery. Send one bracketed-paste message with the
  configured provider submit key. Do not retry an uncertain delivery.
- Missing, ambiguous, changed, or unavailable live evidence is `NotAbsent`.
  Such evidence permits neither delivery nor respawn. Preserve positively
  established dead-agent resume, including legacy window tags.

This is an operational identity boundary among processes owned by one user.
It does not authenticate against a process that can control that user's tmux
server. Unsupported wrappers and unavailable evidence produce explicit refusal.

## Validation

Use isolated native tmux servers and executable provider fixtures. Exercise
production registration and notification, including untagged recovery, wrong
projects/worktrees, duplicate candidates, split panes, stale metadata, process
replacement/PID reuse, and failed probes or metadata writes/readbacks. Keep
existing provider-key, dispatch rollback, and dead-agent resume coverage.
Run only new and directly impacted tests after the impacted-test selector.

## Process evidence

macOS uses `proc_pidinfo(PROC_PIDTBSDINFO)` and `proc_pidpath`; Linux uses
`/proc/<pid>/stat` start ticks and `/proc/<pid>/exe`. Precise native start tokens
follow the existing Rust lifecycle implementation and avoid the one-second
resolution of formatted `ps` timestamps.

Sources: [tmux manual](https://man.openbsd.org/tmux.1),
[Apple process structures](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/proc_info.h),
[Apple libproc](https://github.com/apple-oss-distributions/xnu/blob/main/libsyscall/wrappers/libproc/libproc.h),
[Linux process stat](https://man7.org/linux/man-pages/man5/proc_pid_stat.5.html),
[Linux executable link](https://man7.org/linux/man-pages/man5/proc_pid_exe.5.html).
