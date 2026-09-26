# Daemon ownership: who starts the daemon, and at what class

Design of record for **SH-784**. Approved by Mikey 2026-09-25; this document
records the approved design plus the operational decisions implementation
added, including one amended by council vote (verdict on `story show SH-784`).

## The problem

The storyhook daemon had no scheduling class, resource coalition, or launchd
ownership of its own. It was spawned by whichever client happened to need it
first (`spawn_child`), so its CPU/IO priority, its jetsam/resource coalition,
and its umask were all accidents of that client. The installed launchd agent
— the thing meant to own it — never actually ran the production daemon: it
lost the pidfile-claim race against an on-demand fork and then stayed dead
(no `KeepAlive`). Verifier gates are children of the daemon, so the
accidental class reached every gate run too, compounding the load-sensitive
failures SH-766/SH-767/SH-785 were already tracking.

## Confirmed root cause of `last exit code = 2`

The original story evidence only *hypothesized* the cause. Confirmed
directly from the reporting machine's own runtime state during planning:

- `daemons/<key>/daemon.failure.json` (mtime after the currently-live
  daemon claimed the pidfile) read exactly
  `{"kind":"usage","detail":"a storyhook daemon is already running. Run
  \`story daemon stop\` first."}` — `AppError::Usage` from `claim_pidfile`
  (`lifecycle.rs`), recorded by `record_startup_failure` on the losing side
  of the race.
- The live daemon shared its resource/jetsam coalition with an unrelated
  client process (`proc_pidinfo(PROC_PIDCOALITIONINFO)`), confirming that
  client — not launchd's `RunAtLoad` — won the race at login.

## As built

### launchd is the sole daemon owner on macOS when installed

A client that finds no usable daemon asks launchd to start it
(`launchctl kickstart`, never `-k`) rather than forking. Plain `kickstart`
starts the job only if it is not already running — a safe no-op otherwise —
which is what makes this race-free: there is only ever one mechanism
creating `daemon --serve` processes for an installed agent, so the pidfile
race that produced `exit code 2` cannot recur. If the job is not yet loaded,
`kickstart` reports `LAUNCHCTL_SERVICE_NOT_FOUND` (113); the client
`bootstrap`s it once and retries. If launchd refuses outright, the command
fails loudly and never falls back to a fork.

Fork remains the path for:
- test builds (`is_test_build()`), unconditionally, regardless of OS or of
  whatever happens to be installed on the development machine;
- any OS other than macOS (Linux has no service-manager integration yet —
  SH-787 tracks giving it one; `story daemon install` already refuses there,
  so "no agent installed" is simply always true);
- a store with no launchd agent installed, or whose label's plist serves a
  *different* store (`agent::Health::NotInstalled` /
  `Health::ServesAnotherStore`) — kickstarting either would be wrong.

The fork path prints a warning (TTY-only) naming `story daemon install` when
the reason is "no agent installed"; it stays silent for a test build.

### Ownership is self-reported, not inferred

`choose_launcher(env)` is a pure function of this build and this store's
login-agent health — **never** of the calling process's own scheduling class
or coalition, so the daemon's ownership cannot itself become an accident of
whichever client happened to trigger a start.

The resulting daemon self-reports which mechanism started it via a hidden
`--owner <launchd|fork-test-build|fork-no-agent>` flag on `daemon --serve`
(absent from `--help`, the same convention `--serve` itself follows).
`spawn_child` always passes it; the installed plist's fixed
`ProgramArguments` always carries `--owner launchd`. When the flag is
**absent** — a human ran `story daemon --serve` by hand — the daemon
self-detects `Forked { parent_pid: getppid(), reason: Manual }`. This is
published in the portfile as `DaemonInfo.owner: Option<DaemonOwner>`
(`None` for a pre-SH-784 portfile) and rendered by `story daemon status`.

### The plist: `ProcessType = Interactive`

`launchd.plist(5)` permits `Interactive` "only if an app's ability to be
responsive depends on it, and cannot be made Adaptive." True since SH-114
made every `story` command depend on the daemon; `Adaptive` needs XPC, which
the daemon does not use. Measured effective priority (Mach
`THREAD_EXTENDED_INFO.pth_priority`) on the reporting machine:

