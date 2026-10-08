# macOS daemon descriptor limits

The macOS daemon attempts to raise its own soft `RLIMIT_NOFILE` to 1,024 during
startup, after acquiring its lifetime lock and before starting activity workers.
It never lowers an existing soft limit, changes the hard limit, or changes any
other resource limit. A finite hard limit below 1,024 caps the requested raise.
An unlimited hard limit does not cause an unlimited soft-limit request.

This is conservative headroom for the default 128 HTTP connection slots,
ten pooled SQLite connections, listeners, journals, and subprocess pipes.
It is not a measured universal capacity requirement. Operators increasing
connection or workload caps still need to size their own limits. The fixed
target is below the macOS SDK's documented `OPEN_MAX` of 10,240; `setrlimit(2)`
rejects an infinite NOFILE soft-limit request. A kernel refusal leaves startup
running with a warning; there is no global-limit or launchd-policy fallback.
Successful changes are read back before the daemon reports the resulting limit.
A readback failure or mismatch is reported as unverified, not successful.

## Operator policy

`STORYHOOK_DAEMON_NOFILE` controls normalization in the actual daemon environment:

| Value | Behavior |
| --- | --- |
| Unset or `auto` | Apply the conservative startup policy when permitted below. |
| `inherit` | Keep the inherited soft and hard limits unchanged. |
| Any other value, including invalid UTF-8 | Warn and keep inherited limits unchanged. |

The variable must reach the serving process. Setting it only on a client does
not make launchd inherit that client's environment. A custom service can carry
`inherit` in its `EnvironmentVariables`; a manually launched daemon can inherit
it directly from its launcher. No installation or manager-wide environment
change is performed automatically.

For launchd-owned daemons, automatic normalization is allowed only when the
current on-disk plist is exactly the managed definition for this executable,
store, label, and log destination. The stored PATH is read from the plist and
round-tripped through the canonical writer; the current shell's PATH is not
substituted. An explicit `SoftResourceLimits` or `HardResourceLimits` dictionary,
including `NumberOfFiles`, preserves inherited limits. So do missing, unreadable,
binary, symlinked, oversized, reformatted, foreign, duplicate-key, and other
custom definitions. These conservative refusals produce a warning when a raise
would otherwise be attempted. Plist reads are nonblocking, so a FIFO cannot
stall daemon startup.

The current file does not establish the configuration of an older job still
loaded by launchd. Likewise, `getrlimit` cannot distinguish a default inherited
soft limit from an intentional shell limit. Use explicit `inherit` when that
provenance matters. In particular, replacing a custom loaded service's file
with the managed definition does not make its previously inherited policy
discoverable. The hard ceiling remains unchanged in every case.

Only the daemon process is changed; future children inherit its effective
limit through normal OS rules. Existing providers, client shells, launchd,
global settings, and Linux daemons are unaffected.

## Evidence and regression boundary

[The paired measurement](../rca/sh-800-daemon-resource-limits.md) found soft
NOFILE limits of 1,048,575 (fork) and 256 (launchd) on one Mac, both with unlimited
hard limits. Its temporary credential-free launchd fixture used a custom
`env -i` wrapper and working directory. That custom definition intentionally
retains its inherited limit under this policy; it is not evidence of automatic
normalization after this change.

Focused regression probes run only in bounded child processes. They begin at
a soft limit of 256, exercise the same initialization routine against canonical
and custom service fixtures, and hold 300 simultaneous file descriptors after
an allowed raise. They also cover an explicit opt-out, a higher existing soft
limit, a finite hard cap, a FIFO definition, syscall failures, readback mismatch,
and unchanged parent/other-resource limits. They do not bootstrap a real service
or alter the test runner's resource policy.
