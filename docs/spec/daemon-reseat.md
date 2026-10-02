# Daemon replacement and failed installation (SH-820)

`make install` replaces the executable, then asks that executable to refresh
registered provider plugins. Binary replacement remains available without a
test gate or a readable store. A failed refresh returns failure and states that
the binary was installed; it must not certify a mixed installation as complete.
The release caller must not stop a working daemon before the build or refresh.

## Evidence and limits

The September 26, 2026 incident left an old daemon and old plugin after a failed
managed start. A later registration replacement restored only the previous plist
after bootstrap error 5, leaving no daemon. The retained report
`story-2026-09-26-124010.ips`, incident
`4F5F53FB-44F6-4B53-B341-1CE20CE188C1`, identifies launchd as parent and records
`CODESIGNING` code 4, `Launch Constraint Violation`.

That report does not identify the violated constraint. A changed executable
identity is a hypothesis: [Apple describes this class of upgrade failure](https://developer.apple.com/forums/thread/795022).
The fix does not change signing policy or disable a constraint. It preserves
fresh-file installation, as [Apple recommends](https://developer.apple.com/documentation/security/updating-mac-software),
and makes a failed registration recoverable and observable.

## Transaction

All macOS registration mutations serialize with client starts through the
per-store spawn lock. Before writing, read the prior plist and inspect the
registration and authenticated incumbent. An unreadable prior definition is an
error, not an absent registration. Plist replacement is atomic.

A managed incumbent drains before its job is removed. An unmanaged incumbent
remains available until launchd accepts the new registration, then drains before
the managed start. Bootstrap acceptance does not prove readiness: success needs
authenticated health for the expected build, store and launchd owner.

Removal waits for the exact service to become absent. Removal and bootstrap
share ten seconds; each control command is limited to the remaining budget.
Poll every 100 ms. Bootstrap error 5 can retry only inside that budget, after a
new absence observation. Other refusals retain their command, target, status and
diagnostic. Human-readable `launchctl print` output is diagnostic, not a parser
contract.

After a failed mutation, remove the partial registration before restoring the
previous definition. Reload and check a previously managed daemon, allowing the
previous build. Restore unmanaged mode only when it was the previous mode and
no installed registration remains authoritative. An incumbent that never stopped
does not need a new process: authenticate its recorded token again within the
remaining removal budget. A stale portfile cannot excuse a late replacement's
lifetime lock. A fresh installation that had no daemon returns to that initial
state.

If recovery fails, return both the install failure and the recovery failures.
State whether service health was actually verified. Restored bytes alone are
not a recovered service. Persistent OS denial, inaccessible files or a broken
old executable can defeat recovery; the command cannot guarantee availability
under those conditions.

## Startup recovery

Ordinary healthy starts remain idempotent. Never use `kickstart -k`.
After an accepted start fails health, collect the exact job's launchctl
diagnostic. One bounded registration replacement is allowed only if the store
has no live daemon, no application startup-failure record, and a valid matching
plist. Check again before unloading to protect a late startup. Clear stale
failure evidence before the initial launch; preserve records published during
the attempt or its recovery. During an unmanaged handoff, clear the losing
RunAtLoad child's record after the old daemon drains. A second failure keeps
the first failure and the job diagnostic.

Schema errors, malformed registration, application failures and a daemon still
holding the lifetime lock do not trigger replacement. Managed health requires
the expected owner, so an old fork cannot make a managed start look successful.

Tests inject service responses and time, not transaction behavior. All files
and daemon fixtures stay in isolated homes/stores. No automated test registers
a real launchd agent in the developer's session. The centralized verifier owns
the full suite and integration.