| How the process started | Base | After requesting `QOS_CLASS_USER_INITIATED` |
|---|---|---|
| Unclamped terminal CLI (today's on-demand fork) | 31 | 31 (no effect) |
| launchd `ProcessType = Background` (the old plist) | 4 | 4 |
| launchd `ProcessType = Standard` | 20 | 20 |
| launchd `ProcessType = Interactive` | 31 | **37** |

Only `Interactive` lets a thread's elevated request actually take effect.
This is also why requesting the class is always safe to do unconditionally:
under any other launch path it is accepted but has no effect, never a
regression.

### Thread-level class: `WorkClass`

`daemon::qos::WorkClass::{Serving, Housekeeping}` — every thread the daemon
spawns calls one explicitly at its own top, rather than relying on whatever
it happened to inherit:

- **Serving** (`QOS_CLASS_USER_INITIATED`): the per-connection thread
  (`http1::serve_one_connection`, one per accepted connection, every
  listener), the fixed dispatcher pool (`serve::dispatch`), and the
  nested-invoke lane's per-job thread.
- **Housekeeping** (`QOS_CLASS_DEFAULT`, explicit rather than left
  unstated): every background poller spawned from `serve`'s own
  `thread::scope` (heartbeat, change-token poll, parent watch, block
  delivery, continuation, GitHub poll, verifier orchestration and its
  progress publisher, the Full Auto engine poll, cleanup, tailnet reprobe,
  the crash-pending sweep, and the shutdown drain loop), plus two threads
  spawned *from* an already-`Serving` thread that do longer background work
  rather than answering the request itself (`api::dispatch::spawn_dispatch`,
  `api::reset`'s reset worker) — both would otherwise inherit the elevated
  class they do not need.

Gates are unchanged here: SH-785 (blocked by SH-766/SH-767) lowers verifier
gate spawns to utility QoS separately, so gates never inherit a raised
serving class. A thread's QoS does not pass to processes it spawns in any
case (measured: children of an elevated thread start at the machine's
ordinary default) — the same fact that keeps a dispatched Full Auto agent
session's own process from inheriting anything from the thread that spawned
it.

### Restart's port: a per-store hint, not a bare documented limitation

**Amended by council vote** (3-0, unanimous; verdict recorded on `story show
SH-784`). The original plan recommendation — accept that a launchd-owned
restart cannot preserve a non-default port, document it, build nothing new
— was factually incomplete:
`default_daemon_port_for_store` returns `preferred_port = 0`
**unconditionally** for every non-default store, so for a launchd-owned
*named* store the gap is not a rare corner case, it is the deterministic
outcome of every restart. `commands::restart`'s own doc comment ("preserving
its loopback port") and `tests/daemon_lifecycle.rs::
restarting_replaces_the_daemon_on_the_same_port` already promised the exact
guarantee "just document it" would have silently broken.

Adopted instead: a per-store sidecar file, `daemon.port-hint`
(`Environment::daemon_port_hint`, JSON `{"port": u16}`), written best-effort
by `bind_preferred` on every successful bind, holding the port actually
bound. `bind_preferred`'s attempt order: the environment's preferred port
first when nonzero (unchanged for the default store, and for a fork that
explicitly passed `--port`, since restart already sets this) → the sidecar
hint if present and nonzero (the new step — what actually helps a named
store, or a default-store fallback) → an OS-assigned port, exactly as
before. No plist rewrite, no re-`bootstrap`, no parallel fork path; fully
unit-testable with no live `launchctl`, unlike either rejected alternative.

The existing pinned test keeps passing unchanged: `is_test_build()` always
selects `ForkLauncher`, so it only ever exercises the fork-exact path, which
is untouched by this change.

### The rest of the class (acceptance #5)

- **umask**: fixed. `lifecycle::run` resets it to `0o022` unconditionally,
  early, before anything else — a real gap, not a hypothetical one: the
  couple of files that rely on `OpenOptionsExt::mode` alone (rather than an
  explicit `set_permissions` afterward, which ignores umask entirely) would
  otherwise have their requested mode ANDed with whatever the launching
  client's shell happened to have set.
- **Seatbelt sandbox**: resolved by construction for the launchd path —
  launchd-spawned processes never inherit a client's sandbox. Remains a
  known, already-true, and now explicitly documented gap for the fork
  fallback (no agent installed, or a test build): a sandboxed client's
  sandbox still reaches a forked daemon exactly as it always has. No new
  code; this is the trade-off of the fallback path being a fallback.
- **Resource limits (`ulimit`)**: no measured defect (the story's own
  evidence lists it "not checked"). Filed as a follow-up story rather than
  guessed at, per this project's own discipline against fixing an
  unmeasured problem.

## Types

```
trait DaemonLauncher { fn launch(&self, env) -> Result<DaemonOwner, AppError> }
  ├─ ForkLauncher { reason: ForkReason }         (lifecycle.rs)
  └─ LaunchdLauncher                             (launchd.rs)

enum DaemonOwner (pub, lifecycle.rs)
  ├─ Launchd { label: String }
  └─ Forked  { parent_pid: u32, reason: ForkReason }

enum ForkReason (pub)
  ├─ TestBuild
  ├─ NoAgentInstalled
  └─ Manual

enum WorkClass (pub(crate), qos.rs)
  ├─ Serving       → QOS_CLASS_USER_INITIATED
  └─ Housekeeping  → QOS_CLASS_DEFAULT

DaemonInfo (existing) gains: owner: Option<DaemonOwner>
```

`launch()` returns just `DaemonOwner`, not the full `DaemonInfo`: the owner
fact is self-reported by the daemon (the `--owner` flag) into its own
portfile, not computed by the client. `launch_daemon` (the actual call site
`spawn_locked`/`restart` use) re-derives `DaemonInfo` from the portfile once
`launch()` confirms health.

## Verification

**Automated**, no live `launchctl` (matching every existing test in this
codebase, which deliberately never registers a real agent): `choose_launcher`'s
decision table (pure, synthetic `Health` inputs); `WorkClass::enter`'s
requested-class assertions; `launchd::ensure_running`'s kickstart/bootstrap
retry logic against an injected fake `launchctl`; `bind_preferred`'s
port-hint fallback order; the plist's `Interactive`/`--owner launchd`
content; `DaemonOwner` round-tripping through the portfile and `story daemon
status`.

**Manual**, on a real machine, after landing:
1. `story daemon install` (picks up the `Interactive` plist).
2. `story daemon stop`; `taskpolicy -c utility story list` (exercises the
   new client path from a clamped caller).
3. `sudo taskinfo <pid>` — expect an unclamped QoS and a coalition not
   matching the calling shell's own.
4. Reboot, log in, confirm `launchctl print gui/<uid>/<label>` shows
   `state = running` with no `last exit code` regression.

## Out of scope

- Linux ownership (SH-787): no service-manager integration; fork remains the
  only path, tagged `ForkReason::NoAgentInstalled`.
- Verifier gate QoS (SH-785): lowering gate spawns to utility, so they never
  inherit a raised serving class.
- Daemon `RLIMIT_*` under fork vs. launchd (follow-up story, unmeasured).
