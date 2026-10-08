# Daemon resource limits by launch path (SH-800)

On 2026-10-08, the actual launchd daemon on the reporting Mac had a soft
`RLIMIT_NOFILE` of **256**, while the actual forked daemon had **1,048,575**.
Both hard limits were unlimited. The other eight distinct resources matched.
This demonstrates a launch-path difference in descriptor headroom on this
host; it does not demonstrate an observed descriptor-exhaustion incident.

## Measurements

Host: macOS/Darwin 25.6.0, arm64. Both modes used the same private store and
the same uninstalled production-feature binary: StoryHook 3.0.3 (110), build
`56fecaae8094`. The measurement build added temporary in-process `getrlimit`
instrumentation to source `2b68a57b9f5b246aede7ae1c307ca21ee4389335`.
Neither `fault-injection` nor `test-seam` was enabled.

| Resource | Fork soft | Fork hard | Launchd soft | Launchd hard |
| --- | ---: | ---: | ---: | ---: |
| NOFILE | 1,048,575 | unlimited | 256 | unlimited |
| NPROC | 10,666 | 16,000 | 10,666 | 16,000 |
| CPU | unlimited | unlimited | unlimited | unlimited |
| FSIZE | unlimited | unlimited | unlimited | unlimited |
| DATA | unlimited | unlimited | unlimited | unlimited |
| STACK | 8,372,224 | 67,092,480 | 8,372,224 | 67,092,480 |
| CORE | 0 | unlimited | 0 | unlimited |
| MEMLOCK | unlimited | unlimited | unlimited | unlimited |
| AS (RSS alias on macOS) | unlimited | unlimited | unlimited | unlimited |

Sizes are bytes, CPU is seconds, and NOFILE/NPROC are counts. AS and RSS
identify one resource on Darwin, so this is nine measurements, not ten.

## Method and controls

The collector called `getrlimit` in the serving daemon after owner resolution
and before portfile publication. It distinguished finite values, infinity,
and syscall errors. A helper process's limits were not used as daemon evidence.

The fork mode used the normal `daemon start` path with no service installed
for the private store; its reported reason was `NoAgentInstalled`. It was
authenticated with `daemon status` and then stopped before the second mode.
The second mode used a transient user GUI launchd job, with the same executable
bytes and store. Successful bootstrap, the manager's exact service PID,
native process incarnation and executable identity, parent PID 1, authenticated
status, and the daemon's own measurement agreed.

The temporary plist retained the normal `Interactive`, `RunAtLoad`, absent
`KeepAlive`, and absent resource-limit settings. Two isolation differences were
deliberate: an `/usr/bin/env -i` exec wrapper removed ambient manager environment
values without forking or changing limits, and a private working directory
prevented ambient project discovery. The daemon inherited a private HOME/XDG
environment, a nondefault store, loopback-only binding, and a native lifetime
contract tied to the experiment owner. No provider credentials were passed.

The runner verified identical executable hashes for both modes. It stopped
only the captured fixture incarnations, verified released lifetime locks and
closed listeners, and removed only the exact transient job. Final inspection
confirmed that job absent and the protected production/provider process
identities unchanged. Private runtime files were retained outside the source
tree and are not part of this report. No installed binary or system limit was
changed. The temporary collector and runner are not shipped.

## Consequence and follow-up

The daemon admits up to 128 HTTP connections across its listeners and keeps
a default pool of ten SQLite connections, alongside listeners, journals,
standard streams, and subprocess pipes. A 256-descriptor soft limit therefore
leaves materially less operating headroom than the fork baseline. The exact
descriptor demand under peak workload was not measured here, so these results
do not establish a universal minimum or justify copying the fork's million-file
ceiling.

The warranted follow-up is a separate daemon-local normalization change with
regression coverage, preserving higher inherited limits, hard ceilings, and
explicit operator controls. It must not raise a global limit or change launchd
manager policy. A second host was not available; no cross-host result is claimed.
